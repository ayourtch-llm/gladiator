use gladiator_core::*;
use gladiator_agent::*;

// =========================================================================
// AgentActor construction tests
// =========================================================================

#[tokio::test]
async fn agent_actor_announce() {
    let actor = AgentActor::new(
        0,
        "agent:in".to_string(),
        "llm:in".to_string(),
        "llm:out".to_string(),
        "llm:stream".to_string(),
        "llm:tool_calls".to_string(),
        "tool:results".to_string(),
        "agent:stream".to_string(),
        AgentConfig::default(),
    );
    let ann = actor.announce();
    assert_eq!(ann.id, "gladiator-agent-0");
    assert!(ann.subscriptions.contains(&"agent:in".to_string()));
    assert!(ann.subscriptions.contains(&"llm:out".to_string()));
    assert!(ann.subscriptions.contains(&"llm:stream".to_string()));
    assert!(ann.subscriptions.contains(&"llm:tool_calls".to_string()));
    assert!(ann.subscriptions.contains(&"tool:results".to_string()));
    assert!(ann.publications.contains(&"agent:stream".to_string()));
    assert!(ann.publications.contains(&"llm:in".to_string()));
}

#[tokio::test]
async fn agent_actor_multiple_instances() {
    let actor0 = AgentActor::new(
        0, "agent:0:in".to_string(), "llm:0:in".to_string(),
        "llm:0:out".to_string(), "llm:0:stream".to_string(),
        "llm:0:tool_calls".to_string(), "tool:results".to_string(),
        "agent:0:stream".to_string(), AgentConfig::default(),
    );
    let actor1 = AgentActor::new(
        1, "agent:1:in".to_string(), "llm:1:in".to_string(),
        "llm:1:out".to_string(), "llm:1:stream".to_string(),
        "llm:1:tool_calls".to_string(), "tool:results".to_string(),
        "agent:1:stream".to_string(), AgentConfig::default(),
    );
    assert_eq!(actor0.announce().id, "gladiator-agent-0");
    assert_eq!(actor1.announce().id, "gladiator-agent-1");
}

// =========================================================================
// AgentActor with tools tests
// =========================================================================

#[tokio::test]
async fn agent_actor_with_tools() {
    let actor = AgentActor::new(
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
    .with_max_iterations(10)
    .with_system_message("You are a test agent.".to_string());

    assert_eq!(actor.max_iterations, 10);
    assert_eq!(actor.system_message, "You are a test agent.");
}

// =========================================================================
// Conversation state tests
// =========================================================================

#[test]
fn conversation_state_new() {
    let state = ConversationState::new();
    assert!(state.messages.is_empty());
    assert_eq!(state.iteration_count, 0);
    assert!(state.pending_tool_calls.is_empty());
}

#[test]
fn conversation_state_add_user_message() {
    let mut state = ConversationState::new();
    state.add_user_message("Write a function");
    assert_eq!(state.messages.len(), 1);
    assert_eq!(state.messages[0]["role"], "user");
    assert_eq!(state.messages[0]["content"], "Write a function");
}

#[test]
fn conversation_state_add_assistant_message() {
    let mut state = ConversationState::new();
    state.add_assistant_message("Here is the function");
    assert_eq!(state.messages.len(), 1);
    assert_eq!(state.messages[0]["role"], "assistant");
}

#[test]
fn conversation_state_add_tool_result() {
    let mut state = ConversationState::new();
    state.add_tool_result("call-1", "bash", "output here", true);
    assert_eq!(state.messages.len(), 1);
    assert_eq!(state.messages[0]["role"], "tool");
    assert_eq!(state.messages[0]["tool_call_id"], "call-1");
    assert_eq!(state.messages[0]["name"], "bash");
}

#[test]
fn conversation_state_add_tool_calls() {
    let mut state = ConversationState::new();
    let tool_calls = vec![
        serde_json::json!({
            "id": "call-1",
            "type": "function",
            "function": {"name": "bash", "arguments": "{}"}
        }),
    ];
    state.add_tool_calls(tool_calls.clone());
    assert_eq!(state.messages.len(), 1);
    assert_eq!(state.messages[0]["role"], "assistant");
    assert!(state.messages[0]["tool_calls"].is_array());
    assert!(state.pending_tool_calls.contains(&"call-1".to_string()));
}

#[test]
fn conversation_state_resolve_tool_call() {
    let mut state = ConversationState::new();
    state.pending_tool_calls.insert("call-1".to_string());
    state.resolve_tool_call("call-1");
    assert!(state.pending_tool_calls.is_empty());
}

#[test]
fn conversation_state_increment_iteration() {
    let mut state = ConversationState::new();
    state.increment_iteration();
    state.increment_iteration();
    assert_eq!(state.iteration_count, 2);
}

#[test]
fn conversation_state_max_reached() {
    let mut state = ConversationState::new();
    state.increment_iteration();
    assert!(!state.max_reached(5));
    for _ in 0..4 {
        state.increment_iteration();
    }
    assert!(state.max_reached(5));
}

// =========================================================================
// Interrupt / message sequence repair tests
// =========================================================================

#[test]
fn conversation_state_was_interrupted_default_false() {
    let state = ConversationState::new();
    assert!(!state.was_interrupted);
}

#[test]
fn conversation_state_was_interrupted_set() {
    let mut state = ConversationState::new();
    state.was_interrupted = true;
    assert!(state.was_interrupted);
}

#[test]
fn merge_user_message_appends_to_last_user() {
    let mut state = ConversationState::new();
    state.add_user_message("Hello");
    state.merge_user_message("World");
    assert_eq!(state.messages.len(), 1, "Should have 1 message, not 2");
    assert_eq!(state.messages[0]["role"], "user");
    assert_eq!(state.messages[0]["content"], "Hello\nWorld");
}

#[test]
fn merge_user_message_adds_new_when_last_is_assistant() {
    let mut state = ConversationState::new();
    state.add_user_message("Hello");
    state.add_assistant_message("Hi there");
    state.merge_user_message("World");
    assert_eq!(state.messages.len(), 3);
    assert_eq!(state.messages[1]["role"], "assistant");
    assert_eq!(state.messages[2]["role"], "user");
    assert_eq!(state.messages[2]["content"], "World");
}

#[test]
fn merge_user_message_adds_new_when_empty() {
    let mut state = ConversationState::new();
    state.merge_user_message("Hello");
    assert_eq!(state.messages.len(), 1);
    assert_eq!(state.messages[0]["role"], "user");
    assert_eq!(state.messages[0]["content"], "Hello");
}

#[test]
fn merge_user_message_multiple_merges() {
    let mut state = ConversationState::new();
    state.add_user_message("first");
    state.merge_user_message("second");
    state.merge_user_message("third");
    assert_eq!(state.messages.len(), 1);
    assert_eq!(state.messages[0]["content"], "first\nsecond\nthird");
}
