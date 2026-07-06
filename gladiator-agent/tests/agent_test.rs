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

#[test]
fn conversation_state_reset_iteration() {
    let mut state = ConversationState::new();
    state.increment_iteration();
    state.increment_iteration();
    assert_eq!(state.iteration_count, 2);
    assert!(state.max_reached(2));
    state.reset_iteration();
    assert_eq!(state.iteration_count, 0);
    assert!(!state.max_reached(2));
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

// =========================================================================
// ConversationState serialization tests
// =========================================================================

#[test]
fn conversation_state_serialize_roundtrip() {
    let mut state = ConversationState::new();
    state.add_user_message("Hello");
    state.add_assistant_message("Hi there");
    state.add_tool_result("call-1", "bash", "output", true);
    state.increment_iteration();
    state.was_interrupted = true;

    let json = serde_json::to_value(&state).unwrap();
    let deserialized: ConversationState = serde_json::from_value(json).unwrap();

    assert_eq!(deserialized.messages.len(), state.messages.len());
    assert_eq!(deserialized.iteration_count, state.iteration_count);
    assert_eq!(deserialized.was_interrupted, state.was_interrupted);
}

#[test]
fn conversation_state_serialize_has_expected_fields() {
    let mut state = ConversationState::new();
    state.add_user_message("test");
    state.increment_iteration();

    let json = serde_json::to_value(&state).unwrap();
    assert!(json.get("messages").is_some());
    assert!(json.get("iteration_count").is_some());
    assert!(json.get("pending_tool_calls").is_some());
    assert!(json.get("pending_messages").is_some());
    assert!(json.get("was_interrupted").is_some());
}

#[test]
fn conversation_state_serialize_empty() {
    let state = ConversationState::new();
    let json_str = serde_json::to_string(&state).unwrap();
    let deserialized: ConversationState = serde_json::from_str(&json_str).unwrap();
    assert!(deserialized.messages.is_empty());
    assert_eq!(deserialized.iteration_count, 0);
    assert!(!deserialized.was_interrupted);
}

// =========================================================================
// Reasoning accumulation tests
// =========================================================================

#[test]
fn reasoning_attached_to_assistant_message() {
    let mut state = ConversationState::new();
    state.append_reasoning("Thinking step 1");
    state.append_reasoning("Thinking step 2");
    state.add_assistant_message("Here is the answer");
    assert_eq!(state.messages.len(), 1);
    assert_eq!(state.messages[0]["role"], "assistant");
    assert_eq!(state.messages[0]["content"], "Here is the answer");
    assert_eq!(state.messages[0]["reasoning"], "Thinking step 1Thinking step 2");
    // Reasoning should be drained after attaching
    assert!(state.current_reasoning.is_empty());
}

#[test]
fn reasoning_attached_to_tool_calls() {
    let mut state = ConversationState::new();
    state.append_reasoning("I need to use a tool");
    let tool_calls = vec![serde_json::json!({
        "id": "call-1",
        "type": "function",
        "function": {"name": "bash", "arguments": "{}"}
    })];
    state.add_tool_calls(tool_calls);
    assert_eq!(state.messages.len(), 1);
    assert_eq!(state.messages[0]["role"], "assistant");
    assert_eq!(state.messages[0]["reasoning"], "I need to use a tool");
    assert!(state.messages[0]["tool_calls"].is_array());
    assert!(state.current_reasoning.is_empty());
}

#[test]
fn content_attached_to_tool_calls() {
    // Natural-language content streamed alongside tool calls must be preserved
    // on the assistant message so the model retains a record of its decisions.
    let mut state = ConversationState::new();
    state.append_partial_response("Let me check the build output.");
    let tool_calls = vec![serde_json::json!({
        "id": "call-1",
        "type": "function",
        "function": {"name": "bash", "arguments": "{}"}
    })];
    state.add_tool_calls(tool_calls);
    assert_eq!(state.messages.len(), 1);
    assert_eq!(state.messages[0]["content"], "Let me check the build output.");
    assert!(state.messages[0]["tool_calls"].is_array());
    assert!(state.current_partial_response.is_empty());
    // Content flows back to the LLM (only reasoning is stripped).
    let sent = state.build_messages_with_system("");
    assert_eq!(sent[0]["content"], "Let me check the build output.");
}

#[test]
fn no_content_field_on_tool_calls_when_no_partial() {
    let mut state = ConversationState::new();
    let tool_calls = vec![serde_json::json!({
        "id": "call-1",
        "type": "function",
        "function": {"name": "bash", "arguments": "{}"}
    })];
    state.add_tool_calls(tool_calls);
    assert!(state.messages[0].get("content").is_none());
}

#[test]
fn partial_response_does_not_leak_from_text_turn_to_tool_calls() {
    // A completed text turn must not carry its streamed content into a later
    // tool-call turn.
    let mut state = ConversationState::new();
    state.append_partial_response("Final answer text.");
    state.add_assistant_message("Final answer text.");
    assert!(state.current_partial_response.is_empty());
    let tool_calls = vec![serde_json::json!({
        "id": "call-1",
        "type": "function",
        "function": {"name": "bash", "arguments": "{}"}
    })];
    state.add_tool_calls(tool_calls);
    assert!(state.messages[1].get("content").is_none());
}

#[test]
fn reasoning_cleared_on_user_message() {
    let mut state = ConversationState::new();
    state.append_reasoning("Transient thinking");
    state.add_user_message("New question");
    assert!(state.current_reasoning.is_empty());
}

#[test]
fn reasoning_cleared_on_merge_user_message() {
    let mut state = ConversationState::new();
    state.add_user_message("Hello");
    state.append_reasoning("Transient thinking");
    state.merge_user_message("Follow up");
    assert!(state.current_reasoning.is_empty());
}

#[test]
fn reasoning_clear_method() {
    let mut state = ConversationState::new();
    state.append_reasoning("Some thinking");
    state.clear_reasoning();
    assert!(state.current_reasoning.is_empty());
}

#[test]
fn reasoning_not_in_serialized_state() {
    let mut state = ConversationState::new();
    state.append_reasoning("Transient thinking");
    let json = serde_json::to_value(&state).unwrap();
    assert!(json.get("current_reasoning").is_none());
}

#[test]
fn reasoning_stored_in_message_and_survives_roundtrip() {
    let mut state = ConversationState::new();
    state.add_user_message("Hello");
    state.append_reasoning("Let me think...");
    state.add_assistant_message("Hi there");
    state.append_reasoning("Thinking about tools");
    let tool_calls = vec![serde_json::json!({
        "id": "call-1",
        "type": "function",
        "function": {"name": "bash", "arguments": "{}"}
    })];
    state.add_tool_calls(tool_calls);
    state.add_tool_result("call-1", "bash", "output", true);

    let json_str = serde_json::to_string(&state).unwrap();
    let deserialized: ConversationState = serde_json::from_str(&json_str).unwrap();

    assert_eq!(deserialized.messages.len(), 4);
    // Assistant message should have reasoning
    assert_eq!(deserialized.messages[1]["role"], "assistant");
    assert_eq!(deserialized.messages[1]["reasoning"], "Let me think...");
    assert_eq!(deserialized.messages[1]["content"], "Hi there");
    // Tool calls message should have reasoning
    assert_eq!(deserialized.messages[2]["role"], "assistant");
    assert_eq!(deserialized.messages[2]["reasoning"], "Thinking about tools");
    assert!(deserialized.messages[2]["tool_calls"].is_array());
    // current_reasoning should be empty after deserialization
    assert!(deserialized.current_reasoning.is_empty());
}

#[test]
fn reasoning_no_reasoning_field_when_empty() {
    let mut state = ConversationState::new();
    state.add_assistant_message("No reasoning here");
    assert_eq!(state.messages.len(), 1);
    assert!(state.messages[0].get("reasoning").is_none());
}

#[test]
fn build_messages_strips_reasoning() {
    let mut state = ConversationState::new();
    state.add_user_message("Hello");
    state.append_reasoning("My private reasoning");
    state.add_assistant_message("My answer");

    let messages = state.build_messages_with_system("system prompt");
    assert_eq!(messages.len(), 3); // system + user + assistant
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(messages[2]["content"], "My answer");
    assert!(messages[2].get("reasoning").is_none(), "reasoning should be stripped");
}

#[test]
fn build_messages_strips_reasoning_from_tool_calls() {
    let mut state = ConversationState::new();
    state.append_reasoning("Tool reasoning");
    let tool_calls = vec![serde_json::json!({
        "id": "call-1",
        "type": "function",
        "function": {"name": "bash", "arguments": "{}"}
    })];
    state.add_tool_calls(tool_calls);

    let messages = state.build_messages_with_system("");
    assert_eq!(messages.len(), 1);
    assert!(messages[0].get("reasoning").is_none(), "reasoning should be stripped");
    assert!(messages[0]["tool_calls"].is_array());
}

#[test]
fn reasoning_multiple_chunks_concatenated() {
    let mut state = ConversationState::new();
    state.append_reasoning("Hello");
    state.append_reasoning(" world");
    state.add_assistant_message("Answer");
    assert_eq!(state.messages[0]["reasoning"], "Hello world");
}

// =========================================================================
// Tool result ordering tests
// =========================================================================

#[test]
fn tool_results_reordered_to_match_tool_calls() {
    let mut state = ConversationState::new();
    state.add_user_message("test");

    // LLM returns tool calls in order: call-a, call-b, call-c
    let tool_calls = vec![
        serde_json::json!({
            "id": "call-a",
            "type": "function",
            "function": {"name": "tool_a", "arguments": "{}"}
        }),
        serde_json::json!({
            "id": "call-b",
            "type": "function",
            "function": {"name": "tool_b", "arguments": "{}"}
        }),
        serde_json::json!({
            "id": "call-c",
            "type": "function",
            "function": {"name": "tool_c", "arguments": "{}"}
        }),
    ];
    state.add_tool_calls(tool_calls);

    // Results arrive in a different order: call-b, call-c, call-a
    state.add_tool_result("call-b", "tool_b", "result b", true);
    state.resolve_tool_call("call-b");
    // After resolving call-b, not all resolved yet, so no reordering yet
    assert_eq!(state.messages.len(), 3); // user + tool_calls + 1 result
    assert_eq!(state.messages[2]["tool_call_id"], "call-b");

    state.add_tool_result("call-a", "tool_a", "result a", true);
    state.resolve_tool_call("call-a");
    // After resolving call-a, still call-c pending, no reordering yet
    assert_eq!(state.messages.len(), 4); // user + tool_calls + 2 results
    assert_eq!(state.messages[2]["tool_call_id"], "call-b");
    assert_eq!(state.messages[3]["tool_call_id"], "call-a");

    // Last one: call-c - this triggers reordering
    state.add_tool_result("call-c", "tool_c", "result c", true);
    state.resolve_tool_call("call-c");

    // After all resolved, tool results should be reordered to match tool_calls
    // 5 messages: user + assistant(tool_calls) + 3 tool results
    assert_eq!(state.messages.len(), 5);
    assert_eq!(state.messages[2]["tool_call_id"], "call-a");
    assert_eq!(state.messages[3]["tool_call_id"], "call-b");
    assert_eq!(state.messages[4]["tool_call_id"], "call-c");
}

#[test]
fn tool_results_reordered_correct_count() {
    let mut state = ConversationState::new();
    state.add_user_message("test");

    let tool_calls = vec![
        serde_json::json!({
            "id": "call-a",
            "type": "function",
            "function": {"name": "tool_a", "arguments": "{}"}
        }),
        serde_json::json!({
            "id": "call-b",
            "type": "function",
            "function": {"name": "tool_b", "arguments": "{}"}
        }),
        serde_json::json!({
            "id": "call-c",
            "type": "function",
            "function": {"name": "tool_c", "arguments": "{}"}
        }),
    ];
    state.add_tool_calls(tool_calls);

    // Results arrive in reverse order: call-c, call-b, call-a
    state.add_tool_result("call-c", "tool_c", "result c", true);
    state.resolve_tool_call("call-c");
    state.add_tool_result("call-b", "tool_b", "result b", true);
    state.resolve_tool_call("call-b");
    state.add_tool_result("call-a", "tool_a", "result a", true);
    state.resolve_tool_call("call-a");

    // 5 messages: user + assistant(tool_calls) + 3 tool results
    assert_eq!(state.messages.len(), 5);
    assert_eq!(state.messages[0]["role"], "user");
    assert_eq!(state.messages[1]["role"], "assistant");
    assert!(state.messages[1]["tool_calls"].is_array());

    // Tool results should be in original tool_calls order: call-a, call-b, call-c
    assert_eq!(state.messages[2]["tool_call_id"], "call-a");
    assert_eq!(state.messages[2]["content"], "result a");
    assert_eq!(state.messages[3]["tool_call_id"], "call-b");
    assert_eq!(state.messages[3]["content"], "result b");
    assert_eq!(state.messages[4]["tool_call_id"], "call-c");
    assert_eq!(state.messages[4]["content"], "result c");
}

#[test]
fn tool_results_single_call_no_reorder_needed() {
    let mut state = ConversationState::new();
    state.add_user_message("test");

    let tool_calls = vec![serde_json::json!({
        "id": "call-1",
        "type": "function",
        "function": {"name": "bash", "arguments": "{}"}
    })];
    state.add_tool_calls(tool_calls);

    state.add_tool_result("call-1", "bash", "output", true);
    state.resolve_tool_call("call-1");

    assert_eq!(state.messages.len(), 3);
    assert_eq!(state.messages[2]["tool_call_id"], "call-1");
}

#[test]
fn tool_results_two_batches_reorder_independently() {
    let mut state = ConversationState::new();
    state.add_user_message("first query");

    // First batch: call-a, call-b
    let tool_calls_1 = vec![
        serde_json::json!({"id": "call-a", "type": "function", "function": {"name": "tool_a", "arguments": "{}"}}),
        serde_json::json!({"id": "call-b", "type": "function", "function": {"name": "tool_b", "arguments": "{}"}}),
    ];
    state.add_tool_calls(tool_calls_1);

    // Results arrive in reverse order
    state.add_tool_result("call-b", "tool_b", "result b", true);
    state.resolve_tool_call("call-b");
    state.add_tool_result("call-a", "tool_a", "result a", true);
    state.resolve_tool_call("call-a");

    // Check first batch reordering
    assert_eq!(state.messages.len(), 4); // user + assistant + 2 results
    assert_eq!(state.messages[2]["tool_call_id"], "call-a");
    assert_eq!(state.messages[3]["tool_call_id"], "call-b");

    // Second batch: call-c, call-d
    let tool_calls_2 = vec![
        serde_json::json!({"id": "call-c", "type": "function", "function": {"name": "tool_c", "arguments": "{}"}}),
        serde_json::json!({"id": "call-d", "type": "function", "function": {"name": "tool_d", "arguments": "{}"}}),
    ];
    state.add_tool_calls(tool_calls_2);

    // Results arrive in reverse order again
    state.add_tool_result("call-d", "tool_d", "result d", true);
    state.resolve_tool_call("call-d");
    state.add_tool_result("call-c", "tool_c", "result c", true);
    state.resolve_tool_call("call-c");

    // Check second batch reordering
    assert_eq!(state.messages.len(), 7); // user + assistant + 2 results + assistant + 2 results
    assert_eq!(state.messages[5]["tool_call_id"], "call-c");
    assert_eq!(state.messages[6]["tool_call_id"], "call-d");
}

#[test]
fn tool_call_order_not_serialized() {
    let mut state = ConversationState::new();
    let tool_calls = vec![serde_json::json!({
        "id": "call-1",
        "type": "function",
        "function": {"name": "bash", "arguments": "{}"}
    })];
    state.add_tool_calls(tool_calls);
    let json = serde_json::to_value(&state).unwrap();
    assert!(json.get("tool_call_order").is_none());
}

// =========================================================================
// PersistenceActor tests
// =========================================================================

#[tokio::test]
async fn persistence_actor_announce() {
    let actor = gladiator_agent::PersistenceActor::new(
        0,
        "persistence:command".to_string(),
        "persistence:response".to_string(),
        "agent:state_control".to_string(),
        "agent:state".to_string(),
    );
    let ann = actor.announce();
    assert_eq!(ann.id, "gladiator-persistence-0");
    assert!(ann.subscriptions.contains(&"persistence:command".to_string()));
    assert!(ann.subscriptions.contains(&"agent:state".to_string()));
    assert!(ann.publications.contains(&"persistence:response".to_string()));
    assert!(ann.publications.contains(&"agent:state_control".to_string()));
}

#[tokio::test]
async fn persistence_actor_multiple_instances() {
    let actor0 = gladiator_agent::PersistenceActor::new(
        0, "persistence:command".to_string(), "persistence:response".to_string(),
        "agent:state_control".to_string(), "agent:state".to_string(),
    );
    let actor1 = gladiator_agent::PersistenceActor::new(
        1, "persistence:command".to_string(), "persistence:response".to_string(),
        "agent:state_control".to_string(), "agent:state".to_string(),
    );
    assert_eq!(actor0.announce().id, "gladiator-persistence-0");
    assert_eq!(actor1.announce().id, "gladiator-persistence-1");
}

// =========================================================================
// Context-usage tracking
// =========================================================================

#[test]
fn context_remaining_unknown_when_window_unset() {
    let state = ConversationState::new();
    assert_eq!(state.context_remaining(), None);
    assert!(state.context_status_line().contains("no usage reported"));
}

#[test]
fn context_remaining_with_window_but_no_usage() {
    let mut state = ConversationState::new();
    state.context_window = Some(128_000);
    // No usage reported yet — remaining is the full window, but status line
    // still nudges that no usage has been seen.
    assert_eq!(state.context_remaining(), Some(128_000));
    assert!(state.context_status_line().contains("no usage reported"));
}

#[test]
fn context_remaining_subtracts_input_tokens() {
    let mut state = ConversationState::new();
    state.context_window = Some(128_000);
    state.record_usage(
        Usage {
            input_tokens: Some(32_000),
            output_tokens: Some(1_000),
            total_tokens: Some(33_000),
            reasoning_tokens: None,
        },
        None,
    );
    assert_eq!(state.context_remaining(), Some(96_000));
    let line = state.context_status_line();
    assert!(line.contains("32000"), "line missing used tokens: {}", line);
    assert!(line.contains("128000"), "line missing window: {}", line);
    assert!(line.contains("96000"), "line missing remaining: {}", line);
    assert!(line.contains("25%"), "line missing percentage: {}", line);
}

#[test]
fn record_usage_propagates_context_window_from_stats() {
    let mut state = ConversationState::new();
    // A StreamStats message may carry the window even if the agent was seeded
    // without one at startup (e.g. probe raced and arrived late).
    state.record_usage(
        Usage {
            input_tokens: Some(100),
            output_tokens: None,
            total_tokens: None,
            reasoning_tokens: None,
        },
        Some(8192),
    );
    assert_eq!(state.context_window, Some(8192));
    assert_eq!(state.context_remaining(), Some(8092));
}

#[test]
fn context_status_line_without_window_shows_input_only() {
    let mut state = ConversationState::new();
    state.record_usage(
        Usage {
            input_tokens: Some(500),
            output_tokens: None,
            total_tokens: None,
            reasoning_tokens: None,
        },
        None,
    );
    let line = state.context_status_line();
    assert!(line.contains("500"), "missing token count: {}", line);
    assert!(line.contains("window unknown"), "expected 'window unknown': {}", line);
}

#[test]
fn context_reminder_fires_once_when_threshold_crossed() {
    let mut state = ConversationState::new();
    state.add_context_reminder(150_000, "Do a context refresh now.".into());
    // Below threshold — no fire.
    let injected = state.check_context_reminders(100_000);
    assert!(injected.is_empty());
    assert!(!state.context_reminders[0].fired);
    // Above threshold — fires once.
    let injected = state.check_context_reminders(160_000);
    assert_eq!(injected.len(), 1);
    assert_eq!(injected[0], "Do a context refresh now.");
    assert!(state.context_reminders[0].fired);
    // Second call at same/higher level — does not fire again.
    let injected = state.check_context_reminders(200_000);
    assert!(injected.is_empty());
}

#[test]
fn context_reminder_update_rearms() {
    let mut state = ConversationState::new();
    state.add_context_reminder(100, "first message".into());
    // Fire it.
    let injected = state.check_context_reminders(200);
    assert_eq!(injected.len(), 1);
    assert!(state.context_reminders[0].fired);
    // Update at same threshold — re-arms (fired=false).
    state.add_context_reminder(100, "second message".into());
    assert!(!state.context_reminders[0].fired);
    assert_eq!(state.context_reminders.len(), 1); // replaced, not added
}

#[test]
fn context_reminder_injects_into_pending() {
    let mut state = ConversationState::new();
    state.add_context_reminder(50, "Wake up!".into());
    let injected = state.check_context_reminders(60);
    assert_eq!(injected.len(), 1);
    // The message should also be in pending_messages.
    assert_eq!(state.pending_messages.len(), 1);
    assert_eq!(state.pending_messages[0], "Wake up!");
}

#[test]
fn one_shot_wake_up_fires_when_idle() {
    let mut state = ConversationState::new();
    // Schedule a wake-up 0 seconds from now (immediate).
    state.add_one_shot_wake_up(0, "Wake up!".into());
    assert_eq!(state.wake_ups.len(), 1);
    // When idle and due, it should fire.
    let fired = state.check_wake_ups();
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0], "Wake up!");
    // One-shot is removed after firing.
    assert!(state.wake_ups.is_empty());
}

#[test]
fn cron_wake_up_reschedules_after_firing() {
    let mut state = ConversationState::new();
    state.add_cron_wake_up(0, 60, "Tick".into());
    assert_eq!(state.wake_ups.len(), 1);
    // Fire it.
    let fired = state.check_wake_ups();
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0], "Tick");
    // Cron wake-up should still be in the list (rescheduled).
    assert_eq!(state.wake_ups.len(), 1);
}

#[test]
fn wake_up_deferred_when_not_idle() {
    let mut state = ConversationState::new();
    state.add_one_shot_wake_up(0, "Wake up!".into());
    // Simulate busy loop.
    state.inference_in_flight = true;
    // Should not fire when busy — one-shot is deferred (rescheduled +1s).
    let fired = state.check_wake_ups();
    assert!(fired.is_empty());
    assert_eq!(state.wake_ups.len(), 1); // still there, rescheduled
}

#[test]
fn wake_up_injects_into_pending() {
    let mut state = ConversationState::new();
    state.add_one_shot_wake_up(0, "Do something".into());
    let fired = state.check_wake_ups();
    assert_eq!(fired.len(), 1);
    // Should be in pending_messages.
    assert_eq!(state.pending_messages.len(), 1);
    assert_eq!(state.pending_messages[0], "Do something");
}

// =========================================================================
// Subagent tests — push/pop mechanics, depth tracking, system message override
// =========================================================================

#[test]
fn subagent_depth_defaults_zero() {
    let state = ConversationState::new();
    assert_eq!(state.subagent_depth, 0);
    assert!(state.active_system_message.is_none());
}

#[test]
fn subagent_active_system_message_overrides_build_messages() {
    let mut state = ConversationState::new();
    state.add_user_message("hello");
    // Without override — uses passed-in system message
    let msgs = state.build_messages_with_system("default prompt");
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[0]["content"], "default prompt");

    // With override — active_system_message takes precedence
    state.active_system_message = Some("subagent prompt".to_string());
    let msgs2 = state.build_messages_with_system("should be ignored");
    assert_eq!(msgs2[0]["role"], "system");
    assert_eq!(msgs2[0]["content"], "subagent prompt");
}

#[test]
fn subagent_clear_for_restart_preserves_depth() {
    let mut state = ConversationState::new();
    state.subagent_depth = 1;
    state.active_system_message = Some("inner".to_string());
    state.add_user_message("before clear");

    // clear_for_restart should NOT touch depth/active_system_message
    state.clear_for_restart();

    assert_eq!(state.messages.len(), 0);
    assert_eq!(state.subagent_depth, 1); // preserved!
    assert_eq!(state.active_system_message, Some("inner".to_string()));
}

#[test]
fn subagent_conversation_state_clone_includes_subagent_fields() {
    let mut state = ConversationState::new();
    state.add_user_message("parent msg");
    state.subagent_depth = 2;
    state.active_system_message = Some("sub prompt".into());

    // Clone should preserve these (they're not serialized but ARE cloned)
    let clone = state.clone();

    assert_eq!(clone.messages.len(), 1);
    assert_eq!(clone.subagent_depth, 2);
    assert_eq!(clone.active_system_message.as_ref().unwrap(), "sub prompt");
}

#[test]
fn subagent_frame_constructs() {
    // Verify SubagentFrame is exported and constructable
    let state = ConversationState::new();
    let frame = gladiator_agent::SubagentFrame {
        saved_state: state,
        saved_system_message: "parent system".to_string(),
    };
    assert_eq!(frame.saved_system_message, "parent system");
}

#[test]
fn subagent_call_subagent_is_internal_tool() {
    // Verify call_subagent is registered as an internal tool
    assert!(!gladiator_agent::is_internal_tool("not_a_real_tool"));
    assert!(
        gladiator_agent::is_internal_tool("call_subagent"),
        "call_subagent should be recognized as an internal tool"
    );
}

#[test]
fn subagent_call_subagent_in_defs() {
    let defs = gladiator_agent::internal_tools::internal_tool_defs();
    let names: Vec<&str> = defs
        .iter()
        .map(|d| d["function"]["name"].as_str().unwrap())
        .collect();

    assert!(
        names.contains(&"call_subagent"),
        "call_subagent should appear in internal tool definitions"
    );

    // Verify the definition has required fields (task)
    let def = defs
        .iter()
        .find(|d| d["function"]["name"] == "call_subagent")
        .unwrap();
    assert!(def["function"]["parameters"]["properties"]["task"].is_object());
}

#[test]
fn subagent_depth_not_serialized() {
    // Transient fields should not appear in serialized state
    let mut state = ConversationState::new();
    state.subagent_depth = 3;
    state.active_system_message = Some("inner".into());

    let json_str = serde_json::to_string(&state).unwrap();

    assert!(
        !json_str.contains("\"subagent_depth\""),
        "depth should not be serialized"
    );
    assert!(
        !json_str.contains("\"active_system_message\""),
        "active_system_message should not be serialized"
    );

    // Deserialized state has defaults
    let deserialized: ConversationState = serde_json::from_str(&json_str).unwrap();
    assert_eq!(deserialized.subagent_depth, 0);
}

#[test]
fn subagent_build_messages_with_empty_active_and_default() {
    // When active_system_message is None and system_message is empty,
    // no system message should be added
    let mut state = ConversationState::new();
    state.add_user_message("hi");

    let msgs = state.build_messages_with_system("");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "user");
}

#[test]
fn subagent_build_messages_falls_back_when_active_none() {
    // When active_system_message is None, fall back to passed-in system message
    let mut state = ConversationState::new();
    state.add_user_message("hi");

    let msgs = state.build_messages_with_system("fallback prompt");
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[0]["content"], "fallback prompt");
}
