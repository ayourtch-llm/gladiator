use crate::internal_tools::InternalToolOutcome;
use crate::state::ConversationState;
use gladiator_core::{Actor, ActorAnnouncement, AgentConfig, Bus, Message};
use gladiator_llm::LlmRequest;
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, error, info, warn};

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

#[derive(Debug, Default)]
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

    /// After a tool call resolves, check whether the whole batch is now
    /// complete. If so, drain any user messages that arrived mid-execution,
    /// honour the iteration cap, and dispatch the next LLM turn. Shared by the
    /// external-tool result path and the internal-tool inline path so the
    /// follow-up logic stays in exactly one place.
    async fn advance_turn_if_resolved(
        &self,
        bus: &Bus,
        state: &Arc<tokio::sync::Mutex<ConversationState>>,
    ) {
        let mut s = state.lock().await;
        if !s.all_tool_calls_resolved() {
            return;
        }
        let pending = s.drain_pending_messages();
        if !pending.is_empty() {
            s.reset_iteration();
            for m in &pending {
                s.add_user_message(m.clone());
                let displayed_msg = Message::new(
                    &self.stream_output_topic,
                    &self.id(),
                    m.clone(),
                )
                .with_type("UserMessageDisplayed");
                let _ = bus.publish(&self.id(), displayed_msg).await;
            }
        }
        if s.max_reached(self.max_iterations) {
            drop(s);
            let warn_msg = Message::new(
                &self.stream_output_topic,
                &self.id(),
                format!("Max iterations ({}) reached", self.max_iterations),
            )
            .with_type("Warning");
            let _ = bus.publish(&self.id(), warn_msg).await;
        } else {
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
        state: &Arc<tokio::sync::Mutex<ConversationState>>,
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
                InternalToolOutcome::ok(s.todos_render())
            }
            "restart_from_file" => self.handle_restart_from_file(args, state).await,
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

    async fn send_conversation(
        &self,
        bus: &Bus,
        messages: &[serde_json::Value],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
        let state: Arc<tokio::sync::Mutex<ConversationState>> =
            Arc::new(tokio::sync::Mutex::new(ConversationState::new()));

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

        let mut tool_watchdog = tokio::time::interval(std::time::Duration::from_secs(10));

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
                                if !s.pending_tool_calls.is_empty() {
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

                            // Publish status to TUI
                            let status_msg = Message::new(
                                &self.stream_output_topic,
                                &self.id(),
                                "Sending request to LLM...",
                            ).with_type("Info");
                            let _ = bus.publish(&self.id(), status_msg).await;

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
                            debug!("Agent {} received LLM output: {}", self.index, output);
                            // Check if this is an interrupt message from the LLM
                            if output.starts_with("Interrupted:") {
                                let mut s = state.lock().await;
                                s.was_interrupted = true;
                                s.clear_reasoning();
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
                                {
                                    let mut s = state.lock().await;
                                    s.add_assistant_message(output);
                                    s.increment_iteration();
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
                            let preview = if preview.len() > 60 { format!("{}...", &preview[..60]) } else { preview };
                            debug!("Agent {} forwarding stream ({}) to {}: {}", self.index, msg_type, self.stream_output_topic, preview);
                            let mut forwarded = msg.clone();
                            forwarded.topic = self.stream_output_topic.clone();
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
                                s.add_tool_calls(tool_calls.clone());
                                s.increment_iteration();
                            }

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
                                    let tool_status = Message::new(
                                        &self.stream_output_topic,
                                        &self.id(),
                                        format!("Calling tool: {}({})", func_name, func_args_str),
                                    ).with_type("Info");
                                    let _ = bus.publish(&self.id(), tool_status).await;

                                    let outcome =
                                        self.handle_internal_tool(&func_name, &args, &state).await;
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
                                    let _ = bus.publish(&self.id(), stream_msg).await;

                                    self.advance_turn_if_resolved(bus, &state).await;
                                    continue;
                                }

                                info!("Agent {} dispatching tool call: {}({})", self.index, func_name, func_args_str);

                                // Publish tool call status to TUI
                                let tool_status = Message::new(
                                    &self.stream_output_topic,
                                    &self.id(),
                                    format!("Calling tool: {}({})", func_name, func_args_str),
                                ).with_type("Info");
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
                            let _ = bus.publish(&self.id(), stream_msg).await;

                            self.advance_turn_if_resolved(bus, &state).await;
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
            }
        }

        Ok(())
    }
}
