//! Five-Whys root cause analysis for context refresh.
//!
//! When the agent performs a context refresh (`restart_from_file`), it gathers all
//! "surprises" recorded during the session, writes them to `tmp/surprises.md`,
//! and spawns an async five-whys LLM call per spec in `tmp/five-whys.md`.
//!
//! Surprises are unexpected incidents: stuck-model loops, cross-turn loop detection,
//! max-iterations reached. Each surprise is converted into an IncidentReport for
//! the analysis prompt.

use serde::{Deserialize, Serialize};
use std::time::SystemTime;

/// A surprise encountered during a session — something unexpected that warrants
/// root cause analysis via five-whys.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Surprise {
    /// Kind: "within_stream_loop", "idle_timeout", "cross_turn_loop",
    /// "max_iterations".
    pub kind: String,
    /// Human-readable trigger description for the analysis prompt.
    pub trigger: String,
    /// The reasoning/output text that was repeating or stuck, truncated to
    /// 8000 chars max per spec §4.1.
    #[serde(default)]
    pub trace_text: String,
    /// Epoch milliseconds when recorded (for filename dedup + ordering).
    #[serde(default)]
    pub timestamp_ms: u64,
}

impl Surprise {
    pub fn new(kind: impl Into<String>, trigger: impl Into<String>) -> Self {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Self {
            kind: kind.into(),
            trigger: trigger.into(),
            trace_text: String::new(),
            timestamp_ms: now,
        }
    }

    /// Attach truncated trace text (the repeating reasoning/output).
    pub fn with_trace(mut self, text: impl Into<String>) -> Self {
        let t = text.into();
        if t.chars().count() > 8000 {
            self.trace_text =
                format!("{}...[truncated]", t.chars().take(8000).collect::<String>());
        } else {
            self.trace_text = t;
        }
        self
    }

    /// Format the timestamp as UTC YYYYmmdd-HHMMSS for filenames.
    pub fn file_timestamp(&self) -> String {
        format_utc_datetime(self.timestamp_ms / 1000)
    }
}

/// IncidentReport per five-whys spec §4.1 — input to the LLM analysis prompt.
#[derive(Debug, Clone)]
pub struct IncidentReport {
    pub kind: String,
    pub trigger: String,
    pub trace_text: String,
    /// Quantitative data (streak count, iteration_count, etc).
    pub metrics: Vec<(String, String)>,
    /// Recent tool calls (name + truncated args) from the last N assistant
    /// messages — shows what the agent was attempting.
    #[allow(dead_code)]
    pub recent_tool_calls: Vec<String>,
}

impl IncidentReport {
    /// Build an IncidentReport from a Surprise plus optional context data.
    pub fn from_surprise(surprise: &Surprise, tool_calls: &[String]) -> Self {
        let trace = if surprise.trace_text.is_empty() {
            "(no trace available)".to_string()
        } else {
            surprise.trace_text.clone()
        };
        Self {
            kind: surprise.kind.clone(),
            trigger: surprise.trigger.clone(),
            trace_text: trace,
            metrics: vec![("kind".into(), surprise.kind.clone())],
            recent_tool_calls: tool_calls.to_vec(),
        }
    }

    /// Kebab-case slug from kind + trigger for filenames.
    pub fn slug(&self) -> String {
        let combined = format!("{}-{}", self.kind, self.trigger);
        let mut slug = String::new();
        let mut prev_hyphen = false;
        for c in combined.chars() {
            if c.is_alphanumeric() {
                slug.push(c.to_ascii_lowercase());
                prev_hyphen = false;
            } else if !prev_hyphen {
                slug.push('-');
                prev_hyphen = true;
            }
        }
        while slug.ends_with('-') {
            slug.pop();
        }
        slug.truncate(80);
        slug
    }

    /// Output path for this report's analysis: tmp/five-whys/{slug}-{timestamp}.md
    pub fn file_path(&self) -> String {
        format!("tmp/five-whys/{}-{}.md", self.slug(), now_timestamp())
    }
}

/// Format epoch seconds as UTC YYYYmmdd-HHMMSS using Howard Hinnant's
/// days-from-civil algorithm — no chrono dependency required.
fn format_utc_datetime(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let sod = secs % 86400; // seconds of day

    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };

    let hh = sod / 3600;
    let mm = (sod % 3600) / 60;
    let ss = sod % 60;

    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        year, m, d, hh, mm, ss
    )
}

fn now_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_utc_datetime(secs)
}

/// Build the five-whys analysis prompt per spec §5.1.
pub fn build_five_whys_prompt(report: &IncidentReport) -> String {
    let mut metrics_text = String::new();
    for (name, value) in &report.metrics {
        metrics_text.push_str(&format!("- {}: {}\n", name, value));
    }

    let tool_calls_text = if report.recent_tool_calls.is_empty() {
        "(none)".to_string()
    } else {
        report
            .recent_tool_calls
            .iter()
            .enumerate()
            .map(|(i, tc)| format!("{}. {}", i + 1, tc))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let trace = if report.trace_text.chars().count() > 8000 {
        format!(
            "{}...[truncated]",
            report.trace_text.chars().take(8000).collect::<String>()
        )
    } else {
        report.trace_text.clone()
    };

    format!(
r#"You are performing a Five-Whys root cause analysis on a coding agent
that got stuck. Your job is to find the STRUCTURAL cause — not to
assign blame or say "be more careful."

## Incident
Type: {kind}
Trigger: {trigger}

## Metrics
{metrics}## Recent tool calls (what the agent was attempting)
{tool_calls}

## Trace (the actual repeated/stuck reasoning, truncated)
"""
{trace}
"""

## Your task
1. Ask "why did this happen?" and answer FACTUALLY. Verify against the
   trace above. If you can't verify, say so explicitly.
2. For each answer, ask "why?" again. Branch if there are multiple
   independent causes. Go 3-5 levels deep.
3. Stop at bedrock: a structural property that can be changed (missing
   guardrail, bad default, absent check, missing prompt rule) OR an
   external boundary OR an accepted tradeoff.
4. Every bedrock node MUST produce an action item that results in a
   concrete artifact: a code fix, a config change, a system-message
   addition, a new test. NOT "be more careful."
5. Output as structured markdown (see format below).

## Output format
# Five-Whys: {{short descriptive title}}

## What happened
{{1-3 sentence factual summary}}

## Why tree
PROBLEM: {{problem}}
├─ WHY: {{answer — verified by [evidence]}}
│  └─ WHY: {{deeper answer}}
│     → BEDROCK: {{structural cause | external boundary | accepted tradeoff}}
│        ACTION: {{concrete artifact: file, diff description}}
├─ WHY: {{branch 2 if multiple causes}}
│  └─ ...
→ BEDROCK: ...

## Action items
1. {{artifact-producing action}} — target: {{file or config}}
2. ...

## Root cause hash
{{single line: 3-5 keywords capturing the structural cause}}"#,
        kind = report.kind,
        trigger = report.trigger,
        metrics = metrics_text,
        tool_calls = tool_calls_text,
        trace = trace,
    )
}

/// Write all recorded surprises to `tmp/surprises.md` as a readable markdown file.
pub fn write_surprises_md(surprises: &[Surprise]) -> String {
    if surprises.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str("# Session Surprises\n\n");
    out.push_str(&format!("Recorded during the session ({} incidents):\n\n", surprises.len()));
    for (i, s) in surprises.iter().enumerate() {
        out.push_str(&format!(
            "## Surprise {} — {}\n- **Kind**: {}\n- **Trigger**: {}\n",
            i + 1,
            s.file_timestamp(),
            s.kind,
            s.trigger,
        ));
        if !s.trace_text.is_empty() {
            let trace_preview: String = s.trace_text.chars().take(500).collect();
            out.push_str(&format!(
                "- **Trace (first 500 chars)**:\n```\n{}\n```\n",
                trace_preview
            ));
        }
        out.push('\n');
    }
    out
}

/// Spawn an async five-whys analysis for a single IncidentReport.
///
/// Per spec §5.3: runs via `tokio::spawn` so it doesn't block the agent loop.
/// Uses temperature 0.3, max_tokens 4096 (cloned config). Writes output to
/// `tmp/five-whys/{slug}-{timestamp}.md`. If the LLM call fails, writes a `.raw`
/// file with the incident report instead so trace data is preserved.
pub fn run_five_whys(
    llm_config: &gladiator_core::LlmConfig,
    report: IncidentReport,
) {
    let mut cfg = llm_config.clone();
    cfg.temperature = 0.3;
    cfg.max_tokens = 4096;

    let prompt = build_five_whys_prompt(&report);
    let path = report.file_path();

    tokio::spawn(async move {
        match gladiator_llm::llm_call(&cfg, &prompt).await {
            Ok(analysis) => {
                let _ = std::fs::create_dir_all("tmp/five-whys");
                let _ = std::fs::write(&path, &analysis);
                tracing::info!("[five-whys] analysis written to {}", path);

                // TUI surfacing would go here — publish an Info message on
                // stream_output_topic. But this is a detached task with no bus
                // handle; the agent loop will see the file if it reads tmp/.
            }
            Err(e) => {
                tracing::warn!("[five-whys] analysis failed: {}", e);
                let raw_path = format!("{}.raw", path);
                let _ = std::fs::create_dir_all("tmp/five-whys");
                let _ = std::fs::write(
                    &raw_path,
                    format!(
                        "# Analysis failed: {}\n\n## IncidentReport\nkind: {}\ntrigger: {}",
                        e, report.kind, report.trigger
                    ),
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surprise_new_sets_timestamp() {
        let s = Surprise::new("within_stream_loop", "stuck repeating");
        assert_eq!(s.kind, "within_stream_loop");
        assert!(!s.trigger.is_empty());
        assert!(s.timestamp_ms > 0);
    }

    #[test]
    fn trace_truncation_at_8000_chars() {
        let long_text = "x".repeat(10000);
        let s = Surprise::new("idle_timeout", "went idle").with_trace(&long_text);
        // Truncated text should be longer than 8000 (adds suffix) but the
        // core content is capped at 8000 chars.
        assert!(s.trace_text.contains("[truncated]"));
    }

    #[test]
    fn incident_report_slug_is_kebab() {
        let s = Surprise::new("within_stream_loop", "stuck on edit signature");
        let r = IncidentReport::from_surprise(&s, &[]);
        let slug = r.slug();
        assert!(slug.contains("within-stream-loop"));
        assert!(!slug.is_empty());
    }

    #[test]
    fn file_path_format() {
        let s = Surprise::new("cross_turn_loop", "identical triage text");
        let r = IncidentReport::from_surprise(&s, &[]);
        let path = r.file_path();
        assert!(path.starts_with("tmp/five-whys/"));
        assert!(path.ends_with(".md"));
    }

    #[test]
    fn build_prompt_contains_sections() {
        let s = Surprise::new("max_iterations", "hit 50 iterations").with_trace("reading files repeatedly");
        let r = IncidentReport::from_surprise(&s, &["bash ls".to_string()]);
        let prompt = build_five_whys_prompt(&r);
        assert!(prompt.contains("## Incident"));
        assert!(prompt.contains("Type: max_iterations"));
        assert!(prompt.contains("Trigger: hit 50 iterations"));
        assert!(prompt.contains("Recent tool calls"));
        assert!(prompt.contains("1. bash ls"));
    }

    #[test]
    fn write_surprises_md_empty_returns_empty() {
        let out = write_surprises_md(&[]);
        assert!(out.is_empty());
    }

    #[test]
    fn write_surprises_md_nonempty_has_headers() {
        let s = Surprise::new("within_stream_loop", "stuck").with_trace("repeating reasoning");
        let out = write_surprises_md(&[s]);
        assert!(out.contains("# Session Surprises"));
        assert!(out.contains("Kind**: within_stream_loop"));
    }

    #[test]
    fn format_utc_datetime_epoch() {
        assert_eq!(format_utc_datetime(0), "19700101-000000");
    }

    #[test]
    fn format_utc_datetime_known_instant() {
        // 2025-01-01 00:00:00 UTC = 1735689600
        let result = format_utc_datetime(1735689600);
        assert_eq!(result, "20250101-000000");
    }

    #[test]
    fn format_utc_datetime_midday() {
        // 2024-02-29 12:34:56 UTC (leap day) = 1709210096
        let result = format_utc_datetime(1709210096);
        assert_eq!(result, "20240229-123456");
    }
}
