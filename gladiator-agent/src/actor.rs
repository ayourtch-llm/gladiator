use crate::five_whys::{IncidentReport, Surprise};
use crate::internal_tools::InternalToolOutcome;
use crate::state::ConversationState;
use gladiator_core::{Actor, ActorAnnouncement, AgentConfig, Bus, Message};
use gladiator_llm::LlmRequest;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, error, info, warn};

/// A saved snapshot of the parent agent's context when a subagent is spawned.
/// Each push saves system_message + ConversationState so they can be restored
/// on pop. The stack depth equals indentation level for log/stream messages.
#[derive(Debug, Clone)]
pub struct SubagentFrame {
    /// The parent's conversation state at the time of the call_subagent push.
    pub saved_state: ConversationState,
    /// The parent agent's system message that was active before the push.
    pub saved_system_message: String,
}

/// Resolve a path against the agent working directory. Absolute paths and
/// `~`-prefixed paths are used as-is; relative paths are joined onto the
/// working dir (with `.` interpreted as the process cwd). Mirrors the
/// `resolve_path` helper used by the built-in file tools.
fn resolve_against_working_dir(path: &str, working_dir: &str) -> String {
    if path.starts_with('/') || path.starts_with('~') {
        return path.to_string();
    }
    let wd = if working_dir == "." {
        std::env::current_dir()
            .map(|d| d.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    } else {
        working_dir.to_string()
    };
    format!("{}/{}", wd, path)
}

#[derive(Debug)]
pub struct AgentActor {
    pub index: usize,
    pub input_topic: String,
    pub llm_in_topic: String,
    pub llm_out_topic: String,
    pub llm_stream_topic: String,
    pub llm_tool_calls_topic: String,
    pub tool_results_topic: String,
    pub stream_output_topic: String,
    pub config: AgentConfig,
    pub max_iterations: u32,
    pub system_message: String,
    pub tool_defs: Vec<serde_json::Value>,
    pub tool_timeout_secs: u64,
    pub state_control_topic: String,
    pub state_topic: String,
    /// LLM stats topic to subscribe to for per-turn usage / context window.
    /// Empty means "do not subscribe" (used by tests that don't publish stats).
    pub llm_stats_topic: String,
    /// Context window discovered at startup (from LlmConfig, which may itself
    /// have probed /v1/models). Seeded into ConversationState on first use so
    /// the agent can compute "tokens remaining" before the first stats arrive.
    pub context_window: Option<usize>,
    /// LLM config for triage calls (stuck-model detection). When the LLM
    /// goes idle for >90s, the agent uses this to call llm_call for triage.
    /// None disables triage (idle timeout still breaks the stream).
    pub llm_config: Option<gladiator_core::LlmConfig>,
}

impl Default for AgentActor {
    fn default() -> Self {
        Self {
            index: 0,
            input_topic: String::new(),
            llm_in_topic: String::new(),
            llm_out_topic: String::new(),
            llm_stream_topic: String::new(),
            llm_tool_calls_topic: String::new(),
            tool_results_topic: String::new(),
            stream_output_topic: String::new(),
            config: AgentConfig::default(),
            max_iterations: 200,
            system_message: String::new(),
            tool_defs: Vec::new(),
            tool_timeout_secs: 300,
            state_control_topic: String::new(),
            state_topic: String::new(),
            llm_stats_topic: String::new(),
            context_window: None,
            llm_config: None,
        }
    }
}

impl AgentActor {
    pub fn new(
        index: usize,
        input_topic: String,
        llm_in_topic: String,
        llm_out_topic: String,
        llm_stream_topic: String,
        llm_tool_calls_topic: String,
        tool_results_topic: String,
        stream_output_topic: String,
        config: AgentConfig,
    ) -> Self {
        Self {
            index,
            input_topic,
            llm_in_topic,
            llm_out_topic,
            llm_stream_topic,
            llm_tool_calls_topic,
            tool_results_topic,
            stream_output_topic,
            max_iterations: config.max_iterations,
            system_message: config.system_message.clone(),
            config,
            tool_defs: Vec::new(),
            tool_timeout_secs: 300,
            state_control_topic: String::new(),
            state_topic: String::new(),
            llm_stats_topic: String::new(),
            context_window: None,
            llm_config: None,
        }
    }

    pub fn with_max_iterations(mut self, max: u32) -> Self {
        self.max_iterations = max;
        self
    }

    pub fn with_system_message(mut self, msg: String) -> Self {
        self.system_message = msg;
        self
    }

    pub fn with_tool_defs(mut self, defs: Vec<serde_json::Value>) -> Self {
        self.tool_defs = defs;
        self
    }

    pub fn with_tool_timeout_secs(mut self, secs: u64) -> Self {
        self.tool_timeout_secs = secs;
        self
    }

    pub fn with_state_topics(mut self, control: String, state: String) -> Self {
        self.state_control_topic = control;
        self.state_topic = state;
        self
    }

    /// Subscribe to the LLM stats topic for per-turn usage / context-window
    /// updates. Pair with `with_context_window` to seed the initial window.
    pub fn with_llm_stats_topic(mut self, topic: String) -> Self {
        self.llm_stats_topic = topic;
        self
    }

    /// Seed the model context window (in tokens) discovered at startup.
    pub fn with_context_window(mut self, window: Option<usize>) -> Self {
        self.context_window = window;
        self
    }

    /// Provide the LLM config so the agent can make standalone triage calls
    /// when the model goes idle (stuck-model detection).
    pub fn with_llm_config(mut self, config: gladiator_core::LlmConfig) -> Self {
        self.llm_config = Some(config);
        self
    }

    /// Stamp a message with the current subagent depth (from ConversationState)
    /// so the TUI can indent nested output. No-op when at top level (depth 0).
    /// Stamp the current subagent depth onto a bus message. Sync — the
    /// caller is responsible for reading `depth` while it already holds the
    /// state lock (avoids a second lock acquire, which would deadlock since
    /// tokio::sync::Mutex is not reentrant).
    fn stamp_depth_sync(&self, msg: Message, depth: usize) -> Message {
        if depth > 0 {
            msg.with_depth(depth)
        } else {
            msg
        }
    }

    /// Cross-turn loop breaker: when the agent detects that consecutive turns
    /// are near-identical, it (1) collapses the duplicate turns from history
    /// to remove the attractor, then (2) injects a tie-breaker message with a
    /// random number. The randomness perturbs the context enough to escape
    /// the deterministic attractor without forcing the model to "admit defeat."
    async fn maybe_break_cross_turn_loop(
        &self,
        bus: &Bus,
        state: &Arc<tokio::sync::Mutex<ConversationState>>,
    ) -> bool {
        let streak = {
            let mut s = state.lock().await;
            s.record_turn_and_check_loop()
        };
        // Fire after 3 consecutive identical turns.
        if streak < 3 {
            return false;
        }

        // (1) Collapse duplicate turns from history — the accumulated copies
        // are themselves the attractor. The tie-breaker perturbs the next
        // turn, but leaving 12 identical reasoning traces in history keeps
        // pulling the model back.
        let removed = {
            let mut s = state.lock().await;
            s.collapse_loop_turns()
        };

        let roll = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            std::time::SystemTime::now().hash(&mut h);
            state.lock().await.iteration_count.hash(&mut h);
            (h.finish() % 10000) as u32
        };
        let warn = Message::new(
            &self.stream_output_topic,
            &self.id(),
            format!(
                "Cross-turn loop detected: {} consecutive near-identical turns \
                 ({} messages compacted from history). Injecting tie-breaker.",
                streak, removed,
            ),
        )
        .with_type("Warning");
        let _ = bus.publish(&self.id(), warn).await;

        // (2) Tie-breaker injection.
        let inject = format!(
            "You have repeated the same approach {} times across turns and it is not working. \
             You MUST try a DIFFERENT approach this time — not a minor variation of the same one. \
             If you need a tie breaker in choosing a different approach, \
             here is a random number for you: {}. \
             Use it to pick between alternatives you haven't tried yet.",
            streak, roll,
        );
        let _ = bus
            .publish(
                &self.id(),
                Message::new(&self.input_topic, &self.id(), inject),
            )
            .await;

        // Record a surprise for five-whys analysis on context refresh.
        {
            let mut s = state.lock().await;
            s.record_surprise(Surprise::new(
                "cross_turn_loop",
                format!("{} consecutive near-identical turns (streak={})", streak, removed),
            ));
        }
        true
    }
    /// tokens) or got caught in a think-loop by the similarity detector. Saves
    /// the partial response, warns the TUI, asks a standalone `llm_call` what
    /// the model was likely trying to do, and injects the guidance as a user
    /// message to trigger a fresh turn. `reason` describes the trigger for
    /// logging/TUI; `partial` is whatever the stuck stream accumulated.
    async fn triage_stuck_model(
        &self,
        bus: &Bus,
        state: &Arc<tokio::sync::Mutex<ConversationState>>,
        reason: &str,
        partial: String,
    ) {
        {
            let mut s = state.lock().await;
            s.inference_in_flight = false;
            s.was_interrupted = true;
            if !partial.is_empty() {
                s.add_assistant_message(partial.clone());
            }
            // Record a surprise for five-whys analysis on context refresh.
            let kind = if reason.contains("idle") { "idle_timeout" } else { "within_stream_loop" };
            s.record_surprise(
                Surprise::new(kind, format!("triage: {}", reason))
                    .with_trace(&partial),
            );
        }

        let warn = Message::new(
            &self.stream_output_topic,
            &self.id(),
            format!("Model {} — running triage...", reason),
        )
        .with_type("Warning");
        let _ = bus.publish(&self.id(), warn).await;

        if let Some(ref llm_cfg) = self.llm_config {
            let triage_prompt = format!(
                "The coding agent's LLM appeared stuck ({reason}) and was interrupted.\n\n\
                 Here is the accumulated output/reasoning that was repeating (truncated):\n\
                 \"\"\"\n{partial}\n\"\"\"\n\n\
                 The model was going in circles. Identify what it was stuck on, then state the \
                 SINGLE most concrete next action it should take to make progress. \
                 Be specific: name the file, the function, the exact change. \
                 Answer in 2-3 sentences max.",
                reason = reason,
                partial = partial.chars().take(4000).collect::<String>(),
            );
            match gladiator_llm::llm_call(llm_cfg, &triage_prompt).await {
                Ok(guidance) => {
                    let guidance_msg = Message::new(
                        &self.stream_output_topic,
                        &self.id(),
                        format!("Triage guidance: {}", guidance.trim()),
                    )
                    .with_type("Info");
                    let _ = bus.publish(&self.id(), guidance_msg).await;
                    let inject = format!(
                        "The model was stuck ({reason}) and has been interrupted. \
                         Triage guidance: {guidance}\n\n\
                         Please continue based on this guidance. Do NOT repeat your previous reasoning.",
                        reason = reason,
                        guidance = guidance.trim(),
                    );
                    let _ = bus
                        .publish(
                            &self.id(),
                            Message::new(&self.input_topic, &self.id(), inject),
                        )
                        .await;
                }
                Err(e) => {
                    warn!("Triage llm_call failed: {}", e);
                }
            }
        }
    }

    /// After a tool call resolves, check whether the whole batch is now
    /// complete. If so, drain any user messages that arrived mid-execution,
    /// honour the iteration cap, and dispatch the next LLM turn. Shared by the
    /// external-tool result path and the internal-tool inline path so the
    /// follow-up logic stays in exactly one place.
    async fn advance_turn_if_resolved(
        &self,
        bus: &Bus,
        state: &Arc<Mutex<ConversationState>>,
        subagent_stack: &Arc<Mutex<Vec<SubagentFrame>>>,
    ) {
        let mut s = state.lock().await;
        if !s.all_tool_calls_resolved() {
            return;
        }
        let pending = s.drain_pending_messages();
        if !pending.is_empty() {
            s.reset_iteration();
            let depth = s.subagent_depth;
            for m in &pending {
                s.add_user_message(m.clone());
                let mut displayed_msg = Message::new(
                    &self.stream_output_topic,
                    &self.id(),
                    m.clone(),
                )
                .with_type("UserMessageDisplayed");
                if depth > 0 {
                    displayed_msg = displayed_msg.with_depth(depth);
                }
                drop(s);
                let _ = bus.publish(&self.id(), displayed_msg).await;
                s = state.lock().await;
            }
        }
        if s.max_reached(self.max_iterations) {
            let summary = s.recent_messages_summary(10);
            // Record a surprise for five-whys analysis on context refresh.
            s.record_surprise(Surprise::new(
                "max_iterations",
                format!("hit {} iterations limit", self.max_iterations),
            ).with_trace(&summary));
            drop(s);

            // If we're inside a subagent (depth > 0), pop back to parent
            // instead of writing handoff — the inner agent's partial output is
            // returned as call_subagent result.
            let depth = {
                let s = state.lock().await;
                s.subagent_depth
            };
            if depth > 0 && !subagent_stack.lock().await.is_empty() {
                let last_output = {
                    let s = state.lock().await;
                    s.messages.iter().rev()
                        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"))
                        .and_then(|m| m.get("content").and_then(|c| c.as_str()).map(String::from))
                        .unwrap_or_else(|| format!("Subagent hit max_iterations ({})", self.max_iterations))
                };
                warn!(
                    "Agent {}: subagent hit max_iterations ({}), popping with partial result",
                    self.index, self.max_iterations
                );
                let depth_for_warn = {
                    let s = state.lock().await;
                    s.subagent_depth
                };
                let _ = bus.publish(&self.id(), Message::new(
                    &self.stream_output_topic,
                    &self.id(),
                    format!("[subagent] reached max iterations ({}) — returning partial", self.max_iterations),
                ).with_type("Warning").with_depth(depth_for_warn)).await;
                Box::pin(self.pop_subagent(state, subagent_stack, bus, last_output)).await;
                return;
            }

            // Graceful recovery: persist a handoff file so the conversation
            // can be resumed via the restart_from_file internal tool.
            let handoff_path = "tmp/maxiter-handoff.txt";
            if std::fs::create_dir_all("tmp").is_ok() {
                if let Err(e) = std::fs::write(handoff_path, &summary) {
                    warn!("Failed to write max-iter handoff: {}", e);
                }
            }
            let warn_msg = Message::new(
                &self.stream_output_topic,
                &self.id(),
                format!(
                    "Max iterations ({}) reached. Handoff saved to {}. \
                     Use `restart_from_file {{\"path\":\"{}\"}}` to resume, or send a message to continue.",
                    self.max_iterations, handoff_path, handoff_path,
                ),
            )
            .with_type("Warning");
            let _ = bus.publish(&self.id(), warn_msg).await;
        } else {
            // Mark inference as in-flight before sending so user messages
            // arriving during the LLM stream are buffered.
            s.inference_in_flight = true;
            let messages = s.build_messages_with_system(&self.system_message);
            drop(s);
            if let Err(e) = self.send_conversation(bus, &messages).await {
                error!("Failed to send tool results to LLM: {}", e);
            }
        }
    }

    /// Handle an agent-internal tool call (e.g. todo_write/todo_read/
    /// restart_from_file) directly against in-memory state. These never reach a
    /// `ToolActorRunner`, so no execute message is published on the bus.
    ///
    /// The returned `InternalToolOutcome::context_reset` flag signals that the
    /// handler rebuilt the conversation from scratch (only `restart_from_file`
    /// does this): the dispatch loop must then skip appending a tool result,
    /// since the assistant tool_calls message it would answer has been wiped.
    async fn handle_internal_tool(
        &self,
        name: &str,
        args: &serde_json::Value,
        state: &Arc<Mutex<ConversationState>>,
        subagent_stack: &Arc<Mutex<Vec<SubagentFrame>>>,
        bus: &Bus,
    ) -> crate::internal_tools::InternalToolOutcome {
        use crate::internal_tools as it;
        match name {
            "todo_write" => {
                let raw_todos = match args.get("todos").and_then(|t| t.as_array()) {
                    Some(arr) => arr,
                    None => return InternalToolOutcome::err("Missing 'todos' array"),
                };
                let mut entries = Vec::with_capacity(raw_todos.len());
                for (i, raw) in raw_todos.iter().enumerate() {
                    match it::TodoEntry::from_json(raw) {
                        Ok(e) => entries.push(e),
                        Err(e) => {
                            return InternalToolOutcome::err(format!("todos[{}]: {}", i, e))
                        }
                    }
                }
                // Enforce at most one in_progress item (matches the contract
                // advertised in the tool description); coerce extras to pending.
                let mut seen_in_progress = false;
                for e in &mut entries {
                    if e.status == it::TodoStatus::InProgress {
                        if seen_in_progress {
                            e.status = it::TodoStatus::Pending;
                        } else {
                            seen_in_progress = true;
                        }
                    }
                }
                let summary = {
                    let mut s = state.lock().await;
                    s.set_todos(entries)
                };
                info!("Agent {} updated todos:\n{}", self.index, summary);
                InternalToolOutcome::ok(summary)
            }
            "todo_read" => {
                let s = state.lock().await;
                let mut out = s.todos_render();
                // Append live context-usage so the model can see how much
                // budget remains when deciding whether to restart_from_file.
                out.push_str(&format!("\n{}", s.context_status_line()));
                InternalToolOutcome::ok(out)
            }
            "restart_from_file" => self.handle_restart_from_file(args, state).await,
            "call_subagent" => {
                self.handle_call_subagent(args, &state, subagent_stack, bus)
                    .await
            }
            "set_context_reminder" => {
                let threshold = args.get("threshold_tokens")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| InternalToolOutcome::err("Missing 'threshold_tokens'"));
                let message = match args.get("message").and_then(|m| m.as_str()) {
                    Some(s) => s.to_string(),
                    None => return InternalToolOutcome::err("Missing 'message'"),
                };
                match threshold {
                    Ok(thresh) => {
                        let mut s = state.lock().await;
                        s.add_context_reminder(thresh, message);
                        debug!("Agent {}: set context reminder at {} tokens", self.index, thresh);
                        InternalToolOutcome::ok(format!(
                            "Context reminder set: will inject when usage exceeds {} tokens.",
                            thresh
                        ))
                    }
                    Err(e) => e,
                }
            }
            "schedule_wake_up" => {
                let delay = args.get("delay_seconds")
                    .and_then(|v| v.as_u64());
                let message = match args.get("message").and_then(|m| m.as_str()) {
                    Some(s) => s.to_string(),
                    None => return InternalToolOutcome::err("Missing 'message'"),
                };
                match delay {
                    Some(secs) => {
                        let interval = args.get("interval_seconds")
                            .and_then(|v| v.as_u64());
                        let mut s = state.lock().await;
                        if let Some(interval_secs) = interval {
                            s.add_cron_wake_up(secs, interval_secs, message);
                            debug!("Agent {}: scheduled cron wake-up in {}s (every {}s)",
                                self.index, secs, interval_secs);
                            InternalToolOutcome::ok(format!(
                                "Cron wake-up scheduled: fires in {}s, repeats every {}s.",
                                secs, interval_secs
                            ))
                        } else {
                            s.add_one_shot_wake_up(secs, message);
                            debug!("Agent {}: scheduled one-shot wake-up in {}s", self.index, secs);
                            InternalToolOutcome::ok(format!(
                                "One-shot wake-up scheduled: fires in {} seconds.",
                                secs
                            ))
                        }
                    }
                    None => InternalToolOutcome::err("Missing 'delay_seconds'"),
                }
            }
            _ => InternalToolOutcome::err(format!("Unknown internal tool: {}", name)),
        }
    }

    /// Back up the live `ConversationState` to `/tmp/<pid>-<datetime>.json`,
    /// wipe the context, and inject the file's contents (wrapped with a
    /// continuation directive) as a fresh user instruction. Failure to back up
    /// or read the file aborts the restart so no context is lost.
    async fn handle_restart_from_file(
        &self,
        args: &serde_json::Value,
        state: &Arc<tokio::sync::Mutex<ConversationState>>,
    ) -> crate::internal_tools::InternalToolOutcome {
        use crate::internal_tools as it;

        let filename = match args.get("filename").and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => s.trim().to_string(),
            _ => return InternalToolOutcome::err("Missing 'filename' parameter"),
        };

        let resolved = resolve_against_working_dir(&filename, &self.config.working_dir);

        let content = match std::fs::read_to_string(&resolved) {
            Ok(c) => c,
            Err(e) => {
                return InternalToolOutcome::err(format!(
                    "Failed to read restart file '{}': {}",
                    resolved, e
                ))
            }
        };

        let pid = std::process::id();
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backup_name = it::backup_filename(pid, epoch);
        let backup_path = format!("/tmp/{}", backup_name);

        // Serialize the snapshot BEFORE taking the lock we clear under, so a
        // serialization failure also aborts without mutating anything.
        let snapshot = {
            let s = state.lock().await;
            match serde_json::to_string_pretty(&*s) {
                Ok(json) => json,
                Err(e) => {
                    return InternalToolOutcome::err(format!(
                        "Failed to serialize context backup: {}",
                        e
                    ))
                }
            }
        };

        if let Err(e) = std::fs::write(&backup_path, &snapshot) {
            return InternalToolOutcome::err(format!(
                "Failed to write context backup to '{}': {}",
                backup_path, e
            ));
        }
        warn!(
            "Agent {}: restart_from_file backing up context to {}",
            self.index, backup_path
        );

        // Before clearing the conversation state, drain all recorded surprises,
        // write them to tmp/surprises.md, and spawn async five-whys analyses.
        let surprises = {
            let mut s = state.lock().await;
            s.take_surprises()
        };
        if !surprises.is_empty() {
            let md_content = crate::five_whys::write_surprises_md(&surprises);
            let _ = std::fs::create_dir_all("tmp");
            let _ = std::fs::write("tmp/surprises.md", &md_content);
            info!(
                "Agent {}: wrote tmp/surprises.md ({} incidents) before context refresh",
                self.index,
                surprises.len()
            );

            // Spawn five-whys analyses per spec in tmp/five-whys.md.
            if let Some(ref llm_cfg) = self.llm_config {
                for surprise in &surprises {
                    let report = IncidentReport::from_surprise(surprise, &[]);
                    crate::five_whys::run_five_whys(llm_cfg, report);
                }
            } else {
                warn!(
                    "Agent {}: cannot run five-whys analysis — no llm_config available",
                    self.index
                );
            }
        }

        let instruction = it::build_restart_instruction(&content);
        {
            let mut s = state.lock().await;
            s.clear_for_restart();
            s.add_user_message(instruction.clone());
            // Fresh turn budget for the restarted conversation.
            s.reset_iteration();
        }
        info!(
            "Agent {}: context restarted from '{}', backup at '{}'",
            self.index, resolved, backup_path
        );

        // context_reset = true: the dispatch loop must not append a tool
        // result, since the assistant tool_calls message was just wiped.
        InternalToolOutcome::ok(format!(
            "Context backed up to {} and cleared. Restarted from '{}'.",
            backup_path, resolved
        ))
        .with_reset(format!(
            "Restarted from '{}'. Backup: {}. Continuing with injected instructions.",
            resolved, backup_path
        ))
    }

    /// Push current context onto subagent stack, clear state, and inject the
    /// task text as a fresh user message. The parent's system_message is saved
    /// in the frame so it can be restored on pop; `active_system_message` is
    /// set to the subagent prompt (or cleared if none provided). Returns an
    /// outcome with context_reset=true so the dispatch loop skips appending a
    /// tool result — advance_turn_if_resolved will send the inner conversation.
    async fn handle_call_subagent(
        &self,
        args: &serde_json::Value,
        state: &Arc<Mutex<ConversationState>>,
        subagent_stack: &Arc<Mutex<Vec<SubagentFrame>>>,
        bus: &Bus,
    ) -> InternalToolOutcome {
        let task = match args.get("task").and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => s.to_string(),
            _ => return InternalToolOutcome::err("Missing 'task' parameter"),
        };

        // Optional subagent system prompt. If not provided, the parent's
        // default system_message is inherited (active_system_message = None).
        let sub_prompt: Option<String> = args
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        {
            let mut stack = subagent_stack.lock().await;
            // Save the parent context. We snapshot messages + iteration_count +
            // todos etc by cloning the entire ConversationState (transient
            // fields like reasoning/partial are skipped since they're empty at
            // tool-call dispatch time).
            let frame = {
                let s = state.lock().await;
                SubagentFrame {
                    saved_state: (*s).clone(),
                    saved_system_message: self.system_message.clone(),
                }
            };
            stack.push(frame);
        }

        // Clear the shared conversation state for the subagent's fresh context.
        {
            let mut s = state.lock().await;
            s.clear_for_restart();
            s.subagent_depth += 1;
            if let Some(ref prompt) = sub_prompt {
                s.active_system_message = Some(prompt.clone());
            } else {
                // Inherit parent system message (no override).
                s.active_system_message = None;
            }
        }

        info!(
            "Agent {}: call_subagent pushed frame, depth={}",
            self.index,
            subagent_stack.lock().await.len()
        );

        let depth_for_start = {
            let s = state.lock().await;
            s.subagent_depth
        };
        let indent_msg = Message::new(
            &self.stream_output_topic,
            &self.id(),
            format!("[subagent] starting: {}", task),
        )
        .with_type("SubagentStart")
        .with_depth(depth_for_start);
        let _ = bus.publish(&self.id(), indent_msg).await;

        // Inject the task as a fresh user message. advance_turn_if_resolved
        // will pick it up and send to LLM.
        {
            let mut s = state.lock().await;
            s.add_user_message(task.clone());
            s.reset_iteration();
            s.inference_in_flight = true;
        }

        InternalToolOutcome::ok(format!("Subagent started: {}", task))
            .with_reset("Subagent context initialized.")
    }

    /// Pop the subagent stack and restore parent context. Called when inner
    /// conversation completes (assistant text response, no pending tools).
    /// The `result_text` is the assistant's final output from the inner turn,
    /// which becomes the tool result for call_subagent in the restored parent.
    async fn pop_subagent(
        &self,
        state: &Arc<Mutex<ConversationState>>,
        subagent_stack: &Arc<Mutex<Vec<SubagentFrame>>>,
        bus: &Bus,
        result_text: String,
    ) {
        let frame = {
            let mut stack = subagent_stack.lock().await;
            if stack.is_empty() {
                warn!("Agent {}: pop_subagent called but stack is empty", self.index);
                return;
            }
            stack.pop().unwrap()
        };

        // Restore parent state. The saved_state already contains the assistant
        // message with tool_calls=[{id:"call-X", function:{name:"call_subagent"}}]
        // and pending_tool_calls={"call-X"}. We need to find that id so we can
        // add a matching tool result.
        let depth_after;
        let mut subagent_tc_id: Option<String> = None;
        {
            let mut s = state.lock().await;
            *s = frame.saved_state.clone();
            s.subagent_depth = s.subagent_depth.saturating_sub(1);
            depth_after = s.subagent_depth;

            // Find the call_subagent's tool_call_id and stash it for the
            // display message published after this block.
            for msg in &s.messages {
                if msg.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                    if let Some(tcs) = msg.get("tool_calls").and_then(|t| t.as_array()) {
                        for tc in tcs {
                            let name = tc["function"]["name"].as_str().unwrap_or("");
                            if name == "call_subagent" {
                                subagent_tc_id = Some(
                                    tc["id"].as_str().unwrap_or("").to_string()
                                );
                            }
                        }
                    }
                }
            }

            match subagent_tc_id {
                Some(ref id) => {
                    s.add_tool_result(id, "call_subagent", &result_text, true);
                    s.resolve_tool_call(id);
                }
                None => {
                    warn!("Agent {}: pop_subagent could not find call_subagent tool_call_id in restored state — result will be lost",
                        self.index);
                }
            }

            // Clear the active_system_message override since we're back to parent.
            s.active_system_message = None;
        }

        // Publish a display message so the TUI can coalesce the result into
        // the [tool] placeholder for call_subagent.
        if let Some(id) = subagent_tc_id.as_ref() {
            let display = format!(
                "  [tool_result] call_subagent({}) => {}",
                id,
                result_text.chars().take(500).collect::<String>()
            );
            let depth_for_stamp = depth_after;
            let display_msg = Message::new(
                &self.stream_output_topic,
                &self.id(),
                display,
            ).with_type("LlmToolResult");
            let display_msg = self.stamp_depth_sync(display_msg, depth_for_stamp);
            let _ = bus.publish(&self.id(), display_msg).await;
        }

        info!(
            "Agent {}: subagent popped, depth={}, result length={}",
            self.index,
            depth_after,
            result_text.len()
        );

        // depth_after is the parent's depth (0 for top-level). The completed
        // message belongs to the inner level that just finished, so stamp with
        // depth_after + 1.
        let indent_msg = Message::new(
            &self.stream_output_topic,
            &self.id(),
            format!("[subagent] completed: {}", result_text.chars().take(200).collect::<String>()),
        )
        .with_type("SubagentEnd")
        .with_depth(depth_after + 1);
        let _ = bus.publish(&self.id(), indent_msg).await;

        // advance_turn_if_resolved will send the restored parent conversation
        // to the LLM now that all tool calls (including call_subagent) are resolved.
        self.advance_turn_if_resolved(bus, state, subagent_stack).await;
    }

    async fn send_conversation(
        &self,
        bus: &Bus,
        messages: &[serde_json::Value],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Signal the TUI that inference is starting (prefill phase). This
        // lets the spinner show "Thinking..." before any LlmStream token
        // arrives, so the user does not see a frozen 'gladiator ready'.
        let prefill_msg = Message::new(
            &self.stream_output_topic,
            &self.id(),
            "request_sent",
        ).with_type("LlmRequestSent");
        let _ = bus.publish(&self.id(), prefill_msg).await;

        let llm_request = LlmRequest {
            messages: Some(messages.to_vec()),
            prompt: String::new(),
            config: None,
            tools: if self.tool_defs.is_empty() {
                None
            } else {
                Some(self.tool_defs.clone())
            },
            grammar: None,
        };

        let msg = Message::new(
            &self.llm_in_topic,
            &self.id(),
            serde_json::to_value(&llm_request)
                .map_err(|e| format!("Failed to serialize LLM request: {}", e))?,
        );

        let mut attempt = 0u32;
        loop {
            match bus.publish(&self.id(), msg.clone()).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if attempt < 3 {
                        error!("Failed to publish to LLM input (attempt {}): {}", attempt + 1, e);
                        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                        attempt += 1;
                    } else {
                        return Err(format!("Failed to publish to LLM input after 3 attempts: {}", e).into());
                    }
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl Actor for AgentActor {
    fn id(&self) -> gladiator_core::ActorId {
        format!("gladiator-agent-{}", self.index)
    }

    fn announce(&self) -> ActorAnnouncement {
        let mut subs = vec![
            self.input_topic.clone(),
            self.llm_out_topic.clone(),
            self.llm_stream_topic.clone(),
            self.llm_tool_calls_topic.clone(),
            self.tool_results_topic.clone(),
        ];
        let mut pubs = vec![self.stream_output_topic.clone(), self.llm_in_topic.clone()];
        if !self.state_control_topic.is_empty() {
            subs.push(self.state_control_topic.clone());
        }
        if !self.llm_stats_topic.is_empty() {
            subs.push(self.llm_stats_topic.clone());
        }
        if !self.state_topic.is_empty() {
            pubs.push(self.state_topic.clone());
        }
        ActorAnnouncement {
            id: self.id(),
            subscriptions: subs,
            publications: pubs,
        }
    }

    async fn run(&self, bus: &Bus) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let state: Arc<Mutex<ConversationState>> =
            Arc::new(Mutex::new(ConversationState::new()));

        // Subagent stack — each frame holds the parent's ConversationState +
        // system_message snapshot, saved on call_subagent push and restored
        // when the inner conversation completes. Depth = stack.len().
        let subagent_stack: Arc<Mutex<Vec<SubagentFrame>>> =
            Arc::new(Mutex::new(Vec::new()));

        // Seed the discovered context window so the agent can report it before
        // the first StreamStats message arrives.
        if let Some(w) = self.context_window {
            state.lock().await.context_window = Some(w);
        }

        let mut input_rx = bus.subscribe(&self.id(), &self.input_topic).await?;
        let mut out_rx = bus.subscribe(&self.id(), &self.llm_out_topic).await?;
        let mut stream_rx = bus.subscribe(&self.id(), &self.llm_stream_topic).await?;
        let mut tool_calls_rx = bus.subscribe(&self.id(), &self.llm_tool_calls_topic).await?;
        let mut tool_results_rx = bus.subscribe(&self.id(), &self.tool_results_topic).await?;

        let mut state_control_rx: Option<tokio::sync::broadcast::Receiver<gladiator_core::Message>> =
            if !self.state_control_topic.is_empty() {
                Some(bus.subscribe(&self.id(), &self.state_control_topic).await?)
            } else {
                None
            };

        // Optional stats subscription for per-turn usage + context-window.
        let mut stats_rx: Option<tokio::sync::broadcast::Receiver<gladiator_core::Message>> =
            if !self.llm_stats_topic.is_empty() {
                Some(bus.subscribe(&self.id(), &self.llm_stats_topic).await?)
            } else {
                None
            };

        let mut tool_watchdog = tokio::time::interval(std::time::Duration::from_secs(10));
        // Wake-up check interval: every 5 seconds, scan for due wake-ups.
        let mut wake_up_timer = tokio::time::interval(std::time::Duration::from_secs(5));

        info!(
            "Agent actor {} listening on '{}' with {} tools, max_iterations={}",
            self.index,
            self.input_topic,
            self.tool_defs.len(),
            self.max_iterations
        );

        loop {
            tokio::select! {
                result = input_rx.recv() => {
                    match result {
                        Ok(msg) => {
                            let user_message = msg.payload_str().unwrap_or_else(|| msg.payload.to_string());
                            info!("Agent {} received user input: {}", self.index, user_message);

                            {
                                let mut s = state.lock().await;
                                if !s.pending_tool_calls.is_empty() || s.inference_in_flight {
                                    s.buffer_user_message(user_message.clone());
                                    drop(s);
                                    // Notify TUI that this message is queued (pending)
                                    let queued_msg = Message::new(
                                        &self.stream_output_topic,
                                        &self.id(),
                                        user_message,
                                    ).with_type("UserMessageQueued");
                                    let _ = bus.publish(&self.id(), queued_msg).await;
                                    continue;
                                }
                                s.reset_iteration();
                                if s.was_interrupted {
                                    s.merge_user_message(user_message.clone());
                                    s.was_interrupted = false;
                                } else {
                                    s.add_user_message(user_message.clone());
                                }
                                // Mark inference as in-flight so user messages
                                // arriving while the LLM is streaming are buffered.
                                s.inference_in_flight = true;
                                drop(s);
                                // Notify TUI that this message is now displayed in the chat
                                let displayed_msg = Message::new(
                                    &self.stream_output_topic,
                                    &self.id(),
                                    user_message,
                                ).with_type("UserMessageDisplayed");
                                let _ = bus.publish(&self.id(), displayed_msg).await;
                            }

                            let messages = {
                                let s = state.lock().await;
                                s.build_messages_with_system(&self.system_message)
                            };

                            if let Err(e) = self.send_conversation(bus, &messages).await {
                                error!("Failed to send conversation: {}", e);
                            }
                        }
                        Err(RecvError::Lagged(n)) => warn!("Agent {} input lagged: {}", self.index, n),
                        Err(RecvError::Closed) => break,
                    }
                }
                result = out_rx.recv() => {
                    match result {
                        Ok(msg) => {
                            let output = msg.payload_str().unwrap_or_else(|| msg.payload.to_string());
                            let msg_type = msg.meta_type().unwrap_or_default().to_string();
                            debug!("Agent {} received LLM output (type={}): {}", self.index, msg_type, output);

                            // Stuck-model detection: the LLM either went idle
                            // (>90s no tokens) or got caught repeating itself
                            // (think-loop, caught by similarity detection).
                            // Both signals trigger triage via a standalone
                            // llm_call + guidance injection.
                            if msg_type == "LlmIdleTimeout" || msg_type == "LlmStuckLoop" {
                                let reason = if msg_type == "LlmIdleTimeout" {
                                    "went idle (>90s no tokens)"
                                } else {
                                    "stuck in a think-loop (repeated output)"
                                };
                                self.triage_stuck_model(
                                    bus,
                                    &state,
                                    reason,
                                    output.clone(),
                                )
                                .await;
                            } else if output.starts_with("Interrupted:") {
                                let mut s = state.lock().await;
                                s.was_interrupted = true;
                                s.inference_in_flight = false;
                                s.clear_reasoning();
                                // Drain pending user messages so they don't
                                // accumulate and confuse the agent on next turn.
                                // The interrupt means the user wants to change
                                // direction, not continue with queued messages.
                                let _drained: Vec<String> =
                                    std::mem::take(&mut s.pending_messages);
                                // Preserve partial assistant text that was streamed before the interrupt
                                if let Some(partial) = s.drain_partial_response() {
                                    s.add_assistant_message(partial);
                                }
                                drop(s);
                                // Forward to TUI as a warning so the user sees the interrupt
                                let warn_msg = Message::new(
                                    &self.stream_output_topic,
                                    &self.id(),
                                    output.clone(),
                                ).with_type("Warning");
                                let _ = bus.publish(&self.id(), warn_msg).await;
                            } else {
                                let output_for_pop = output.clone();
                                {
                                    let mut s = state.lock().await;
                                    // Inference is complete — clear the in-flight flag
                                    // so user messages are no longer buffered.
                                    s.inference_in_flight = false;
                                    s.add_assistant_message(output);
                                    s.increment_iteration();
                                }

                                // Subagent completion detection: when inner
                                // conversation produces a text-only response (no pending tool calls) and we're at depth > 0, pop back to parent.
                                if !subagent_stack.lock().await.is_empty() {
                                    // Before popping, check if the user sent
                                    // messages to steer the subagent. If so,
                                    // drain them into the subagent's
                                    // conversation and continue for another
                                    // turn instead of popping.
                                    let pending = {
                                        let mut s = state.lock().await;
                                        s.drain_pending_messages()
                                    };
                                    if !pending.is_empty() {
                                        // Inject pending messages into the
                                        // subagent and send next LLM turn.
                                        let depth = {
                                            let mut s = state.lock().await;
                                            for m in &pending {
                                                s.add_user_message(m.clone());
                                            }
                                            let depth = s.subagent_depth;
                                            let displayed = Message::new(
                                                &self.stream_output_topic,
                                                &self.id(),
                                                pending.last().unwrap().clone(),
                                            ).with_type("UserMessageDisplayed");
                                            let displayed = self.stamp_depth_sync(displayed, depth);
                                            drop(s);
                                            let _ = bus.publish(&self.id(), displayed).await;
                                            depth
                                        };
                                        info!("Agent {}: subagent at depth {} received {} pending user message(s), continuing turn",
                                              self.index, depth, pending.len());
                                        let messages = {
                                            let s = state.lock().await;
                                            s.build_messages_with_system(&self.system_message)
                                        };
                                        {
                                            let mut s = state.lock().await;
                                            s.inference_in_flight = true;
                                        }
                                        let _ = self.send_conversation(bus, &messages).await;
                                    } else {
                                        let should_pop = {
                                            let s = state.lock().await;
                                            s.subagent_depth > 0 && !s.inference_in_flight && s.all_tool_calls_resolved()
                                        };
                                        if should_pop {
                                            self.pop_subagent(&state, &subagent_stack, bus,
                                                output_for_pop).await;
                                        } else {
                                            // Cross-turn loop detection (tie-breaker injection)
                                            let _ = self.maybe_break_cross_turn_loop(bus, &state).await;
                                        }
                                    }
                                } else {
                                    // Depth 0 (no subagent): drain pending messages
                                    // the same way — after a text-only response,
                                    // user messages that arrived during inference
                                    // should start a new turn.
                                    let pending = {
                                        let mut s = state.lock().await;
                                        s.drain_pending_messages()
                                    };
                                    if !pending.is_empty() {
                                        for m in &pending {
                                            let mut s = state.lock().await;
                                            s.add_user_message(m.clone());
                                            drop(s);
                                            let displayed = Message::new(
                                                &self.stream_output_topic,
                                                &self.id(),
                                                m.clone(),
                                            ).with_type("UserMessageDisplayed");
                                            let _ = bus.publish(&self.id(), displayed).await;
                                        }
                                        info!("Agent {}: {} pending user message(s) delivered after text response",
                                              self.index, pending.len());
                                        let messages = {
                                            let s = state.lock().await;
                                            s.build_messages_with_system(&self.system_message)
                                        };
                                        {
                                            let mut s = state.lock().await;
                                            s.inference_in_flight = true;
                                        }
                                        let _ = self.send_conversation(bus, &messages).await;
                                    } else {
                                        // Cross-turn loop detection (tie-breaker injection)
                                        let _ = self.maybe_break_cross_turn_loop(bus, &state).await;
                                    }
                                }
                            }
                        }
                        Err(RecvError::Lagged(n)) => warn!("Agent {} output lagged: {}", self.index, n),
                        Err(RecvError::Closed) => break,
                    }
                }
                result = stream_rx.recv() => {
                    match result {
                        Ok(msg) => {
                            let msg_type = msg.meta_type().unwrap_or_default().to_string();
                            // Accumulate reasoning chunks for save/load
                            if msg_type == "LlmThinking" {
                                let chunk = msg.payload_str().unwrap_or_default();
                                if !chunk.is_empty() {
                                    let mut s = state.lock().await;
                                    s.append_reasoning(&chunk);
                                }
                            } else if msg_type == "LlmStream" {
                                let chunk = msg.payload_str().unwrap_or_default();
                                if !chunk.is_empty() {
                                    let mut s = state.lock().await;
                                    s.append_partial_response(&chunk);
                                }
                            }
                            let preview = msg.payload_str().unwrap_or_default();
                            let preview = if preview.chars().count() > 60 { let truncated: String = preview.chars().take(60).collect(); format!("{}...", truncated) } else { preview };
                            debug!("Agent {} forwarding stream ({}) to {}: {}", self.index, msg_type, self.stream_output_topic, preview);
                            let mut forwarded = msg.clone();
                            forwarded.topic = self.stream_output_topic.clone();
                            // Stamp subagent depth so the TUI can indent nested output.
                            {
                                let s = state.lock().await;
                                if s.subagent_depth > 0 {
                                    forwarded = forwarded.with_depth(s.subagent_depth);
                                }
                            }
                            let _ = bus.publish(&self.id(), forwarded).await;
                        }
                        Err(RecvError::Lagged(n)) => warn!("Agent {} stream lagged: {}", self.index, n),
                        Err(RecvError::Closed) => break,
                    }
                }
                result = tool_calls_rx.recv() => {
                    match result {
                        Ok(msg) => {
                            let tool_calls: Vec<serde_json::Value> = match &msg.payload {
                                serde_json::Value::Array(arr) => arr.clone(),
                                serde_json::Value::Object(_) => continue,
                                _ => continue,
                            };

                            {
                                let mut s = state.lock().await;
                                // The LLM responded with tool calls — inference is
                                // no longer in flight, but pending_tool_calls now
                                // gates further buffering.
                                s.inference_in_flight = false;
                                s.add_tool_calls(tool_calls.clone());
                                s.increment_iteration();
                            }
                            // Cross-turn loop detection (tie-breaker injection)
                            let _ = self.maybe_break_cross_turn_loop(bus, &state).await;

                            for (i, tc) in tool_calls.iter().enumerate() {
                                debug!("[agent] tool_call[{}]: {:?}", i, tc);
                                let tool_call_id = {
                                    let raw = tc["id"].as_str().unwrap_or("");
                                    if raw.is_empty() {
                                        format!("__idx_{}", i)
                                    } else {
                                        raw.to_string()
                                    }
                                };
                                let func_name = tc["function"]["name"].as_str().unwrap_or("").to_string();
                                let func_args_str = match tc["function"]["arguments"].as_str() {
                                    Some(s) => s.to_string(),
                                    None => tc["function"]["arguments"].to_string(),
                                };
                                debug!("[agent] func_name={}, func_args_str={}", func_name, func_args_str);

                                let args: serde_json::Value = match serde_json::from_str(&func_args_str) {
                                    Ok(a) => a,
                                    Err(e) => {
                                        error!("Failed to parse tool args for {}: {}", func_name, e);
                                        let mut s = state.lock().await;
                                        s.add_tool_result(&tool_call_id, &func_name, format!("Error parsing arguments: {}", e), false);
                                        s.resolve_tool_call(&tool_call_id);
                                        continue;
                                    }
                                };

                                // Agent-internal tools (todo_write/todo_read/
                                // restart_from_file) are handled inline against
                                // ConversationState — they never go to a
                                // ToolActorRunner, so no execute message is
                                // published on the bus.
                                if crate::internal_tools::is_internal_tool(&func_name) {
                                    info!("Agent {} handling internal tool: {}", self.index, func_name);
                                    // Publish structured JSON so the TUI can match
                                    // this dispatch back to the streamed [tool]
                                    // placeholder by id.
                                    let tool_status = Message::new(
                                        &self.stream_output_topic,
                                        &self.id(),
                                        serde_json::json!({
                                            "id": tool_call_id,
                                            "name": func_name,
                                            "text": format!("Calling tool: {}({})", func_name, func_args_str),
                                        }),
                                    ).with_type("Info");
                                    let depth = state.lock().await.subagent_depth;
                                    let tool_status = self.stamp_depth_sync(tool_status, depth);
                                    let _ = bus.publish(&self.id(), tool_status).await;

                                    let outcome =
                                        self.handle_internal_tool(&func_name, &args, &state, &subagent_stack, bus).await;
                                    let success = outcome.success;
                                    let display_snapshot = outcome.result_text.clone();

                                    {
                                        let mut s = state.lock().await;
                                        if outcome.context_reset {
                                            // restart_from_file rebuilt the whole transcript:
                                            // appending a tool result here would answer a
                                            // tool_calls message that no longer exists, so
                                            // only resolve (no-op on the cleared pending set)
                                            // and let advance_turn_if_resolved send the fresh
                                            // conversation to the LLM.
                                            s.resolve_tool_call(&tool_call_id);
                                        } else {
                                            s.add_tool_result(
                                                &tool_call_id,
                                                &func_name,
                                                outcome.result_text,
                                                success,
                                            );
                                            s.resolve_tool_call(&tool_call_id);
                                        }
                                    }
                                    let stream_msg = Message::new(
                                        &self.stream_output_topic,
                                        &self.id(),
                                        format!("  [tool_{}] {}({}) => {}",
                                            if success { "result" } else { "error" },
                                            func_name,
                                            tool_call_id,
                                            display_snapshot,
                                        ),
                                    ).with_type("LlmToolResult");
                                    let depth = state.lock().await.subagent_depth;
                                    let stream_msg = self.stamp_depth_sync(stream_msg, depth);
                                    let _ = bus.publish(&self.id(), stream_msg).await;

                                    self.advance_turn_if_resolved(bus, &state, &subagent_stack).await;
                                    continue;
                                }

                                info!("Agent {} dispatching tool call: {}({})", self.index, func_name, func_args_str);

                                // Publish tool call status to TUI
                                // Publish structured JSON so the TUI can match
                                // this dispatch back to the streamed [tool]
                                // placeholder by id.
                                let tool_status = Message::new(
                                    &self.stream_output_topic,
                                    &self.id(),
                                    serde_json::json!({
                                        "id": tool_call_id,
                                        "name": func_name,
                                        "text": format!("Calling tool: {}({})", func_name, func_args_str),
                                    }),
                                ).with_type("Info");
                                let depth = state.lock().await.subagent_depth;
                                let tool_status = self.stamp_depth_sync(tool_status, depth);
                                let _ = bus.publish(&self.id(), tool_status).await;

                                let exec_payload = serde_json::json!({
                                    "tool_call_id": tool_call_id,
                                    "tool_name": func_name,
                                    "arguments": args,
                                });

                                let exec_msg = Message::new(
                                    &format!("tool:{}:execute", func_name),
                                    &self.id(),
                                    exec_payload,
                                );
                                let _ = bus.publish(&self.id(), exec_msg).await;
                            }
                        }
                        Err(RecvError::Lagged(n)) => warn!("Agent {} tool_calls lagged: {}", self.index, n),
                        Err(RecvError::Closed) => break,
                    }
                }
                result = tool_results_rx.recv() => {
                    match result {
                        Ok(msg) => {
                            let tool_result: gladiator_tools::ToolResultMessage = match serde_json::from_value(msg.payload) {
                                Ok(tr) => tr,
                                Err(e) => {
                                    error!("Failed to parse tool result: {}", e);
                                    continue;
                                }
                            };

                            {
                                let mut s = state.lock().await;
                                s.add_tool_result(
                                    &tool_result.tool_call_id,
                                    &tool_result.tool_name,
                                    &tool_result.result,
                                    tool_result.success,
                                );
                                s.resolve_tool_call(&tool_result.tool_call_id);
                            }

                            let result_text = if tool_result.success {
                                tool_result.result.as_str()
                            } else {
                                tool_result.error.as_deref().unwrap_or("unknown")
                            };
                            let stream_msg = Message::new(
                                &self.stream_output_topic,
                                &self.id(),
                                format!("  [tool_{}] {}({}) => {}",
                                    if tool_result.success { "result" } else { "error" },
                                    tool_result.tool_name,
                                    tool_result.tool_call_id,
                                    result_text
                                ),
                            ).with_type("LlmToolResult");
                            let depth = state.lock().await.subagent_depth;
                            let stream_msg = self.stamp_depth_sync(stream_msg, depth);
                            let _ = bus.publish(&self.id(), stream_msg).await;

                            self.advance_turn_if_resolved(bus, &state, &subagent_stack).await;
                        }
                        Err(RecvError::Lagged(n)) => warn!("Agent {} tool_results lagged: {}", self.index, n),
                        Err(RecvError::Closed) => break,
                    }
                }
                result = async {
                    match &mut state_control_rx {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match result {
                        Ok(msg) => {
                            let cmd_agent_id = msg.payload.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
                            if cmd_agent_id != self.id() {
                                continue;
                            }
                            let cmd_type = msg.payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            match cmd_type {
                                "dump_state" => {
                                    let state_json = {
                                        let s = state.lock().await;
                                        serde_json::to_value(&*s)
                                    };
                                    match state_json {
                                        Ok(json) => {
                                            if !self.state_topic.is_empty() {
                                                let msg = Message::new(
                                                    &self.state_topic,
                                                    &self.id(),
                                                    serde_json::json!({"agent_id": self.id(), "state": json}),
                                                );
                                                let _ = bus.publish(&self.id(), msg).await;
                                            }
                                        }
                                        Err(e) => {
                                            error!("Agent {}: failed to serialize state: {}", self.index, e);
                                        }
                                    }
                                }
                                "load_state" => {
                                    let state_json = msg.payload.get("state").cloned().unwrap_or(serde_json::Value::Null);
                                    match serde_json::from_value::<ConversationState>(state_json) {
                                        Ok(new_state) => {
                                            let messages = {
                                                let mut s = state.lock().await;
                                                *s = new_state;
                                                s.messages.clone()
                                            };
                                            // Publish replay so TUI can reconstruct display
                                            let replay_msg = Message::new(
                                                &self.stream_output_topic,
                                                &self.id(),
                                                serde_json::json!({"messages": messages}),
                                            ).with_type("StateReplay");
                                            let _ = bus.publish(&self.id(), replay_msg).await;
                                            let info_msg = Message::new(
                                                &self.stream_output_topic,
                                                &self.id(),
                                                "State loaded successfully",
                                            ).with_type("Info");
                                            let _ = bus.publish(&self.id(), info_msg).await;
                                        }
                                        Err(e) => {
                                            let err_msg = Message::new(
                                                &self.stream_output_topic,
                                                &self.id(),
                                                format!("Failed to load state: {}", e),
                                            ).with_type("Error");
                                            let _ = bus.publish(&self.id(), err_msg).await;
                                        }
                                    }
                                }
                                "retrieve_pending" => {
                                    // Drain pending user messages from the
                                    // conversation state and send them back to
                                    // the TUI as a single joined string so the
                                    // user can edit and resubmit. Clears the
                                    // agent-side pending list atomically.
                                    let drained = {
                                        let mut s = state.lock().await;
                                        s.drain_pending_messages()
                                    };
                                    let text = drained.join("\n");
                                    let msg = Message::new(
                                        &self.stream_output_topic,
                                        &self.id(),
                                        serde_json::json!({"text": text}),
                                    ).with_type("RetrievedPending");
                                    let _ = bus.publish(&self.id(), msg).await;
                                }
                                _ => {}
                            }
                        }
                        Err(RecvError::Lagged(_)) => {}
                        Err(RecvError::Closed) => break,
                    }
                }
                _ = tool_watchdog.tick() => {
                    let s = state.lock().await;
                    if !s.pending_tool_calls.is_empty() {
                        warn!("Agent {} has {} pending tool calls (watchdog tick)", self.index, s.pending_tool_calls.len());
                    }
                }
                result = async {
                    match &mut stats_rx {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match result {
                        Ok(msg) => {
                            // StreamStats carries per-turn usage + context_window.
                            // Record into state so todo_read / context_status_line
                            // reflect the latest numbers, and warn proactively
                            // when remaining budget drops below 20%.
                            let usage = crate::state::Usage {
                                input_tokens: msg.payload.get("usage")
                                    .and_then(|u| u.get("input_tokens")).and_then(|v| v.as_u64()),
                                output_tokens: msg.payload.get("usage")
                                    .and_then(|u| u.get("output_tokens")).and_then(|v| v.as_u64()),
                                total_tokens: msg.payload.get("usage")
                                    .and_then(|u| u.get("total_tokens")).and_then(|v| v.as_u64()),
                                reasoning_tokens: msg.payload.get("usage")
                                    .and_then(|u| u.get("reasoning_tokens")).and_then(|v| v.as_u64()),
                            };
                            let ctx_window = msg.payload.get("context_window")
                                .and_then(|v| v.as_u64())
                                .map(|v| v as usize);
                            let status = {
                                let mut s = state.lock().await;
                                s.record_usage(usage, ctx_window);
                                // Check one-shot context reminders — if any
                                // threshold is crossed, inject the message into
                                // pending_messages for the next turn.
                                if let Some(input_tok) = s.last_usage.as_ref()
                                    .and_then(|u| u.input_tokens)
                                {
                                    let injected = s.check_context_reminders(input_tok);
                                    if !injected.is_empty() {
                                        debug!(
                                            "Agent {}: context reminder fired ({} messages)",
                                            self.index, injected.len()
                                        );
                                    }
                                }
                                s.context_status_line()
                            };
                            debug!("Agent {}: {}", self.index, status);
                            // If we're close to the limit, surface a chat-level
                            // warning so the model (and the user) notice.
                            let remaining = state.lock().await.context_remaining();
                            let window = state.lock().await.context_window;
                            if let (Some(rem), Some(win)) = (remaining, window) {
                                if win > 0 && rem * 5 < win as u64 {
                                    let warn_msg = Message::new(
                                        &self.stream_output_topic,
                                        &self.id(),
                                        format!(
                                            "Context nearly full: {} tokens remaining ({}%). Consider calling restart_from_file with a handoff note.",
                                            rem,
                                            (rem as f64 / win as f64 * 100.0) as u64
                                        ),
                                    ).with_type("Warning");
                                    let _ = bus.publish(&self.id(), warn_msg).await;
                                }
                            }
                        }
                        Err(RecvError::Lagged(_)) => {}
                        Err(RecvError::Closed) => break,
                    }
                }
                _ = wake_up_timer.tick() => {
                    // Check for due scheduled wake-ups. Messages are injected
                    // into pending_messages only when the loop is idle; busy
                    // loops defer one-shot and reschedule cron.
                    let fired: Vec<String> = {
                        let mut s = state.lock().await;
                        s.check_wake_ups()
                    };
                    if !fired.is_empty() {
                        for msg in &fired {
                            info!("Agent {}: wake-up fired: {}", self.index, msg);
                            // Publish to stream so the TUI shows it.
                            let display_msg = Message::new(
                                &self.stream_output_topic,
                                &self.id(),
                                format!("[wake-up] {}", msg),
                            ).with_type("Info");
                            let _ = bus.publish(&self.id(), display_msg).await;
                        }
                        // If wake-ups injected pending messages and the loop is
                        // idle, kick off a turn to process them.
                        self.advance_turn_if_resolved(bus, &state, &subagent_stack).await;
                    }
                }
            }
        }

        Ok(())
    }
}
