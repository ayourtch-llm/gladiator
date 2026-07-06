use crate::five_whys::Surprise;
use crate::internal_tools::{render_todos, TodoEntry};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ConversationState {
    pub messages: Vec<serde_json::Value>,
    pub iteration_count: u32,
    pub pending_tool_calls: HashSet<String>,
    pub pending_messages: Vec<String>,
    pub was_interrupted: bool,
    /// Transient agent-internal todo list. Saved/restored with the rest of the
    /// conversation state (not a separate disk file). Manipulated by the
    /// internal `todo_write` / `todo_read` tools, handled inline by the agent.
    #[serde(default)]
    pub todos: Vec<TodoEntry>,
    /// Accumulated reasoning from LlmThinking stream chunks.
    /// Transient: not serialized (see #[serde(skip)]). Attached to the
    /// next assistant/tool_calls message via "reasoning" field.
    #[serde(skip)]
    pub current_reasoning: String,
    /// Accumulated partial content from LlmStream chunks.
    /// Transient: not serialized. Used to preserve partial assistant
    /// text when the LLM is interrupted mid-response.
    #[serde(skip)]
    pub current_partial_response: String,
    /// Ordered list of tool_call IDs in the current batch. Used to
    /// reorder tool results to match the LLM's tool_calls array.
    /// Transient: not serialized.
    #[serde(skip)]
    pub tool_call_order: Vec<String>,
    /// True while an LLM inference request is in flight (between sending the
    /// conversation and receiving either a final output or tool calls).
    /// When set, user messages arriving on the input topic are buffered into
    /// `pending_messages` instead of starting a new turn — matching the
    /// behaviour already used for pending tool calls.
    #[serde(skip)]
    pub inference_in_flight: bool,
    /// Last reported token usage from the LLM (`StreamStats.usage`),
    /// published at end-of-stream by the LLM actor. Transient: not
    /// serialized — refreshed each turn from the live bus.
    #[serde(skip)]
    pub last_usage: Option<Usage>,
    /// Model context window in tokens, when known. Transient: not
    /// serialized — populated at agent startup from LlmConfig.
    #[serde(skip)]
    pub context_window: Option<usize>,
    /// Hashes of recent assistant turns (reasoning + tool-call args) for
    /// cross-turn loop detection. Transient. When the last N entries are
    /// near-identical, the agent injects a tie-breaker perturbation.
    #[serde(skip)]
    pub recent_turn_hashes: Vec<u64>,
    /// Context reminder thresholds set by the `record_conclusion` or future
    /// internal tools. Each has an absolute token count and a message; when
    /// usage crosses it, the message is injected once into pending_messages.
    /// Transient — not serialized across restarts (a fresh context starts clean).
    #[serde(skip)]
    pub context_reminders: Vec<ContextReminder>,
    /// Scheduled wake-ups (one-shot or recurring cron-like). Each has a
    /// next-due instant and an optional repeat interval. When due, the message
    /// is injected into pending_messages if the loop is idle.
    /// Transient — not serialized across restarts.
    #[serde(skip)]
    pub wake_ups: Vec<WakeUp>,
    /// Surprises recorded during the session (stuck-model loops, cross-turn
    /// loops, max-iterations). Written to tmp/surprises.md on context refresh,
    /// then analyzed via five-whys llm_call per spec in tmp/five-whys.md.
    #[serde(skip)]
    pub surprises: Vec<Surprise>,
    /// Subagent stack depth (0 = top-level agent). When > 0, the agent is
    /// running inside a call_subagent invocation. Used for log indentation and
    /// to detect inner-conversation completion so we can pop back.
    #[serde(skip)]
    pub subagent_depth: usize,
    /// Active system message override used when running as a subagent (depth >
    /// 0). When None, the agent's default system_message is used. Set by
    /// call_subagent push; cleared on pop.
    #[serde(skip)]
    pub active_system_message: Option<String>,
    /// One-shot flag: auto-nudge at ~90% context has fired for this context.
    /// Reset on clear_for_restart (fresh subagent context gets a fresh nudge).
    #[serde(skip)]
    pub context_nudge_fired: bool,

    /// Tool calls from the current batch that haven't been dispatched yet
    /// (sent to a tool actor or handled internally). Populated by
    /// `add_tool_calls`; drained one-by-one by the agent's dispatch loop.
    /// Used to serialize subagent calls: when the batch contains
    /// `call_subagent`, tools after it are deferred until the subagent
    /// completes. IDs are NOT added to `pending_tool_calls` until dispatch.
    #[serde(skip)]
    pub undispatched_tool_calls: Vec<serde_json::Value>,
}

/// A one-shot context-usage reminder. When `last_usage.input_tokens` exceeds
/// `threshold_tokens`, the agent injects `message` as a user message into its
/// own pending list, then marks `fired = true` so it never fires again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextReminder {
    pub threshold_tokens: u64,
    pub message: String,
    #[serde(default)]
    pub fired: bool,
}

/// A scheduled wake-up. `next_due` is the epoch ms when it should fire.
/// If `interval_secs` is Some, it's a recurring (cron-like) wake-up that
/// reschedules itself after firing; if None, it's one-shot and removed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WakeUp {
    pub next_due_ms: u64,
    pub message: String,
    /// If Some(N), the wake-up recurs every N seconds. If None, one-shot.
    #[serde(default)]
    pub interval_secs: Option<u64>,
}

/// Subset of `gladiator_llm::Usage` mirrored locally so the agent crate
/// doesn't need a direct dep on gladiator-llm just for the field shape.
/// Constructed from a `StreamStats` payload at the bus boundary.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

impl ConversationState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_user_message(&mut self, content: impl Into<String>) {
        self.current_reasoning.clear();
        let content_str = content.into();
        self.messages.push(serde_json::json!({
            "role": "user",
            "content": content_str
        }));
    }

    /// Merge a user message with the last message if it's also a user message.
    /// If the last message is a user message, append the new content to it
    /// (separated by a newline). Otherwise, add as a new user message.
    /// This is used after an interrupt to avoid sending two user messages
    /// in a row to the LLM.
    pub fn merge_user_message(&mut self, content: impl Into<String>) {
        self.current_reasoning.clear();
        let content_str = content.into();
        if let Some(last) = self.messages.last_mut() {
            if last.get("role").and_then(|r| r.as_str()) == Some("user") {
                if let Some(existing) = last.get("content").and_then(|c| c.as_str()).map(|s| s.to_string()) {
                    let new_content = format!("{}\n{}", existing, content_str);
                    last["content"] = serde_json::Value::String(new_content);
                    return;
                }
            }
        }
        // If last message is not a user message, add as new user message
        self.add_user_message(content_str);
    }

    /// Append a chunk of reasoning content accumulated during streaming.
    /// Chunks are concatenated directly (matching TUI streaming behavior).
    pub fn append_reasoning(&mut self, chunk: &str) {
        self.current_reasoning.push_str(chunk);
    }

    /// Append a chunk of partial response content accumulated during streaming.
    pub fn append_partial_response(&mut self, chunk: &str) {
        self.current_partial_response.push_str(chunk);
    }

    /// Drain accumulated partial response, returning it if non-empty.
    pub fn drain_partial_response(&mut self) -> Option<String> {
        if self.current_partial_response.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.current_partial_response))
        }
    }

    /// Drain accumulated reasoning, returning it if non-empty.
    fn drain_reasoning(&mut self) -> Option<String> {
        if self.current_reasoning.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.current_reasoning))
        }
    }

    /// Clear any accumulated reasoning (e.g. after an interrupt).
    pub fn clear_reasoning(&mut self) {
        self.current_reasoning.clear();
    }

    /// Clear accumulated partial response (e.g. after an interrupt).
    pub fn clear_partial_response(&mut self) {
        self.current_partial_response.clear();
    }

    pub fn add_assistant_message(&mut self, content: impl Into<String>) {
        let reasoning = self.drain_reasoning();
        // The committed text supersedes any streamed partial; clear it so it
        // can't leak into a later tool-call turn's content.
        self.clear_partial_response();
        let mut msg = serde_json::json!({
            "role": "assistant",
            "content": content.into()
        });
        if let Some(r) = reasoning {
            msg["reasoning"] = serde_json::Value::String(r);
        }
        self.messages.push(msg);
    }

    pub fn add_tool_calls(&mut self, tool_calls: Vec<serde_json::Value>) {
        let reasoning = self.drain_reasoning();
        // Preserve any natural-language content the model emitted alongside the
        // tool calls (streamed as LlmStream chunks). Without this the assistant
        // turn is pure tool_calls with no words, so the model has no durable
        // record of its own decisions and re-derives them on later turns.
        let content = self.drain_partial_response();
        // Record the order of tool call IDs for reordering results later.
        // Synthesize ids for empty/missing ones so they don't collapse in the HashSet.
        self.tool_call_order.clear();
        for (i, tc) in tool_calls.iter().enumerate() {
            let id = tc["id"].as_str().unwrap_or("");
            let id = if id.is_empty() {
                format!("__idx_{}", i)
            } else {
                id.to_string()
            };
            self.tool_call_order.push(id);
        }
        // Stash tool calls for sequential dispatch. IDs are NOT added to
        // pending_tool_calls here — that happens one-by-one as the dispatch
        // loop sends each tool call. This lets the dispatch loop serialize
        // call_subagent: tools after it wait until the subagent completes.
        self.undispatched_tool_calls = tool_calls.clone();
        let mut msg = serde_json::json!({
            "role": "assistant",
            "tool_calls": tool_calls
        });
        if let Some(c) = content {
            if !c.is_empty() {
                msg["content"] = serde_json::Value::String(c);
            }
        }
        if let Some(r) = reasoning {
            msg["reasoning"] = serde_json::Value::String(r);
        }
        self.messages.push(msg);
    }

    /// Mark a tool call as dispatched (add its ID to pending_tool_calls).
    /// Called by the dispatch loop right before sending each tool call to
    /// a tool actor or handling it internally.
    pub fn mark_dispatched(&mut self, tool_call_id: &str) {
        self.pending_tool_calls.insert(tool_call_id.to_string());
    }

    /// Mark all undispatched tool calls as dispatched: drain
    /// `undispatched_tool_calls` and insert each ID into `pending_tool_calls`.
    /// The dispatch loop calls this per-tool; tests can call it to simulate
    /// "all tools in flight" without running the full dispatch loop.
    pub fn mark_all_dispatched(&mut self) {
        for tc in self.undispatched_tool_calls.drain(..) {
            let id = tc["id"].as_str().unwrap_or("").to_string();
            self.pending_tool_calls.insert(id);
        }
    }

    /// Record a signature of the most recently added assistant message and
    /// return the cross-turn loop streak count if the last `window` assistant
    /// turns are near-identical. Returns 0 if no loop.
    ///
    /// The signature is a hash of the reasoning text + tool-call function
    /// names + truncated arguments. Identical turns produce identical hashes;
    /// near-duplicate turns (same approach, minor wording changes) are caught
    /// by also comparing the raw text via the caller if hashes differ.
    pub fn record_turn_and_check_loop(&mut self) -> usize {
        let last_assistant = self.messages.iter().rev().find_map(|m| {
            if m.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                Some(m)
            } else {
                None
            }
        });
        let Some(msg) = last_assistant else { return 0 };

        // Build a signature: reasoning + tool call names + arg prefixes.
        let reasoning = msg.get("reasoning").and_then(|r| r.as_str()).unwrap_or("");
        let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
        let mut sig = String::new();
        // Use the last 1000 chars of reasoning — the loop content typically
        // appears verbatim at the end. Ignore the first part which may vary.
        let skip = reasoning.chars().count().saturating_sub(1000);
        if skip == 0 {
            sig.push_str(reasoning);
        } else if let Some((idx, _)) = reasoning.char_indices().nth(skip) {
            sig.push_str(&reasoning[idx..]);
        }
        if let Some(tool_calls) = msg.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tool_calls {
                let name = tc["function"]["name"].as_str().unwrap_or("");
                let args = tc["function"]["arguments"].as_str().unwrap_or("");
                sig.push_str(name);
                sig.push_str(&args.chars().take(200).collect::<String>());
            }
        }
        if sig.is_empty() {
            sig.push_str(&content.chars().take(200).collect::<String>());
        }

        let hash = self.hash_str(&sig);
        self.recent_turn_hashes.push(hash);
        // Keep last 10
        if self.recent_turn_hashes.len() > 10 {
            self.recent_turn_hashes.remove(0);
        }

        // Count consecutive trailing duplicates of the latest hash.
        let latest = self.recent_turn_hashes.last().copied().unwrap_or(0);
        let mut streak = 0usize;
        for &h in self.recent_turn_hashes.iter().rev() {
            if h == latest {
                streak += 1;
            } else {
                break;
            }
        }
        streak
    }

    /// FNV-1a hash — fast, no dep.
    fn hash_str(&self, s: &str) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for b in s.as_bytes() {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    /// Collapse duplicate turn-cycles from the message history.
    ///
    /// When `record_turn_and_check_loop` detects a streak of N identical turns,
    /// this method removes all but the most recent cycle (assistant message +
    /// its tool results), replacing the removed messages with a single system
    /// note. This prevents the accumulated duplicates from acting as an
    /// attractor that keeps pulling the model back into the same loop.
    ///
    /// Returns the number of messages removed.
    pub fn collapse_loop_turns(&mut self) -> usize {
        if self.recent_turn_hashes.len() < 3 {
            return 0;
        }
        let latest_hash = self.recent_turn_hashes.last().copied().unwrap_or(0);

        // Count trailing identical hashes.
        let mut dup_count = 0usize;
        for &h in self.recent_turn_hashes.iter().rev() {
            if h == latest_hash {
                dup_count += 1;
            } else {
                break;
            }
        }
        if dup_count < 3 {
            return 0;
        }

        // Find assistant message indices from the end, matching the trailing
        // dup_count hashes. We walk messages in reverse, collecting (index, is_assistant).
        let mut assistant_indices: Vec<usize> = Vec::new();
        for (i, m) in self.messages.iter().enumerate() {
            if m.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                assistant_indices.push(i);
            }
        }
        if assistant_indices.len() < dup_count {
            return 0; // not enough recorded
        }

        // The last `dup_count` assistant messages form the loop. Keep the last
        // one (most recent tool results); remove the earlier dup_count-1 cycles.
        let keep_idx = *assistant_indices.last().unwrap();
        // Start removal from the assistant message that's dup_count-1 from the end.
        let remove_start_assistant = assistant_indices[assistant_indices.len() - dup_count];
        // Removal range: from remove_start_assistant up to (but not including) keep_idx.
        // This removes the duplicate assistant messages AND their interleaved tool results,
        // preserving only the final cycle.
        let removed_count = keep_idx - remove_start_assistant;

        // Extract a snippet from the removed reasoning for the compaction note.
        let removed_reasoning = self.messages[remove_start_assistant]
            .get("reasoning")
            .and_then(|r| r.as_str())
            .unwrap_or("");
        let snippet: String = removed_reasoning.chars().take(300).collect();

        // Perform removal — drop the duplicate cycles.
        self.messages.drain(remove_start_assistant..keep_idx);

        // Insert a fake user/assistant turn where the removed duplicates were.
        // This preserves the user/assistant alternation expected by OpenAI-style
        // chat APIs and gives the model context about what was compacted, so it
        // doesn't re-derive the same dead-end from scratch.
        let compaction_note = format!(
            "[context note: {} duplicate turn-cycles were compacted from history because they \
             repeated the same approach without progress. Do not re-attempt the same approach. \
             Compacted reasoning snippet for reference: {:?}]",
            dup_count - 1,
            snippet,
        );
        self.messages.insert(
            remove_start_assistant,
            serde_json::json!({"role": "user", "content": compaction_note.clone()}),
        );
        self.messages.insert(
            remove_start_assistant + 1,
            serde_json::json!({
                "role": "assistant",
                "content": format!(
                    "Understood — I was stuck repeating myself ({} identical turns). \
                     I will try a different approach.",
                    dup_count - 1,
                ),
            }),
        );

        // Reset hash tracking — the streak is broken by the compaction.
        self.recent_turn_hashes.clear();

        tracing::info!(
            "[loop-collapse] removed {} messages ({} duplicate turns), inserted compaction turn",
            removed_count,
            dup_count - 1,
        );
        removed_count
    }

    pub fn add_tool_result(
        &mut self,
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
        success: bool,
    ) {
        let result_content = if success {
            content.into()
        } else {
            format!("Error: {}", content.into())
        };
        self.messages.push(serde_json::json!({
            "role": "tool",
            "tool_call_id": tool_call_id.into(),
            "name": name.into(),
            "content": result_content,
        }));
    }

    pub fn resolve_tool_call(&mut self, tool_call_id: &str) {
        self.pending_tool_calls.remove(tool_call_id);
        // When all tool calls in the current batch are resolved, reorder
        // the tool result messages to match the original tool_calls array
        // order from the LLM. Wait for undispatched calls too — otherwise
        // we'd reorder prematurely when serialized subagent calls defer
        // later tools in the batch.
        if self.pending_tool_calls.is_empty()
            && self.undispatched_tool_calls.is_empty()
            && !self.tool_call_order.is_empty()
        {
            self.reorder_tool_results();
        }
    }

    pub fn all_tool_calls_resolved(&self) -> bool {
        self.pending_tool_calls.is_empty() && self.undispatched_tool_calls.is_empty()
    }

    /// Reorder the last N tool result messages to match the original
    /// tool_calls array order. Called when all tool calls in a batch
    /// are resolved. N is determined by `tool_call_order.len()`.
    fn reorder_tool_results(&mut self) {
        let n = self.tool_call_order.len();
        if n == 0 || self.messages.len() < n {
            self.tool_call_order.clear();
            return;
        }

        // The last n messages should be the tool results from this batch
        let start = self.messages.len() - n;
        let mut tool_results: Vec<serde_json::Value> = self.messages.drain(start..).collect();

        // Sort tool results to match the original tool_call_order
        let order = self.tool_call_order.clone();
        tool_results.sort_by_key(|result| {
            let tc_id = result
                .get("tool_call_id")
                .and_then(|t| t.as_str())
                .unwrap_or("");
            order.iter().position(|id| id == tc_id).unwrap_or(usize::MAX)
        });

        for result in tool_results {
            self.messages.push(result);
        }

        self.tool_call_order.clear();
    }

    pub fn increment_iteration(&mut self) {
        self.iteration_count += 1;
    }

    /// Reset the iteration counter to zero. Called when a new user message
    /// arrives so the agent gets a fresh iteration budget for the new turn.
    pub fn reset_iteration(&mut self) {
        self.iteration_count = 0;
    }

    pub fn max_reached(&self, max: u32) -> bool {
        self.iteration_count >= max
    }

    /// Build a human-readable summary of the last `n` user/assistant messages,
    /// suitable for a handoff file when max_iterations is reached.
    pub fn recent_messages_summary(&self, n: usize) -> String {
        let mut out: Vec<String> = Vec::new();
        for msg in &self.messages {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("?");
            if role == "user" || role == "assistant" {
                let content = msg
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("(non-text content)");
                out.push(format!("{}: {}", role, content));
            }
        }
        let start = out.len().saturating_sub(n);
        let mut summary = String::new();
        summary.push_str(&format!(
            "# Max-iterations handoff\n\nLast {} of {} user/assistant messages:\n\n",
            out.len() - start,
            out.len(),
        ));
        for line in &out[start..] {
            // Truncate individual messages to keep the file readable.
            let truncated = if line.chars().count() > 2000 {
                format!("{}...[truncated]", line.chars().take(2000).collect::<String>())
            } else {
                line.to_string()
            };
            summary.push_str(&truncated);
            summary.push_str("\n\n---\n\n");
        }
        summary.push_str(&format!(
            "\niteration_count: {} (max)\n",
            self.iteration_count
        ));
        summary
    }

    pub fn buffer_user_message(&mut self, message: String) {
        self.pending_messages.push(message);
    }

    pub fn drain_pending_messages(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_messages)
    }

    pub fn build_messages_with_system(&self, system_message: &str) -> Vec<serde_json::Value> {
        let active = self.active_system_message.as_ref().map(|s| s.as_str()).unwrap_or(system_message);
        let mut result = Vec::new();
        if !active.is_empty() {
            result.push(serde_json::json!({"role": "system", "content": active}));
        }
        for msg in &self.messages {
            let mut m = msg.clone();
            if let serde_json::Value::Object(ref mut obj) = m {
                obj.remove("reasoning");
            }
            result.push(m);
        }
        // Doctor outgoing tool_calls arguments to prevent HTTP 500 from
        // corrupted JSON in conversation history.
        let repairs = crate::outgoing_doctor::doctor_messages(&mut result);
        for r in &repairs {
            tracing::warn!(
                "[outgoing-doctor] msg[{}]: dropped broken {} (args unparseable)",
                r.msg_index,
                if r.tool_names.len() == 1 { r.tool_names[0].clone() } else { format!("batch of {}", r.tool_names.len()) },
            );
        }
        result
    }

    /// Replace the entire todo list with `entries`. Returns the rendered
    /// summary that callers can feed back as the tool result.
    pub fn set_todos(&mut self, entries: Vec<TodoEntry>) -> String {
        self.todos = entries;
        render_todos(&self.todos)
    }

    /// Rendered snapshot of the current todo list (empty-safe).
    pub fn todos_render(&self) -> String {
        render_todos(&self.todos)
    }

    /// Record the latest per-turn usage stats. Called when a `StreamStats`
    /// message arrives on the bus. Also updates `context_window` if the
    /// stats carry one (so a runtime probe by the LLM actor propagates here).
    pub fn record_usage(
        &mut self,
        usage: Usage,
        context_window: Option<usize>,
    ) {
        self.last_usage = Some(usage);
        if let Some(w) = context_window {
            self.context_window = Some(w);
        }
    }

    /// Add a one-shot context reminder. When `last_usage.input_tokens` crosses
    /// `threshold`, the agent injects `message` into pending_messages once.
    pub fn add_context_reminder(&mut self, threshold: u64, message: String) {
        // Replace existing reminder at same threshold (update message).
        if let Some(r) = self.context_reminders.iter_mut()
            .find(|r| r.threshold_tokens == threshold)
        {
            r.message = message;
            r.fired = false; // re-arm on update
            return;
        }
        self.context_reminders.push(ContextReminder {
            threshold_tokens: threshold,
            message,
            fired: false,
        });
    }

    /// Remove all context reminders (e.g., when clearing state).
    pub fn clear_context_reminders(&mut self) {
        self.context_reminders.clear();
    }

    /// Check if any unfired reminder thresholds have been crossed. Returns
    /// messages to inject and marks them as fired.
    pub fn check_context_reminders(&mut self, current_tokens: u64)
        -> Vec<String>
    {
        let mut injected = Vec::new();
        for r in &mut self.context_reminders {
            if !r.fired && current_tokens >= r.threshold_tokens {
                r.fired = true;
                injected.push(r.message.clone());
            }
        }
        // Inject into pending_messages so the agent loop picks them up.
        for msg in &injected {
            self.pending_messages.push(msg.clone());
        }
        injected
    }

    /// Add a one-shot wake-up: fires at `delay_secs` from now, then removed.
    pub fn add_one_shot_wake_up(&mut self, delay_secs: u64, message: String) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.wake_ups.push(WakeUp {
            next_due_ms: now_ms + delay_secs * 1000,
            message,
            interval_secs: None,
        });
    }

    /// Add a recurring wake-up: fires every `interval_secs`, starting at
    /// `delay_secs` from now.
    pub fn add_cron_wake_up(
        &mut self,
        delay_secs: u64,
        interval_secs: u64,
        message: String,
    ) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.wake_ups.push(WakeUp {
            next_due_ms: now_ms + delay_secs * 1000,
            message,
            interval_secs: Some(interval_secs),
        });
    }

    /// Remove all wake-ups.
    pub fn clear_wake_ups(&mut self) {
        self.wake_ups.clear();
    }

    /// Check for due wake-ups. Returns messages to inject (into pending_messages)
    /// and reschedules or removes fired entries. Only fires when the agent loop
    /// is idle (`inference_in_flight == false` and no `pending_tool_calls`);
    /// otherwise, cron wake-ups are cancelled (rescheduled) per fixme spec.
    pub fn check_wake_ups(&mut self) -> Vec<String> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        // Per fixme: inject only if loop is idle; cancel otherwise.
        let is_idle = !self.inference_in_flight
            && self.pending_tool_calls.is_empty()
            && self.undispatched_tool_calls.is_empty();
        if !is_idle {
            // For one-shot wake-ups that can't fire because we're busy:
            // reschedule them to the next tick (defer). For cron: also defer.
            for w in &mut self.wake_ups {
                if w.next_due_ms <= now_ms && w.interval_secs.is_none() {
                    // One-shot not yet fired — push it slightly into the future
                    // so we check again on the next tick. This effectively defers
                    // until idle.
                    w.next_due_ms = now_ms + 1000;
                }
            }
            return Vec::new();
        }

        let mut injected = Vec::new();
        self.wake_ups.retain(|w| {
            if w.next_due_ms <= now_ms {
                injected.push(w.message.clone());
                match w.interval_secs {
                    Some(_secs) => {
                        // Reschedule recurring wake-up.
                        // We can't mutate while retaining, so we'll handle
                        // rescheduling after the retain. For now, keep it in
                        // the list (return true).
                        true
                    }
                    None => false, // Remove one-shot after firing.
                }
            } else {
                true // Not due yet — keep.
            }
        });

        // Reschedule recurring wake-ups that fired.
        for w in &mut self.wake_ups {
            if injected.contains(&w.message) && w.interval_secs.is_some() {
                let interval_ms = w.interval_secs.unwrap() * 1000;
                w.next_due_ms = now_ms + interval_ms;
            }
        }

        // Inject into pending_messages so the agent loop picks them up.
        for msg in &injected {
            self.pending_messages.push(msg.clone());
        }
        injected
    }

    /// Record a surprise (unexpected incident) during the session. Called at
    /// trigger sites: triage_stuck_model, maybe_break_cross_turn_loop,
    /// max-iterations reached. These are written to tmp/surprises.md and fed
    /// into five-whys analysis on context refresh.
    pub fn record_surprise(&mut self, surprise: Surprise) {
        self.surprises.push(surprise);
    }

    /// Drain all recorded surprises (for writing before context refresh).
    pub fn take_surprises(&mut self) -> Vec<Surprise> {
        std::mem::take(&mut self.surprises)
    }
    /// usage. `None` when either piece is unknown.
    pub fn context_remaining(&self) -> Option<u64> {
        let window = self.context_window? as u64;
        let used = self
            .last_usage
            .as_ref()
            .and_then(|u| u.input_tokens)
            .unwrap_or(0);
        window.checked_sub(used)
    }

    /// Human-readable one-line context status for tool results / logs.
    pub fn context_status_line(&self) -> String {
        match (self.context_remaining(), &self.last_usage) {
            (Some(remaining), Some(u)) => {
                let used = u.input_tokens.unwrap_or(0);
                let pct = if self.context_window.unwrap_or(0) > 0 {
                    (used as f64 / self.context_window.unwrap() as f64) * 100.0
                } else {
                    0.0
                };
                format!(
                    "context: {}/{} tokens used ({:.0}%), {} remaining",
                    used, self.context_window.unwrap(), pct, remaining
                )
            }
            (None, Some(u)) => format!(
                "context: {} input tokens used (window unknown)",
                u.input_tokens.unwrap_or(0)
            ),
            _ => "context: no usage reported yet".to_string(),
        }
    }

    /// Wipe the entire conversation context: messages, todos, pending tool
    /// calls, pending user messages, iteration counter, and interrupt flag.
    /// Used by the `restart_from_file` internal tool after a backup snapshot
    /// has been written to disk, so the next injected user message starts a
    /// clean transcript. Transient accumulator fields (reasoning / partial
    /// response / tool_call_order) are cleared too so nothing leaks across the
    /// restart boundary.
    pub fn clear_for_restart(&mut self) {
        self.messages.clear();
        self.todos.clear();
        self.pending_tool_calls.clear();
        self.pending_messages.clear();
        self.iteration_count = 0;
        self.was_interrupted = false;
        self.current_reasoning.clear();
        self.current_partial_response.clear();
        self.tool_call_order.clear();
        self.inference_in_flight = false;
        self.context_reminders.clear();
        self.wake_ups.clear();
        self.context_nudge_fired = false;
        self.undispatched_tool_calls.clear();
        // Note: surprises are NOT cleared here — they're drained via
        // take_surprises() in handle_restart_from_file before clear_for_restart,
        // so the agent can write tmp/surprises.md and spawn five-whys analysis.
    }
}