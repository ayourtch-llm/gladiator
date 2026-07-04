use gladiator_core::Actor;
use gladiator_llm::*;

fn create_test_actor(index: usize) -> LlmActor {
    LlmActor::new(
        index,
        "llm:in".to_string(),
        "llm:out".to_string(),
        "llm:stream".to_string(),
        "llm:stats".to_string(),
        "llm:tool_calls".to_string(),
        "user:control".to_string(),
        gladiator_core::LlmConfig::default(),
    )
}

// =========================================================================
// LlmEvent tests
// =========================================================================

#[test]
fn llm_event_text_delta() {
    let event = LlmEvent::TextDelta {
        id: "text-0".to_string(),
        text: "Hello".to_string(),
    };
    match &event {
        LlmEvent::TextDelta { text, .. } => assert_eq!(text, "Hello"),
        _ => panic!("Expected TextDelta"),
    }
}

#[test]
fn llm_event_reasoning_delta() {
    let event = LlmEvent::ReasoningDelta {
        id: "reasoning-0".to_string(),
        text: "Let me think...".to_string(),
    };
    match &event {
        LlmEvent::ReasoningDelta { text, .. } => assert_eq!(text, "Let me think..."),
        _ => panic!("Expected ReasoningDelta"),
    }
}

#[test]
fn llm_event_tool_call() {
    let event = LlmEvent::ToolCall {
        id: "call-1".to_string(),
        name: "bash".to_string(),
        input: serde_json::json!({"command": "ls"}),
    };
    match &event {
        LlmEvent::ToolCall { name, .. } => assert_eq!(name, "bash"),
        _ => panic!("Expected ToolCall"),
    }
}

#[test]
fn llm_event_finish() {
    let event = LlmEvent::Finish {
        reason: "stop".to_string(),
        usage: Some(Usage {
            input_tokens: Some(100),
            output_tokens: Some(50),
            total_tokens: Some(150),
            reasoning_tokens: None,
        }),
    };
    match &event {
        LlmEvent::Finish { reason, .. } => assert_eq!(reason, "stop"),
        _ => panic!("Expected Finish"),
    }
}

// =========================================================================
// LlmResponse tests
// =========================================================================

#[test]
fn llm_response_reduce_text() {
    let mut response = LlmResponse::default();
    response.reduce(LlmEvent::TextDelta {
        id: "text-0".to_string(),
        text: "Hello ".to_string(),
    });
    response.reduce(LlmEvent::TextDelta {
        id: "text-0".to_string(),
        text: "World".to_string(),
    });
    assert_eq!(response.text, "Hello World");
}

#[test]
fn llm_response_reduce_reasoning() {
    let mut response = LlmResponse::default();
    response.reduce(LlmEvent::ReasoningDelta {
        id: "reasoning-0".to_string(),
        text: "Thinking...".to_string(),
    });
    assert_eq!(response.reasoning, "Thinking...");
}

#[test]
fn llm_response_reduce_tool_call() {
    let mut response = LlmResponse::default();
    response.reduce(LlmEvent::ToolCall {
        id: "call-1".to_string(),
        name: "bash".to_string(),
        input: serde_json::json!({"command": "ls"}),
    });
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0]["name"], "bash");
}

#[test]
fn llm_response_finish() {
    let mut response = LlmResponse::default();
    response.reduce(LlmEvent::Finish {
        reason: "stop".to_string(),
        usage: Some(Usage {
            input_tokens: Some(100),
            output_tokens: Some(50),
            total_tokens: Some(150),
            reasoning_tokens: None,
        }),
    });
    assert!(response.is_complete());
    assert_eq!(response.finish_reason, Some("stop".to_string()));
}

// =========================================================================
// ToolStream tests
// =========================================================================

#[test]
fn tool_stream_append_or_start() {
    let mut tool_stream = ToolStream::new();
    tool_stream.append_or_start(0, "call-1", "bash", "{\"cmd\": \"ls\"}");
    let result = tool_stream.finish_all();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0]["id"], "call-1");
    assert_eq!(result[0]["function"]["name"], "bash");
}

#[test]
fn tool_stream_multiple_calls() {
    let mut tool_stream = ToolStream::new();
    tool_stream.append_or_start(0, "call-1", "bash", "{\"cmd\": \"ls\"}");
    tool_stream.append_or_start(1, "call-2", "read", "{\"file\": \"test.txt\"}");
    let result = tool_stream.finish_all();
    assert_eq!(result.len(), 2);
}

// =========================================================================
// SseFraming tests
// =========================================================================

#[test]
fn decode_sse_chunk_basic() {
    let chunk = b"data: {\"text\": \"hello\"}\n\ndata: {\"text\": \"world\"}\n";
    let payloads = decode_sse_chunk(chunk);
    assert_eq!(payloads.len(), 2);
    assert_eq!(payloads[0], "{\"text\": \"hello\"}");
    assert_eq!(payloads[1], "{\"text\": \"world\"}");
}

#[test]
fn decode_sse_chunk_skips_done() {
    let chunk = b"data: {\"text\": \"hello\"}\n\ndata: [DONE]\n";
    let payloads = decode_sse_chunk(chunk);
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0], "{\"text\": \"hello\"}");
}

#[test]
fn decode_sse_chunk_skips_empty() {
    let chunk = b"data: \n\ndata: {\"text\": \"hello\"}\n";
    let payloads = decode_sse_chunk(chunk);
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0], "{\"text\": \"hello\"}");
}

// =========================================================================
// OpenAIChatProtocol tests
// =========================================================================

#[test]
fn openai_chat_build_body() {
    let protocol = OpenAIChatProtocol;
    let request = CanonicalRequest {
        model: "gpt-4".to_string(),
        messages: vec![serde_json::json!({"role": "user", "content": "hello"})],
        tools: None,
        tool_choice: None,
        temperature: Some(0.7),
        max_tokens: Some(1000),
        stream: true,
        grammar: None,
    };
    let body = protocol.build_body(&request);
    assert_eq!(body["model"], "gpt-4");
    assert_eq!(body["stream"], true);
    // Use approximate comparison for floating point
    let temp = body["temperature"].as_f64().unwrap();
    assert!((temp - 0.7).abs() < 0.001);
}

#[test]
fn openai_chat_build_body_with_tools() {
    let protocol = OpenAIChatProtocol;
    let request = CanonicalRequest {
        model: "gpt-4".to_string(),
        messages: vec![serde_json::json!({"role": "user", "content": "hello"})],
        tools: Some(vec![serde_json::json!({"type": "function", "function": {"name": "bash", "description": "Run bash", "parameters": {"type": "object"}}})]),
        tool_choice: None,
        temperature: Some(0.7),
        max_tokens: Some(1000),
        stream: true,
        grammar: None,
    };
    let body = protocol.build_body(&request);
    assert!(body["tools"].is_array());
}

#[test]
fn openai_chat_parse_event_text() {
    let protocol = OpenAIChatProtocol;
    let mut state = StreamState::default();
    let raw = serde_json::json!({
        "choices": [{
            "delta": {"content": "Hello"}
        }]
    });
    let events = protocol.parse_event(&raw, &mut state);
    assert!(!events.is_empty());
    assert!(events.iter().any(|e| matches!(e, LlmEvent::TextDelta { text, .. } if text == "Hello")));
}

#[test]
fn openai_chat_parse_event_reasoning() {
    let protocol = OpenAIChatProtocol;
    let mut state = StreamState::default();
    let raw = serde_json::json!({
        "choices": [{
            "delta": {"reasoning_content": "Let me think..."}
        }]
    });
    let events = protocol.parse_event(&raw, &mut state);
    assert!(!events.is_empty());
    assert!(events.iter().any(|e| matches!(e, LlmEvent::ReasoningDelta { text, .. } if text == "Let me think...")));
}

#[test]
fn openai_chat_parse_event_tool_call() {
    let protocol = OpenAIChatProtocol;
    let mut state = StreamState::default();
    let raw = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call-1",
                    "function": {
                        "name": "bash",
                        "arguments": "{\"cmd\": \"ls\"}"
                    }
                }]
            }
        }]
    });
    let events = protocol.parse_event(&raw, &mut state);
    assert!(!events.is_empty());
    assert!(events.iter().any(|e| matches!(e, LlmEvent::ToolInputStart { id, name, .. } if id == "call-1" && name == "bash")));
    assert_eq!(state.tool_calls.len(), 1);
    assert_eq!(state.tool_calls[0]["id"], "call-1");
    assert_eq!(state.tool_calls[0]["function"]["name"], "bash");
    assert_eq!(state.tool_calls[0]["function"]["arguments"], "{\"cmd\": \"ls\"}");
}

#[test]
fn openai_chat_parse_event_tool_call_streaming() {
    let protocol = OpenAIChatProtocol;
    let mut state = StreamState::default();

    // First chunk: has id, name, and partial arguments
    let chunk1 = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call-1",
                    "function": {
                        "name": "random_integer",
                        "arguments": "{\"min"
                    }
                }]
            }
        }]
    });
    let _events1 = protocol.parse_event(&chunk1, &mut state);

    // Second chunk: same index, no id, no name, continuation of arguments
    let chunk2 = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "function": {
                        "arguments": "\": 1, \"max\": 10}"
                    }
                }]
            }
        }]
    });
    let _events2 = protocol.parse_event(&chunk2, &mut state);

    // state.tool_calls should have a single entry with accumulated arguments
    assert_eq!(state.tool_calls.len(), 1);
    assert_eq!(state.tool_calls[0]["id"], "call-1");
    assert_eq!(state.tool_calls[0]["function"]["name"], "random_integer");
    let args = state.tool_calls[0]["function"]["arguments"].as_str().unwrap();
    assert_eq!(args, "{\"min\": 1, \"max\": 10}");
}

#[test]
fn openai_chat_parse_event_finish() {
    let protocol = OpenAIChatProtocol;
    let mut state = StreamState::default();
    let raw = serde_json::json!({
        "choices": [{
            "finish_reason": "stop"
        }]
    });
    let events = protocol.parse_event(&raw, &mut state);
    assert!(!events.is_empty());
    assert!(events.iter().any(|e| matches!(e, LlmEvent::Finish { reason, .. } if reason == "stop")));
    assert!(state.finish_reason.is_some());
}

#[test]
fn openai_chat_parse_event_usage() {
    let protocol = OpenAIChatProtocol;
    let mut state = StreamState::default();
    let raw = serde_json::json!({
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150
        }
    });
    let _events = protocol.parse_event(&raw, &mut state);
    assert!(state.usage.is_some());
    let usage = state.usage.as_ref().unwrap();
    assert_eq!(usage.input_tokens, Some(100));
    assert_eq!(usage.output_tokens, Some(50));
}

// =========================================================================
// Config merge tests
// =========================================================================

#[test]
fn config_merge_override_takes_precedence() {
    let base = gladiator_core::LlmConfig {
        model: "base-model".to_string(),
        base_url: "http://base/v1".to_string(),
        api_key: "base-key".to_string(),
        temperature: 0.7,
        max_tokens: 4096,
        request_timeout_secs: 120,
        stream_timeout_secs: 300,
        max_retries: 3,
        retry_base_delay_ms: 500,
    };
    let override_cfg = gladiator_core::LlmConfig {
        model: "override-model".to_string(),
        base_url: String::new(),
        api_key: String::new(),
        temperature: 0.5,
        max_tokens: 0,
        request_timeout_secs: 0,
        stream_timeout_secs: 0,
        max_retries: 0,
        retry_base_delay_ms: 0,
    };
    let merged = merge_config(&base, Some(&override_cfg));
    assert_eq!(merged.model, "override-model");
    assert_eq!(merged.base_url, "http://base/v1");
    assert_eq!(merged.api_key, "base-key");
    assert_eq!(merged.temperature, 0.5);
    assert_eq!(merged.max_tokens, 4096);
}

#[test]
fn config_merge_no_override_returns_base() {
    let base = gladiator_core::LlmConfig::default();
    let merged = merge_config(&base, None);
    assert_eq!(merged.model, base.model);
    assert_eq!(merged.base_url, base.base_url);
}

// =========================================================================
// LlmRequest tests
// =========================================================================

#[test]
fn llm_request_serde() {
    let req = LlmRequest {
        messages: Some(vec![
            serde_json::json!({"role": "system", "content": "You are a coder"}),
            serde_json::json!({"role": "user", "content": "hello"}),
        ]),
        prompt: String::new(),
        config: None,
        tools: None,
        grammar: None,
    };
    let json = serde_json::to_string(&req).unwrap();
    let decoded: LlmRequest = serde_json::from_str(&json).unwrap();
    assert!(decoded.messages.is_some());
    assert_eq!(decoded.messages.as_ref().unwrap().len(), 2);
}

#[test]
fn llm_request_with_tools() {
    let req = LlmRequest {
        messages: Some(vec![serde_json::json!({"role": "user", "content": "test"})]),
        prompt: String::new(),
        config: None,
        tools: Some(vec![
            serde_json::json!({"type": "function", "function": {"name": "bash", "description": "Run bash", "parameters": {"type": "object"}}}),
        ]),
        grammar: None,
    };
    assert!(req.tools.is_some());
    assert_eq!(req.tools.as_ref().unwrap().len(), 1);
}

// =========================================================================
// LlmError tests
// =========================================================================

#[test]
fn llm_error_retryable() {
    assert!(LlmError::Network("conn refused".to_string()).is_retryable());
    assert!(LlmError::Timeout(60).is_retryable());
    assert!(LlmError::Api { status: 429, body: "rate limited".to_string() }.is_retryable());
    assert!(LlmError::Api { status: 503, body: "server error".to_string() }.is_retryable());
    assert!(!LlmError::Api { status: 400, body: "bad request".to_string() }.is_retryable());
    assert!(!LlmError::StreamInterrupted(100, "timeout".to_string()).is_retryable());
}

// =========================================================================
// LlmActor tests
// =========================================================================

#[tokio::test]
async fn llm_actor_announce() {
    let actor = create_test_actor(0);
    let ann = actor.announce();
    assert_eq!(ann.id, "gladiator-llm-0");
    assert!(ann.subscriptions.contains(&"llm:in".to_string()));
    assert!(ann.subscriptions.contains(&"user:control".to_string()));
    assert!(ann.publications.contains(&"llm:out".to_string()));
    assert!(ann.publications.contains(&"llm:stream".to_string()));
    assert!(ann.publications.contains(&"llm:tool_calls".to_string()));
}

#[tokio::test]
async fn llm_actor_multiple_instances() {
    let actor0 = create_test_actor(0);
    let actor1 = create_test_actor(1);
    assert_eq!(actor0.announce().id, "gladiator-llm-0");
    assert_eq!(actor1.announce().id, "gladiator-llm-1");
    assert_ne!(actor0.announce().id, actor1.announce().id);
}

// =========================================================================
// Request body building tests
// =========================================================================

#[test]
fn build_request_body_includes_tools() {
    let actor = LlmActor::new(
        0, String::new(), String::new(), String::new(),
        String::new(), String::new(), String::new(),
        gladiator_core::LlmConfig::default(),
    );
    let messages = vec![serde_json::json!({"role": "user", "content": "hi"})];
    let tools = vec![serde_json::json!({"type": "function", "function": {"name": "bash", "description": "Run", "parameters": {"type": "object"}}})];
    let body = actor.build_request_body(&gladiator_core::LlmConfig::default(), &messages, Some(&tools), None);
    assert_eq!(body["model"], gladiator_core::LlmConfig::default().model);
    assert_eq!(body["messages"][0]["role"], "user");
    assert!(body["tools"].is_array());
    assert_eq!(body["stream"], true);
}

#[test]
fn build_request_body_without_tools() {
    let actor = LlmActor::new(
        0, String::new(), String::new(), String::new(),
        String::new(), String::new(), String::new(),
        gladiator_core::LlmConfig::default(),
    );
    let messages = vec![serde_json::json!({"role": "user", "content": "hi"})];
    let body = actor.build_request_body(&gladiator_core::LlmConfig::default(), &messages, None, None);
    assert!(body["tools"].is_null());
    assert_eq!(body["stream"], true);
}
