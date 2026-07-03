use gladiator_core::*;
use gladiator_tools::*;
use async_trait::async_trait;

// =========================================================================
// ToolSyntax tests
// =========================================================================

#[test]
fn tool_syntax_new() {
    let syntax = ToolSyntax::new(
        "bash".to_string(),
        "Run a bash command".to_string(),
        serde_json::json!({"type": "object", "properties": {}}),
    );
    assert_eq!(syntax.name, "bash");
    assert_eq!(syntax.description, "Run a bash command");
}

#[test]
fn tool_syntax_to_openai_json() {
    let syntax = ToolSyntax::new(
        "read".to_string(),
        "Read a file".to_string(),
        serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
    );
    let json = syntax.to_openai_json();
    assert_eq!(json["type"], "function");
    assert_eq!(json["function"]["name"], "read");
    assert_eq!(json["function"]["description"], "Read a file");
    assert!(json["function"]["parameters"].is_object());
}

// =========================================================================
// ToolExecuteMessage / ToolResultMessage tests
// =========================================================================

#[test]
fn tool_execute_message_serde() {
    let msg = ToolExecuteMessage {
        tool_call_id: "call-1".to_string(),
        tool_name: "bash".to_string(),
        arguments: serde_json::json!({"command": "ls"}),
    };
    let json = serde_json::to_string(&msg).unwrap();
    let decoded: ToolExecuteMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.tool_call_id, "call-1");
    assert_eq!(decoded.tool_name, "bash");
    assert_eq!(decoded.arguments["command"], "ls");
}

#[test]
fn tool_result_message_serde() {
    let msg = ToolResultMessage {
        tool_call_id: "call-1".to_string(),
        tool_name: "bash".to_string(),
        success: true,
        result: "hello".to_string(),
        error: None,
    };
    let json = serde_json::to_string(&msg).unwrap();
    let decoded: ToolResultMessage = serde_json::from_str(&json).unwrap();
    assert!(decoded.success);
    assert_eq!(decoded.result, "hello");
    assert!(decoded.error.is_none());
}

// =========================================================================
// Tool trait tests
// =========================================================================

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str { "echo" }
    fn description(&self) -> &str { "Echoes back the input" }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]})
    }
    async fn execute(&self, args: &serde_json::Value) -> Result<String, String> {
        Ok(args["text"].as_str().unwrap_or("").to_string())
    }
}

#[test]
fn tool_trait_basic() {
    let tool = EchoTool;
    assert_eq!(tool.name(), "echo");
    assert_eq!(tool.description(), "Echoes back the input");
    assert!(tool.parameters().is_object());
}

#[tokio::test]
async fn tool_trait_execute() {
    let tool = EchoTool;
    let result = tool.execute(&serde_json::json!({"text": "hello"})).await;
    assert_eq!(result.unwrap(), "hello");
}

// =========================================================================
// ToolActorRunner roundtrip via bus
// =========================================================================

#[tokio::test]
async fn tool_runner_roundtrip() {
    let bus = Bus::new();
    bus.create_topic("tool:results", 100).await;
    bus.create_topic("tool:echo:execute", 100).await;
    bus.create_topic("user:control", 100).await;

    // Spawn the tool runner
    let runner = ToolActorRunner::new(EchoTool);
    let bus_clone = bus.clone();
    tokio::spawn(async move {
        if let Err(e) = runner.run(&bus_clone).await {
            eprintln!("Tool runner error: {}", e);
        }
    });

    // Subscribe to results
    let bus2 = bus.clone();
    let result_task = tokio::spawn(async move {
        let mut rx = bus2.subscribe_stream("tool:results").await.unwrap();
        rx.recv().await.unwrap()
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Register a publisher and send execute message
    bus.register_announcement(ActorAnnouncement {
        id: "test-sender".to_string(),
        subscriptions: vec![],
        publications: vec!["tool:echo:execute".to_string()],
    }).await;

    let exec_msg = ToolExecuteMessage {
        tool_call_id: "tc-1".to_string(),
        tool_name: "echo".to_string(),
        arguments: serde_json::json!({"text": "roundtrip"}),
    };
    let msg = Message::new(
        "tool:echo:execute",
        "test-sender",
        serde_json::to_value(&exec_msg).unwrap(),
    );
    bus.publish("test-sender", msg).await.unwrap();

    let received = result_task.await.unwrap();
    let result: ToolResultMessage = serde_json::from_value(received.payload).unwrap();
    assert!(result.success);
    assert_eq!(result.result, "roundtrip");
    assert_eq!(result.tool_call_id, "tc-1");
}

// =========================================================================
// ToolRegistry tests
// =========================================================================

#[test]
fn tool_registry_add_and_get() {
    let mut registry = ToolRegistry::new();
    registry.add(Box::new(EchoTool));
    assert_eq!(registry.len(), 1);
    let syntaxes = registry.syntaxes();
    assert_eq!(syntaxes[0].name, "echo");
}

#[test]
fn tool_registry_to_openai_json() {
    let mut registry = ToolRegistry::new();
    registry.add(Box::new(EchoTool));
    let json = registry.to_openai_json();
    assert!(json.is_array());
    assert_eq!(json[0]["function"]["name"], "echo");
}

#[test]
fn tool_registry_rejects_duplicates() {
    let mut registry = ToolRegistry::new();
    assert!(registry.add(Box::new(EchoTool)));
    assert!(!registry.add(Box::new(EchoTool)));
    assert_eq!(registry.len(), 1);
}
