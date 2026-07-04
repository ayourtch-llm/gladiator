/// Integration test: end-to-end agent loop with mock LLM.
/// Tests: user input → agent → LLM (mock) → response → agent → stream output
use gladiator_agent::AgentActor;
use gladiator_core::config::AgentConfig;
use gladiator_core::{Bus, Message};
use gladiator_llm::LlmRequest;
use std::time::Duration;
use tokio::time::timeout;

#[tokio::test]
async fn test_agent_text_roundtrip() {
    let bus = Bus::new();

    let llm_in = "llm:in";
    let llm_out = "llm:out";
    let llm_stream = "llm:stream";
    let llm_tool_calls = "llm:tool_calls";
    let tool_results = "tool:results";
    let agent_in = "agent:in";
    let agent_stream = "agent:stream";

    // Create topics
    bus.create_topic(llm_in, 100).await;
    bus.create_topic(llm_out, 100).await;
    bus.create_topic(llm_stream, 100).await;
    bus.create_topic(llm_tool_calls, 100).await;
    bus.create_topic(tool_results, 100).await;
    bus.create_topic(agent_in, 100).await;
    bus.create_topic(agent_stream, 100).await;

    // Subscribe mock LLM to llm_in BEFORE spawning agent
    bus.register_announcement(gladiator_core::ActorAnnouncement {
        id: "mock-llm".to_string(),
        subscriptions: vec![llm_in.to_string()],
        publications: vec![llm_out.to_string(), llm_stream.to_string()],
    })
    .await;
    let mut llm_rx = bus.subscribe("mock-llm", llm_in).await.unwrap();

    // Spawn agent actor
    let agent = AgentActor::new(
        0,
        agent_in.to_string(),
        llm_in.to_string(),
        llm_out.to_string(),
        llm_stream.to_string(),
        llm_tool_calls.to_string(),
        tool_results.to_string(),
        agent_stream.to_string(),
        AgentConfig::default(),
    );
    let agent_handle = bus.spawn_actor(agent).await.unwrap();

    // Give the agent time to start up and subscribe
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Subscribe to agent_stream to see output
    let mut stream_rx = bus.subscribe_stream(agent_stream).await.unwrap();

    // Publish user input to agent_in
    bus.register_announcement(gladiator_core::ActorAnnouncement {
        id: "test-user".to_string(),
        subscriptions: vec![],
        publications: vec![agent_in.to_string()],
    })
    .await;
    let user_msg = Message::new(agent_in, "test-user", "Hello, agent!")
        .with_type("UserInput");
    bus.publish("test-user", user_msg).await.unwrap();

    // Wait for the agent to forward the request to llm_in
    let llm_request = timeout(Duration::from_secs(5), llm_rx.recv())
        .await
        .expect("timed out waiting for LLM request")
        .expect("broadcast closed");

    // Verify the LLM request contains the user message
    let request: LlmRequest =
        serde_json::from_value(llm_request.payload).expect("Failed to parse LlmRequest");
    assert!(request.messages.is_some());
    let messages = request.messages.unwrap();
    assert!(!messages.is_empty());
    // First message should be system, second should be user
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "Hello, agent!");

    // Mock LLM streams a response, then publishes final to llm_out
    let stream_msg = Message::new(llm_stream, "mock-llm", "Hello from mock LLM!")
        .with_type("LlmStream");
    bus.publish("mock-llm", stream_msg).await.unwrap();

    let response_msg = Message::new(llm_out, "mock-llm", "Hello from mock LLM!");
    bus.publish("mock-llm", response_msg).await.unwrap();

    // Wait for the agent to forward the stream to agent_stream.
    // The agent now also publishes "Info" status messages, so we loop
    // until we find the actual LLM stream content.
    let mut found_stream = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match timeout(Duration::from_secs(2), stream_rx.recv()).await {
            Ok(Ok(msg)) => {
                let payload = msg.payload_str().unwrap_or_default();
                if payload.contains("Hello from mock LLM!") {
                    found_stream = true;
                    break;
                }
                // Skip Info/status messages
            }
            _ => break,
        }
    }
    assert!(found_stream, "expected LLM response in agent_stream");

    // Cleanup
    agent_handle.stop().await;
}

#[tokio::test]
async fn test_agent_with_tool_call() {
    let bus = Bus::new();

    let llm_in = "llm:in";
    let llm_out = "llm:out";
    let llm_stream = "llm:stream";
    let llm_tool_calls = "llm:tool_calls";
    let tool_results = "tool:results";
    let agent_in = "agent:in";
    let agent_stream = "agent:stream";
    let bash_execute = "tool:bash:execute";

    for topic in &[
        llm_in, llm_out, llm_stream, llm_tool_calls, tool_results, agent_in, agent_stream,
        bash_execute,
    ] {
        bus.create_topic(topic, 100).await;
    }

    // Set up mock LLM
    bus.register_announcement(gladiator_core::ActorAnnouncement {
        id: "mock-llm".to_string(),
        subscriptions: vec![llm_in.to_string()],
        publications: vec![
            llm_out.to_string(),
            llm_tool_calls.to_string(),
        ],
    })
    .await;
    let mut llm_rx = bus.subscribe("mock-llm", llm_in).await.unwrap();

    // Spawn a mock tool runner for "bash"
    bus.register_announcement(gladiator_core::ActorAnnouncement {
        id: "tool-bash".to_string(),
        subscriptions: vec![bash_execute.to_string()],
        publications: vec![tool_results.to_string()],
    })
    .await;
    let mut tool_rx = bus.subscribe("tool-bash", bash_execute).await.unwrap();

    // Spawn agent
    let tool_defs = vec![serde_json::json!({
        "type": "function",
        "function": {
            "name": "bash",
            "description": "Run a bash command",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }
        }
    })];

    let agent = AgentActor::new(
        0,
        agent_in.to_string(),
        llm_in.to_string(),
        llm_out.to_string(),
        llm_stream.to_string(),
        llm_tool_calls.to_string(),
        tool_results.to_string(),
        agent_stream.to_string(),
        AgentConfig::default(),
    )
    .with_tool_defs(tool_defs);
    let agent_handle = bus.spawn_actor(agent).await.unwrap();

    // Give the agent time to start up and subscribe
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Subscribe to stream
    let mut stream_rx = bus.subscribe_stream(agent_stream).await.unwrap();

    // Publish user input
    bus.register_announcement(gladiator_core::ActorAnnouncement {
        id: "test-user".to_string(),
        subscriptions: vec![],
        publications: vec![agent_in.to_string()],
    })
    .await;
    let user_msg = Message::new(agent_in, "test-user", "Run ls")
        .with_type("UserInput");
    bus.publish("test-user", user_msg).await.unwrap();

    // Wait for agent to send request to llm_in
    let _llm_request = timeout(Duration::from_secs(5), llm_rx.recv())
        .await
        .expect("timed out waiting for LLM request")
        .expect("broadcast closed");

    // Mock LLM responds with tool calls
    let tool_calls = serde_json::json!([{
        "id": "call_1",
        "type": "function",
        "function": {
            "name": "bash",
            "arguments": "{\"command\": \"ls\"}"
        }
    }]);
    let tc_msg = Message::new(llm_tool_calls, "mock-llm", tool_calls)
        .with_type("LlmToolCalls");
    bus.publish("mock-llm", tc_msg).await.unwrap();

    // Wait for agent to dispatch tool call to tool:bash:execute
    let tool_exec = timeout(Duration::from_secs(5), tool_rx.recv())
        .await
        .expect("timed out waiting for tool execute")
        .expect("broadcast closed");

    let exec_payload = &tool_exec.payload;
    assert_eq!(exec_payload["tool_name"], "bash");
    assert_eq!(exec_payload["tool_call_id"], "call_1");

    // Mock tool responds with result
    let result_msg = Message::new(
        tool_results,
        "tool-bash",
        serde_json::json!({
            "tool_call_id": "call_1",
            "tool_name": "bash",
            "success": true,
            "result": "file1.txt\nfile2.txt",
            "error": null,
        }),
    );
    bus.publish("tool-bash", result_msg).await.unwrap();

    // Wait for the agent to process the tool result and send another LLM request
    let second_request = timeout(Duration::from_secs(5), llm_rx.recv())
        .await
        .expect("timed out waiting for second LLM request")
        .expect("broadcast closed");

    // Verify the second LLM request includes the tool result
    let request: LlmRequest =
        serde_json::from_value(second_request.payload).expect("Failed to parse LlmRequest");
    let messages = request.messages.unwrap();
    // Should have: system, user, assistant (with tool_calls), tool result
    assert!(messages.len() >= 3);
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["role"], "user");
    // The assistant message should have tool_calls
    assert_eq!(messages[2]["role"], "assistant");
    assert!(messages[2]["tool_calls"].is_array());
    // The tool result message
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[3]["tool_call_id"], "call_1");

    // Mock LLM responds with final text
    let final_response = Message::new(llm_out, "mock-llm", "Here are the files: file1.txt and file2.txt");
    bus.publish("mock-llm", final_response).await.unwrap();

    // Wait for stream output
    let _ = timeout(Duration::from_secs(5), stream_rx.recv()).await;

    // Cleanup
    agent_handle.stop().await;
}

#[tokio::test]
async fn test_agent_max_iterations() {
    let bus = Bus::new();

    let llm_in = "llm:in";
    let llm_out = "llm:out";
    let llm_stream = "llm:stream";
    let llm_tool_calls = "llm:tool_calls";
    let tool_results = "tool:results";
    let agent_in = "agent:in";
    let agent_stream = "agent:stream";

    for topic in &[
        llm_in, llm_out, llm_stream, llm_tool_calls, tool_results, agent_in, agent_stream,
    ] {
        bus.create_topic(topic, 100).await;
    }

    // Set up mock LLM that always responds with text (no tool calls)
    bus.register_announcement(gladiator_core::ActorAnnouncement {
        id: "mock-llm".to_string(),
        subscriptions: vec![llm_in.to_string()],
        publications: vec![llm_out.to_string()],
    })
    .await;
    let mut llm_rx = bus.subscribe("mock-llm", llm_in).await.unwrap();

    // Spawn agent with max_iterations = 2
    let mut agent_config = AgentConfig::default();
    agent_config.max_iterations = 2;
    let agent = AgentActor::new(
        0,
        agent_in.to_string(),
        llm_in.to_string(),
        llm_out.to_string(),
        llm_stream.to_string(),
        llm_tool_calls.to_string(),
        tool_results.to_string(),
        agent_stream.to_string(),
        agent_config,
    );
    let agent_handle = bus.spawn_actor(agent).await.unwrap();

    // Give the agent time to start up and subscribe
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Subscribe to stream to see warning
    let mut stream_rx = bus.subscribe_stream(agent_stream).await.unwrap();

    // Publish user input
    bus.register_announcement(gladiator_core::ActorAnnouncement {
        id: "test-user".to_string(),
        subscriptions: vec![],
        publications: vec![agent_in.to_string()],
    })
    .await;
    let user_msg = Message::new(agent_in, "test-user", "Hello")
        .with_type("UserInput");
    bus.publish("test-user", user_msg).await.unwrap();

    // Agent sends to LLM, mock responds with text (this is iteration 1)
    let _ = timeout(Duration::from_secs(5), llm_rx.recv()).await;
    let response = Message::new(llm_out, "mock-llm", "Response 1");
    bus.publish("mock-llm", response).await.unwrap();

    // Agent processes the response (increment iteration to 1)
    // Since there are no tool calls, the agent doesn't loop again for tool results.
    // The agent only loops when tool calls are resolved.
    // So max_iterations is only checked after tool results are resolved.

    // Wait for stream output
    let _ = timeout(Duration::from_secs(5), stream_rx.recv()).await;

    // Cleanup
    agent_handle.stop().await;
}

/// Internal todo tools are handled inline by the agent — no execute message is
/// published on the bus, the tool result is recorded in ConversationState, and
/// the turn advances so the model sees its own plan.
#[tokio::test]
async fn test_agent_internal_todo_write_advances_turn() {
    let bus = Bus::new();

    let llm_in = "llm:in";
    let llm_out = "llm:out";
    let llm_stream = "llm:stream";
    let llm_tool_calls = "llm:tool_calls";
    let tool_results = "tool:results";
    let agent_in = "agent:in";
    let agent_stream = "agent:stream";

    for topic in &[
        llm_in, llm_out, llm_stream, llm_tool_calls, tool_results, agent_in, agent_stream,
    ] {
        bus.create_topic(topic, 100).await;
    }

    // Mock LLM
    bus.register_announcement(gladiator_core::ActorAnnouncement {
        id: "mock-llm".to_string(),
        subscriptions: vec![llm_in.to_string()],
        publications: vec![llm_out.to_string(), llm_stream.to_string(), llm_tool_calls.to_string()],
    })
    .await;
    let mut llm_rx = bus.subscribe("mock-llm", llm_in).await.unwrap();

    // Spy on the execute topic to prove nothing is dispatched for internal tools.
    let exec_topic = "tool:todo_write:execute";
    bus.create_topic(exec_topic, 100).await;
    bus.register_announcement(gladiator_core::ActorAnnouncement {
        id: "spy-exec".to_string(),
        subscriptions: vec![],
        publications: vec![exec_topic.to_string()],
    })
    .await;
    let mut exec_rx = bus.subscribe("spy-exec", exec_topic).await.unwrap();

    // Agent with the internal todo tool defs appended.
    let mut tool_defs = gladiator_agent::internal_tools::internal_tool_defs();
    tool_defs.push(serde_json::json!({
        "type": "function",
        "function": {
            "name": "bash", "description": "Run a bash command",
            "parameters": {"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}
        }
    }));
    let agent = AgentActor::new(
        0, agent_in.to_string(),
        llm_in.to_string(),   llm_out.to_string(),
        llm_stream.to_string(), llm_tool_calls.to_string(),
        tool_results.to_string(), agent_stream.to_string(),
        AgentConfig::default(),
    )
    .with_tool_defs(tool_defs);
    let agent_handle = bus.spawn_actor(agent).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Drive with user input.
    bus.register_announcement(gladiator_core::ActorAnnouncement {
        id: "test-user".to_string(),
        subscriptions: vec![],
        publications: vec![agent_in.to_string()],
    })
    .await;
    let _ = bus
        .publish("test-user", Message::new(agent_in, "test-user", "plan it").with_type("UserInput"))
        .await;

    // Initial request reaches llm_in.
    let _ = timeout(Duration::from_secs(5), llm_rx.recv())
        .await
        .expect("initial LLM request");

    // Mock replies with a todo_write tool call.
    let todo_call = serde_json::json!([{
        "id": "call_todo_1", "type": "function",
        "function": {
            "name": "todo_write",
            "arguments": "{\"todos\":[{\"content\":\"step one\",\"status\":\"in_progress\",\"priority\":\"high\"},{\"content\":\"step two\",\"status\":\"pending\"}]}"
        }
    }]);
    bus.publish(
        "mock-llm",
        Message::new(llm_tool_calls, "mock-llm", todo_call).with_type("LlmToolCalls"),
    )
    .await
    .unwrap();

    // Critical: agent must send a follow-up request to llm_in despite no
    // executor having been dispatched (the internal tool was handled inline).
    let second_req = timeout(Duration::from_secs(5), llm_rx.recv())
        .await
        .expect("timed out waiting for follow-up after internal todo_write")
        .unwrap();
    let parsed: LlmRequest =
        serde_json::from_value(second_req.payload).expect("parse follow-up LlmRequest");
    let messages = parsed.messages.expect("messages in follow-up");

    // History must contain a tool-role entry whose content is the rendered plan.
    let tool_msg = messages
        .iter()
        .find(|m| m["role"] == "tool" && m["name"] == "todo_write")
        .expect("expected todo_write tool result in history");
    let content = tool_msg["content"].as_str().unwrap_or("");
    assert!(content.contains("step one"), "rendered plan missing 'step one': {}", content);
    assert!(content.contains("[~]"), "expected in_progress glyph: {}", content);

    // No execute message should have leaked onto the bus.
    let leaked = timeout(Duration::from_millis(150), exec_rx.recv()).await;
    assert!(leaked.is_err(), "internal tool must not dispatch an executor");

    agent_handle.stop().await;
}

/// ConversationState serializes its todos so they survive save/load.
#[test]
fn test_conversation_state_todos_roundtrip_through_serde() {
    use gladiator_agent::{ConversationState, TodoEntry, TodoStatus};

    let mut state = ConversationState::new();
    state.add_user_message("plan the work");
    state.set_todos(vec![
        TodoEntry {
            content: "implement feature".into(),
            status: TodoStatus::InProgress,
            priority: "high".into(),
        },
        TodoEntry {
            content: "write tests".into(),
            status: TodoStatus::Pending,
            priority: "medium".into(),
        },
    ]);

    let json = serde_json::to_string(&state).unwrap();
    assert!(json.contains("implement feature"), "todos missing from serialized state");
    assert!(json.contains("in_progress"), "status missing from serialized state");

    let restored: ConversationState = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.todos.len(), 2);
    assert_eq!(restored.todos[0].content, "implement feature");
    assert_eq!(restored.todos[0].status, TodoStatus::InProgress);
    assert_eq!(restored.todos[1].status, TodoStatus::Pending);
    assert_eq!(restored.messages.len(), 1);
}

/// A state file written before the todos field existed (no "todos" key) must
/// still load cleanly — the field defaults to empty via #[serde(default)].
#[test]
fn test_conversation_state_loads_without_todos_key() {
    use gladiator_agent::ConversationState;

    let legacy = serde_json::json!({
        "messages": [{"role": "user", "content": "hi"}],
        "iteration_count": 0,
        "pending_tool_calls": [],
        "pending_messages": [],
        "was_interrupted": false
    });
    let state: ConversationState = serde_json::from_value(legacy).unwrap();
    assert!(state.todos.is_empty(), "missing todos key must default to empty");
}

/// restart_from_file: backs up the live context to /tmp, wipes the transcript,
/// and injects the file's contents as a fresh "continue executing" instruction.
/// The follow-up LLM request must contain ONLY the injected user message (no
/// prior history, no orphan tool result for the restart call itself), and a
/// backup file containing the old conversation must appear under /tmp.
#[tokio::test]
async fn test_agent_restart_from_file_clears_and_reinjects() {
    use std::collections::HashSet;

    let bus = Bus::new();

    let llm_in = "llm:in";
    let llm_out = "llm:out";
    let llm_stream = "llm:stream";
    let llm_tool_calls = "llm:tool_calls";
    let tool_results = "tool:results";
    let agent_in = "agent:in";
    let agent_stream = "agent:stream";

    for topic in &[
        llm_in, llm_out, llm_stream, llm_tool_calls, tool_results, agent_in, agent_stream,
    ] {
        bus.create_topic(topic, 100).await;
    }

    // Mock LLM
    bus.register_announcement(gladiator_core::ActorAnnouncement {
        id: "mock-llm".to_string(),
        subscriptions: vec![llm_in.to_string()],
        publications: vec![llm_out.to_string(), llm_stream.to_string(), llm_tool_calls.to_string()],
    })
    .await;
    let mut llm_rx = bus.subscribe("mock-llm", llm_in).await.unwrap();

    // Spy on the execute topic to prove restart is handled inline.
    let exec_topic = "tool:restart_from_file:execute";
    bus.create_topic(exec_topic, 100).await;
    bus.register_announcement(gladiator_core::ActorAnnouncement {
        id: "spy-exec".to_string(),
        subscriptions: vec![],
        publications: vec![exec_topic.to_string()],
    })
    .await;
    let mut exec_rx = bus.subscribe("spy-exec", exec_topic).await.unwrap();

    let tool_defs = gladiator_agent::internal_tools::internal_tool_defs();
    let agent = AgentActor::new(
        0, agent_in.to_string(),
        llm_in.to_string(),   llm_out.to_string(),
        llm_stream.to_string(), llm_tool_calls.to_string(),
        tool_results.to_string(), agent_stream.to_string(),
        AgentConfig::default(),
    )
    .with_tool_defs(tool_defs);
    let agent_handle = bus.spawn_actor(agent).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Write the handoff file the restart tool will read.
    let handoff_path = format!(
        "/tmp/gladiator-restart-test-{}.md",
        std::process::id()
    );
    let handoff_body = "## Handoff\nThe widget parser is half-done. Finish parse_widget().";
    std::fs::write(&handoff_path, handoff_body).unwrap();

    // Snapshot existing /tmp/*.json so we can identify the new backup.
    let before: HashSet<String> = std::fs::read_dir("/tmp")
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().to_string_lossy().to_string())
        .collect();

    bus.register_announcement(gladiator_core::ActorAnnouncement {
        id: "test-user".to_string(),
        subscriptions: vec![],
        publications: vec![agent_in.to_string()],
    })
    .await;
    let _ = bus
        .publish(
            "test-user",
            Message::new(agent_in, "test-user", "prime the history").with_type("UserInput"),
        )
        .await;

    // Initial request reaches llm_in.
    let _ = timeout(Duration::from_secs(5), llm_rx.recv())
        .await
        .expect("initial LLM request");

    // Mock replies with a restart_from_file tool call.
    let restart_call = serde_json::json!([{
        "id": "call_restart_1", "type": "function",
        "function": {
            "name": "restart_from_file",
            "arguments": format!("{{\"filename\":\"{}\"}}", handoff_path)
        }
    }]);
    bus.publish(
        "mock-llm",
        Message::new(llm_tool_calls, "mock-llm", restart_call).with_type("LlmToolCalls"),
    )
    .await
    .unwrap();

    // The agent must send a follow-up LLM request containing the injected
    // restart instruction and ONLY that (history wiped).
    let follow_up = timeout(Duration::from_secs(5), llm_rx.recv())
        .await
        .expect("timed out waiting for follow-up after restart_from_file")
        .unwrap();
    let parsed: LlmRequest =
        serde_json::from_value(follow_up.payload).expect("parse follow-up LlmRequest");
    let messages = parsed.messages.expect("messages in follow-up");

    // History must be wiped: only the default system message and the injected
    // restart instruction remain — no prior user message, no orphan tool result.
    assert_eq!(
        messages.len(),
        2,
        "expected [system, user] after restart, got {}: {:?}",
        messages.len(),
        messages,
    );
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["role"], "user");
    let content = messages[1]["content"].as_str().unwrap_or("");
    assert!(content.contains("parse_widget"), "injected text missing handoff body: {}", content);
    assert!(content.contains("Continue executing"), "missing continuation directive: {}", content);

    // No tool result for the restart call must leak into history, and the old
    // "prime the history" user message must be gone.
    assert!(
        !messages.iter().any(|m| m["role"] == "tool"),
        "restart_from_file must not leave a tool-result message in history: {:?}",
        messages,
    );
    assert!(
        !messages.iter().any(|m| {
            m["content"].as_str().map(|c| c.contains("prime the history")).unwrap_or(false)
        }),
        "old user message survived restart: {:?}",
        messages,
    );

    // No execute message dispatched on the bus.
    let leaked = timeout(Duration::from_millis(150), exec_rx.recv()).await;
    assert!(leaked.is_err(), "restart_from_file must not dispatch an executor");

    // A new /tmp/*-*.json backup must have appeared, containing the OLD
    // conversation (the "prime the history" user message).
    let after: Vec<String> = std::fs::read_dir("/tmp")
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().to_string_lossy().to_string())
        .collect();
    let new_files: Vec<&String> = after.iter().filter(|p| !before.contains(*p)).collect();
    let backup_with_old_msg = new_files.iter().any(|p| {
        std::fs::read_to_string(p)
            .map(|body| body.contains("prime the history"))
            .unwrap_or(false)
    });
    assert!(
        backup_with_old_msg,
        "expected a new /tmp backup containing the old conversation; new files: {:?}",
        new_files
    );

    // Cleanup.
    let _ = std::fs::remove_file(&handoff_path);
    agent_handle.stop().await;
}

/// StreamStats messages from the LLM actor carry per-turn usage + the
/// discovered context window. The agent must subscribe, record them into
/// ConversationState, and surface "tokens remaining" through todo_read.
#[tokio::test]
async fn test_agent_consumes_stream_stats_and_reports_via_todo_read() {
    let bus = Bus::new();

    let llm_in = "llm:in";
    let llm_out = "llm:out";
    let llm_stream = "llm:stream";
    let llm_tool_calls = "llm:tool_calls";
    let llm_stats = "llm:stats";
    let tool_results = "tool:results";
    let agent_in = "agent:in";
    let agent_stream = "agent:stream";

    for topic in &[
        llm_in, llm_out, llm_stream, llm_tool_calls, llm_stats, tool_results, agent_in, agent_stream,
    ] {
        bus.create_topic(topic, 100).await;
    }

    bus.register_announcement(gladiator_core::ActorAnnouncement {
        id: "mock-llm".to_string(),
        subscriptions: vec![llm_in.to_string()],
        publications: vec![
            llm_out.to_string(),
            llm_stream.to_string(),
            llm_tool_calls.to_string(),
            llm_stats.to_string(),
        ],
    })
    .await;
    let mut llm_rx = bus.subscribe("mock-llm", llm_in).await.unwrap();

    let tool_defs = gladiator_agent::internal_tools::internal_tool_defs();
    let agent = AgentActor::new(
        0, agent_in.to_string(),
        llm_in.to_string(),   llm_out.to_string(),
        llm_stream.to_string(), llm_tool_calls.to_string(),
        tool_results.to_string(), agent_stream.to_string(),
        AgentConfig::default(),
    )
    .with_tool_defs(tool_defs)
    .with_llm_stats_topic(llm_stats.to_string())
    .with_context_window(Some(128_000));
    let agent_handle = bus.spawn_actor(agent).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Publish a StreamStats message as the LLM actor would after a turn.
    let stats_payload = serde_json::json!({
        "rx_chars": 1234,
        "usage": {
            "input_tokens": 32000,
            "output_tokens": 500,
            "total_tokens": 32500
        },
        "context_window": 128000
    });
    bus.publish(
        "mock-llm",
        Message::new(llm_stats, "mock-llm", stats_payload).with_type("StreamStats"),
    )
    .await
    .unwrap();

    // Drive the agent with user input, then have the mock respond with a
    // todo_read so we can inspect what the agent reports.
    bus.register_announcement(gladiator_core::ActorAnnouncement {
        id: "test-user".to_string(),
        subscriptions: vec![],
        publications: vec![agent_in.to_string()],
    })
    .await;
    let _ = bus
        .publish("test-user", Message::new(agent_in, "test-user", "check status").with_type("UserInput"))
        .await;
    let _ = timeout(Duration::from_secs(5), llm_rx.recv())
        .await
        .expect("initial LLM request");

    // Mock replies with a todo_read tool call.
    let read_call = serde_json::json!([{
        "id": "call_read_1", "type": "function",
        "function": {"name": "todo_read", "arguments": "{}"}
    }]);
    bus.publish(
        "mock-llm",
        Message::new(llm_tool_calls, "mock-llm", read_call).with_type("LlmToolCalls"),
    )
    .await
    .unwrap();

    // Follow-up LLM request carries the tool result, which must include the
    // context status line reflecting the stats we published.
    let follow_up = timeout(Duration::from_secs(5), llm_rx.recv())
        .await
        .expect("follow-up after todo_read")
        .unwrap();
    let parsed: LlmRequest =
        serde_json::from_value(follow_up.payload).expect("parse follow-up");
    let messages = parsed.messages.expect("messages");
    let tool_msg = messages
        .iter()
        .find(|m| m["role"] == "tool" && m["name"] == "todo_read")
        .expect("expected todo_read tool result");
    let content = tool_msg["content"].as_str().unwrap_or("");
    assert!(
        content.contains("32000"),
        "expected used tokens in todo_read output: {}",
        content
    );
    assert!(
        content.contains("128000"),
        "expected window size in todo_read output: {}",
        content
    );
    assert!(
        content.contains("96000"),
        "expected remaining tokens in todo_read output: {}",
        content
    );

    agent_handle.stop().await;
}
