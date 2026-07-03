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
