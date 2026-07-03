/// End-to-end integration tests with the real LLM endpoint.
/// These tests require a running LLM server at the configured endpoint.
/// Run with: cargo test --test e2e_test -- --ignored
use gladiator_agent::AgentActor;
use gladiator_core::config::{AgentConfig, LlmConfig};
use gladiator_core::{Bus, Message};
use gladiator_llm::LlmActor;
use gladiator_tools::builtin::BashTool;
use gladiator_tools::{ToolActorRunner, ToolRegistry};
use std::time::Duration;
use tokio::time::timeout;

fn llm_config() -> LlmConfig {
    LlmConfig {
        model: "custom/glm-5.2".to_string(),
        base_url: "http://ts-agent-gateway:4000/v1".to_string(),
        api_key: String::new(),
        temperature: 0.7,
        max_tokens: 4096,
        request_timeout_secs: 120,
        stream_timeout_secs: 300,
        max_retries: 3,
        retry_base_delay_ms: 500,
    }
}

/// Helper to set up topics on the bus.
async fn setup_topics(bus: &Bus) {
    let topics = [
        "llm:in", "llm:out", "llm:stream", "llm:stats", "llm:tool_calls",
        "tool:results", "agent:in", "agent:stream", "user:control",
        "tool:bash:execute",
    ];
    for topic in &topics {
        bus.create_topic(topic, 100).await;
    }
}

/// Helper to spawn tool runners for built-in tools.
async fn spawn_tool_runners(bus: &Bus) -> (Vec<tokio::task::JoinHandle<()>>, ToolRegistry) {
    let mut registry = ToolRegistry::new();
    registry.add(Box::new(BashTool::new()));
    let handles: Vec<tokio::task::JoinHandle<()>> = registry
        .iter()
        .map(|tool| {
            let runner = ToolActorRunner::from_arc(tool.clone());
            let bus_clone = bus.clone();
            tokio::spawn(async move {
                let _ = runner.run(&bus_clone).await;
            })
        })
        .collect();
    (handles, registry)
}

/// Test: Send a simple text prompt to the real LLM and verify we get a response.
#[tokio::test]
#[ignore]
async fn e2e_simple_text_response() {
    let bus = Bus::new();
    setup_topics(&bus).await;

    // Spawn tool runners
    let (tool_handles, registry) = spawn_tool_runners(&bus).await;

    // Spawn LLM actor
    let llm_actor = LlmActor::new(
        0,
        "llm:in".to_string(),
        "llm:out".to_string(),
        "llm:stream".to_string(),
        "llm:stats".to_string(),
        "llm:tool_calls".to_string(),
        "user:control".to_string(),
        llm_config(),
    );
    let llm_handle = bus.spawn_actor(llm_actor).await.unwrap();

    // Spawn agent
    let tool_defs = registry.syntaxes().iter().map(|s| s.to_openai_json()).collect();
    let agent = AgentActor::new(
        0,
        "agent:in".to_string(),
        "llm:in".to_string(),
        "llm:out".to_string(),
        "llm:stream".to_string(),
        "llm:tool_calls".to_string(),
        "tool:results".to_string(),
        "agent:stream".to_string(),
        AgentConfig::default(),
    )
    .with_tool_defs(tool_defs);
    let agent_handle = bus.spawn_actor(agent).await.unwrap();

    tokio::time::sleep(Duration::from_millis(300)).await;

    // Subscribe to agent_stream to see responses
    let mut stream_rx = bus.subscribe_stream("agent:stream").await.unwrap();

    // Publish user input
    bus.register_announcement(gladiator_core::ActorAnnouncement {
        id: "test-user".to_string(),
        subscriptions: vec![],
        publications: vec!["agent:in".to_string()],
    })
    .await;
    let user_msg = Message::new("agent:in", "test-user", "What is 2+2? Reply with just the number.")
        .with_type("UserInput");
    bus.publish("test-user", user_msg).await.unwrap();

    // Wait for response on agent_stream (timeout: 60s for real LLM)
    let response = timeout(Duration::from_secs(60), stream_rx.recv())
        .await
        .expect("timed out waiting for LLM response")
        .expect("broadcast closed");

    let payload = response.payload_str().unwrap_or_default();
    println!("LLM response: {}", payload);
    assert!(
        !payload.is_empty(),
        "expected non-empty response from LLM"
    );

    // Cleanup
    llm_handle.stop().await;
    agent_handle.stop().await;
    for handle in tool_handles {
        handle.abort();
    }
}

/// Test: Send a prompt that triggers a tool call (bash: echo hello)
#[tokio::test]
#[ignore]
async fn e2e_tool_call_response() {
    let bus = Bus::new();
    setup_topics(&bus).await;

    let (tool_handles, registry) = spawn_tool_runners(&bus).await;

    let llm_actor = LlmActor::new(
        0,
        "llm:in".to_string(),
        "llm:out".to_string(),
        "llm:stream".to_string(),
        "llm:stats".to_string(),
        "llm:tool_calls".to_string(),
        "user:control".to_string(),
        llm_config(),
    );
    let llm_handle = bus.spawn_actor(llm_actor).await.unwrap();

    let tool_defs = registry.syntaxes().iter().map(|s| s.to_openai_json()).collect();
    let agent = AgentActor::new(
        0,
        "agent:in".to_string(),
        "llm:in".to_string(),
        "llm:out".to_string(),
        "llm:stream".to_string(),
        "llm:tool_calls".to_string(),
        "tool:results".to_string(),
        "agent:stream".to_string(),
        AgentConfig::default(),
    )
    .with_tool_defs(tool_defs);
    let agent_handle = bus.spawn_actor(agent).await.unwrap();

    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut stream_rx = bus.subscribe_stream("agent:stream").await.unwrap();

    // Publish user input that should trigger a bash tool call
    bus.register_announcement(gladiator_core::ActorAnnouncement {
        id: "test-user".to_string(),
        subscriptions: vec![],
        publications: vec!["agent:in".to_string()],
    })
    .await;
    let user_msg = Message::new(
        "agent:in",
        "test-user",
        "Use the bash tool to run the command: echo hello",
    )
    .with_type("UserInput");
    bus.publish("test-user", user_msg).await.unwrap();

    // Collect stream messages (timeout: 120s for real LLM with tool call)
    let mut received_text = String::new();
    let deadline = Duration::from_secs(120);
    loop {
        match timeout(deadline, stream_rx.recv()).await {
            Ok(Ok(msg)) => {
                let payload = msg.payload_str().unwrap_or_default();
                received_text.push_str(&payload);
                if payload.contains("hello") || received_text.contains("hello") {
                    break;
                }
            }
            _ => break,
        }
    }

    println!("Received stream text: {}", received_text);
    assert!(
        !received_text.is_empty(),
        "expected non-empty stream from LLM"
    );

    // Cleanup
    llm_handle.stop().await;
    agent_handle.stop().await;
    for handle in tool_handles {
        handle.abort();
    }
}
