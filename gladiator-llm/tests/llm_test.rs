use gladiator_core::*;
use gladiator_llm::*;

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
// Config merge (2-level config) tests
// =========================================================================

#[test]
fn config_merge_override_takes_precedence() {
    let base = LlmConfig {
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
    let override_cfg = LlmConfig {
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
    let base = LlmConfig::default();
    let merged = merge_config(&base, None);
    assert_eq!(merged.model, base.model);
    assert_eq!(merged.base_url, base.base_url);
}

// =========================================================================
// LlmActor tests
// =========================================================================

#[tokio::test]
async fn llm_actor_announce() {
    let actor = LlmActor::new(
        0,
        "llm:in".to_string(),
        "llm:out".to_string(),
        "llm:stream".to_string(),
        "llm:stats".to_string(),
        "llm:tool_calls".to_string(),
        "user:control".to_string(),
        LlmConfig::default(),
    );
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
    // Test that multiple instances can be created with different indices
    let actor0 = LlmActor::new(
        0, "llm:0:in".to_string(), "llm:0:out".to_string(),
        "llm:0:stream".to_string(), "llm:0:stats".to_string(),
        "llm:0:tool_calls".to_string(), "user:control".to_string(),
        LlmConfig::default(),
    );
    let actor1 = LlmActor::new(
        1, "llm:1:in".to_string(), "llm:1:out".to_string(),
        "llm:1:stream".to_string(), "llm:1:stats".to_string(),
        "llm:1:tool_calls".to_string(), "user:control".to_string(),
        LlmConfig::default(),
    );
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
        LlmConfig::default(),
    );
    let messages = vec![serde_json::json!({"role": "user", "content": "hi"})];
    let tools = vec![serde_json::json!({"type": "function", "function": {"name": "bash", "description": "Run", "parameters": {"type": "object"}}})];
    let body = actor.build_request_body(&LlmConfig::default(), &messages, Some(&tools), None);
    assert_eq!(body["model"], LlmConfig::default().model);
    assert_eq!(body["messages"][0]["role"], "user");
    assert!(body["tools"].is_array());
    assert_eq!(body["stream"], true);
}

#[test]
fn build_request_body_without_tools() {
    let actor = LlmActor::new(
        0, String::new(), String::new(), String::new(),
        String::new(), String::new(), String::new(),
        LlmConfig::default(),
    );
    let messages = vec![serde_json::json!({"role": "user", "content": "hi"})];
    let body = actor.build_request_body(&LlmConfig::default(), &messages, None, None);
    assert!(body["tools"].is_null());
    assert_eq!(body["stream"], true);
}
