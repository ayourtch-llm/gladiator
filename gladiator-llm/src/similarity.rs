//! Loop detection for streaming LLM output.
//!
//! When a model gets stuck repeating the same architectural reasoning or
//! dead-end snippet, the stream stays "active" (tokens arrive continuously)
//! so the idle timeout never fires. This module catches that failure mode
//! two ways:
//!
//! 1. **Cycle detection**: compare the last `cycle_window` chars against the
//!    `cycle_window` chars preceding them, using keyword Jaccard similarity
//!    (paraphrase-resistant — same concepts, different wording still scores
//!    high). After `cycle_streak_limit` consecutive similar windows, declare
//!    a loop. Inactive until `min_total_chars` have been fed (avoids false
//!    positives during legitimate long technical reasoning).
//!
//! 2. **Hard cap**: break unconditionally after `max_total_chars` regardless
//!    of similarity. A simple backstop for cases the similarity check misses.
//!
//! Both conditions report a loop; the caller breaks the stream and the
//! existing triage path (llm_call → guidance injection) handles recovery.

use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// Similarity metrics
// ---------------------------------------------------------------------------

/// Sørensen-Dice coefficient over the multiset of character bigrams of `a`
/// and `b`. Returns a value in `[0.0, 1.0]`. Good for verbatim/near-verbatim
/// comparison. For paraphrase-resistant comparison use `keyword_similarity`.
pub fn similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let ca = count_bigrams(a);
    let cb = count_bigrams(b);
    let total_a: usize = ca.values().sum();
    let total_b: usize = cb.values().sum();
    if total_a == 0 && total_b == 0 {
        return if a == b { 1.0 } else { 0.0 };
    }
    let mut intersection = 0usize;
    for (k, &va) in &ca {
        if let Some(&vb) = cb.get(k) {
            intersection += va.min(vb);
        }
    }
    let total = (total_a + total_b) as f64;
    if total == 0.0 {
        return 1.0;
    }
    2.0 * intersection as f64 / total
}

fn count_bigrams(s: &str) -> HashMap<(char, char), usize> {
    let mut counts = HashMap::new();
    let chars: Vec<char> = s.chars().collect();
    for w in chars.windows(2) {
        *counts.entry((w[0], w[1])).or_insert(0) += 1;
    }
    counts
}

/// Jaccard similarity over the SET of significant words (> 3 chars, lowercased,
/// alphanumeric-stripped). Catches paraphrased repetition: when the model
/// re-discusses the same concepts (function names, API terms) with different
/// connective wording, the keyword sets still overlap heavily.
pub fn keyword_similarity(a: &str, b: &str) -> f64 {
    let wa = significant_words(a);
    let wb = significant_words(b);
    if wa.is_empty() && wb.is_empty() {
        return 1.0;
    }
    if wa.is_empty() || wb.is_empty() {
        return 0.0;
    }
    let inter = wa.intersection(&wb).count() as f64;
    let union = wa.union(&wb).count() as f64;
    if union == 0.0 {
        return 1.0;
    }
    inter / union
}

fn significant_words(s: &str) -> HashSet<String> {
    s.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| w.chars().count() > 3)
        .collect()
}

// ---------------------------------------------------------------------------
// LoopDetector
// ---------------------------------------------------------------------------

/// Reason the detector fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopReason {
    /// Consecutive windows exceeded the similarity threshold.
    CycleDetected,
    /// Total streamed chars exceeded the hard cap.
    HardCapHit,
}

/// Detects think-loops by comparing consecutive windows of streamed text and
/// enforces a hard cap on total output length.
///
/// The detector buffers recent output and, once `min_total_chars` have been
/// fed, compares the last `cycle_window` chars against the `cycle_window`
/// chars preceding them using `keyword_similarity` (paraphrase-resistant).
/// After `cycle_streak_limit` consecutive similar comparisons, it fires with
/// [`LoopReason::CycleDetected`]. Independently, once `max_total_chars` have
/// been fed, it fires with [`LoopReason::HardCapHit`].
#[derive(Debug, Clone)]
pub struct LoopDetector {
    pub buffer: String,
    pub cycle_window: usize,
    pub cycle_streak: usize,
    pub cycle_streak_limit: usize,
    pub cycle_threshold: f64,
    pub min_total_chars: usize,
    pub max_total_chars: usize,
    pub last_check_fed: usize,
    /// Total chars fed so far.
    pub fed_chars: usize,
    /// Last reason the detector fired (None if it hasn't).
    pub last_reason: Option<LoopReason>,
}

impl LoopDetector {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            cycle_window: 5_000,
            cycle_streak: 0,
            cycle_streak_limit: 2,
            cycle_threshold: 0.55,
            min_total_chars: 10_000,
            max_total_chars: 20_000,
            last_check_fed: 0,
            fed_chars: 0,
            last_reason: None,
        }
    }

    /// Feed a chunk of streamed text (or reasoning). Returns `Some(reason)`
    /// when a loop is detected — the caller should break the stream.
    pub fn push(&mut self, text: &str) -> Option<LoopReason> {
        if text.is_empty() {
            return self.last_fired();
        }
        self.fed_chars += text.chars().count();
        self.buffer.push_str(text);

        // Hard cap — unconditional backstop.
        if self.fed_chars >= self.max_total_chars {
            self.last_reason = Some(LoopReason::HardCapHit);
            tracing::info!(
                "[loop-detector] hard cap hit ({} chars fed, max={})",
                self.fed_chars,
                self.max_total_chars
            );
            return self.last_fired();
        }

        // Cycle detection — only once enough text has accumulated, and only
        // re-check when enough new text has arrived to shift the window
        // meaningfully.
        if self.fed_chars < self.min_total_chars {
            return self.last_fired();
        }
        if self.fed_chars < self.last_check_fed + (self.cycle_window / 4) {
            return self.last_fired();
        }
        self.last_check_fed = self.fed_chars;

        let chars: Vec<char> = self.buffer.chars().collect();
        let n = chars.len();
        if n < 2 * self.cycle_window {
            return self.last_fired();
        }

        let recent: String = chars[n - self.cycle_window..].iter().collect();
        let earlier: String = chars[n - 2 * self.cycle_window..n - self.cycle_window]
            .iter()
            .collect();
        let sim = keyword_similarity(&earlier, &recent);

        if sim >= self.cycle_threshold {
            self.cycle_streak += 1;
            tracing::debug!(
                "[loop-detector] similar cycle window (kw-ratio={:.3}, streak={}/{}, {} chars fed)",
                sim,
                self.cycle_streak,
                self.cycle_streak_limit,
                self.fed_chars
            );
            if self.cycle_streak >= self.cycle_streak_limit {
                self.last_reason = Some(LoopReason::CycleDetected);
                tracing::info!(
                    "[loop-detector] cycle detected: {} consecutive similar windows \
                     (last kw-ratio={:.3}, {} chars fed)",
                    self.cycle_streak,
                    sim,
                    self.fed_chars
                );
                return self.last_fired();
            }
        } else {
            if self.cycle_streak > 0 {
                tracing::debug!(
                    "[loop-detector] resetting streak ({}) on dissimilar window (kw-ratio={:.3})",
                    self.cycle_streak,
                    sim
                );
            }
            self.cycle_streak = 0;
        }

        // Bound memory: keep roughly 2*cycle_window chars.
        if n > 3 * self.cycle_window {
            let trim = n - 2 * self.cycle_window;
            self.buffer = self.buffer.chars().skip(trim).collect();
        }

        self.last_fired()
    }

    fn last_fired(&self) -> Option<LoopReason> {
        self.last_reason
    }
}

impl Default for LoopDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_strings_are_perfectly_similar() {
        assert!((similarity("hello world", "hello world") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn disjoint_strings_are_dissimilar() {
        assert!(similarity("abc", "xyz") < 0.1);
    }

    #[test]
    fn near_duplicates_score_high() {
        let a = "Let me reconsider the architecture and think about whether we should do X";
        let b = "OK let me reconsider the architecture and think about whether to do X";
        assert!(similarity(a, b) > 0.85);
    }

    #[test]
    fn empty_inputs_handled() {
        assert_eq!(similarity("", ""), 1.0);
        assert_eq!(similarity("a", ""), 0.0);
        assert_eq!(similarity("", "a"), 0.0);
    }

    #[test]
    fn keyword_similarity_catches_paraphrase() {
        let a = "Should idle detection live in LlmActor or AgentActor?";
        let b = "Whether to put idle detection in the LlmActor vs the AgentActor";
        assert!(keyword_similarity(a, b) > 0.5);
    }

    /// Build a detector with small thresholds so tests don't need huge input.
    fn small_detector() -> LoopDetector {
        let mut d = LoopDetector::new();
        d.cycle_window = 200;
        d.min_total_chars = 300;
        d.max_total_chars = 2000;
        d.cycle_streak_limit = 2;
        d.cycle_threshold = 0.50;
        d
    }

    #[test]
    fn detector_does_not_fire_under_min_floor() {
        let mut d = small_detector(); // floor = 300
        let para = "Let me reconsider whether to do this in LlmActor or AgentActor. ";
        // Feed ~250 chars — under the 300 floor.
        for _ in 0..4 {
            assert!(d.push(para).is_none(), "should not fire under floor");
        }
        assert!(d.fed_chars < 300);
        assert!(d.last_reason.is_none());
    }

    #[test]
    fn detector_fires_on_repeated_cycles() {
        let mut d = small_detector(); // window=200, floor=300, streak_limit=2
        // A paragraph that the model "re-discusses" with minor rewording.
        let cycle_a = "So the question is whether idle detection belongs in the LlmActor \
                       or the AgentActor. LlmActor has stream access, AgentActor has state. ";
        let cycle_b = "Hmm, should idle detection be in LlmActor or AgentActor? LlmActor \
                       has the stream, AgentActor has the conversation state. ";
        // Feed past the floor with alternating reworded cycles.
        // Each ~140 chars; need enough to fill two windows + cross floor.
        let mut fired = None;
        for i in 0..30 {
            let chunk = if i % 2 == 0 { cycle_a } else { cycle_b };
            if let Some(r) = d.push(chunk) {
                fired = Some(r);
                break;
            }
        }
        assert_eq!(fired, Some(LoopReason::CycleDetected));
    }

    #[test]
    fn detector_fires_hard_cap_on_novel_long_output() {
        let mut d = small_detector(); // max=2000
        // Feed genuinely novel content — no repeated boilerplate. Each chunk
        // has completely unique vocabulary so keyword Jaccard stays low.
        let mut fired = None;
        let mut idx = 0;
        while fired.is_none() {
            // Synthesize unique prose: vary sentence structure, vocabulary,
            // and topic with each iteration. No repeated framing.
            let chunk = match idx % 6 {
                0 => format!("Examining the B-tree splitting behavior when leaf nodes overflow during bulk insertion of {} random keys into the indexed table.\n", 100 + idx * 17),
                1 => format!("The certificate authority responded with a timeout after {} milliseconds, suggesting we need to increase the TLS handshake retry window.\n", 500 + idx * 23),
                2 => format!("Profile output shows {} percent of CPU time spent in the allocator, pointing toward jemalloc as a potential swap.\n", 30 + idx * 5),
                3 => format!("Refresh token rotation policy of {} days exceeds our compliance window; recommend shortening to fourteen.\n", 45 + idx),
                4 => format!("Benchmarking rwlock versus parking_lot::RwLock across {} reader threads showed unexpected contention on the write path.\n", 8 + idx),
                _ => format!("GC pause of {} microseconds observed during the old-gen sweep, triggering the SLA breach alert.\n", 4000 + idx * 100),
            };
            if let Some(r) = d.push(&chunk) {
                fired = Some(r);
                break;
            }
            idx += 1;
            if idx > 500 {
                break;
            }
        }
        assert_eq!(fired, Some(LoopReason::HardCapHit));
        assert!(d.fed_chars >= 2000);
    }

    #[test]
    fn detector_does_not_fire_on_short_novel_output() {
        let mut d = small_detector();
        let lines = [
            "The quick brown fox jumps over the lazy dog today.",
            "Configuring the web server with TLS certificates now.",
            "Refactoring database queries to use prepared statements.",
            "Adding unit tests for the payment reconciliation module.",
            "Investigating race conditions in the cache invalidation path.",
        ];
        for line in &lines {
            let text = format!("{}\n", line);
            assert!(d.push(&text).is_none(), "should not fire on novel line");
        }
        assert!(d.last_reason.is_none());
    }
}
