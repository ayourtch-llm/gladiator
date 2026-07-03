use gladiator_core::*;

// =========================================================================
// Bus tests
// =========================================================================

#[tokio::test]
async fn bus_create_and_list_topics() {
    let bus = Bus::new();
    bus.create_topic("test:topic", 100).await;
    let topics = bus.list_topics().await;
    assert!(topics.contains(&"test:topic".to_string()));
}

#[tokio::test]
async fn bus_publish_subscribe() {
    let bus = Bus::new();
    bus.create_topic("test:pubsub", 100).await;
    bus.register_announcement(ActorAnnouncement {
        id: "publisher".to_string(),
        subscriptions: vec![],
        publications: vec!["test:pubsub".to_string()],
    })
    .await;

    let bus2 = bus.clone();
    let rx_task = tokio::spawn(async move {
        let bus_clone = bus2.clone();
        let sub_bus = bus_clone.clone();
        let mut rx = sub_bus.subscribe("subscriber", "test:pubsub").await.unwrap();
        rx.recv().await.unwrap()
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let msg = Message::text("test:pubsub", "publisher", "hello world");
    bus.publish("publisher", msg).await.unwrap();

    let received = rx_task.await.unwrap();
    assert_eq!(received.topic, "test:pubsub");
    assert_eq!(received.source, "publisher");
    assert_eq!(
        received.payload,
        serde_json::Value::String("hello world".to_string())
    );
}

#[tokio::test]
async fn bus_unannounced_actor_cannot_publish() {
    let bus = Bus::new();
    bus.create_topic("test:restricted", 100).await;
    let result = bus
        .publish("unknown", Message::text("test:restricted", "unknown", "msg"))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn bus_multiple_subscribers() {
    let bus = Bus::new();
    bus.create_topic("test:multi", 100).await;
    bus.register_announcement(ActorAnnouncement {
        id: "pub".to_string(),
        subscriptions: vec![],
        publications: vec!["test:multi".to_string()],
    })
    .await;

    let mut rx1 = bus.subscribe("sub1", "test:multi").await.unwrap();
    let mut rx2 = bus.subscribe("sub2", "test:multi").await.unwrap();

    bus.publish("pub", Message::text("test:multi", "pub", "broadcast"))
        .await
        .unwrap();

    let m1 = rx1.recv().await.unwrap();
    let m2 = rx2.recv().await.unwrap();
    assert_eq!(m1.payload, m2.payload);
    assert_eq!(
        m1.payload,
        serde_json::Value::String("broadcast".to_string())
    );
}

#[tokio::test]
async fn bus_list_actors() {
    let bus = Bus::new();
    bus.register_announcement(ActorAnnouncement {
        id: "actor-a".to_string(),
        subscriptions: vec!["topic:1".to_string()],
        publications: vec!["topic:2".to_string()],
    })
    .await;
    bus.register_announcement(ActorAnnouncement {
        id: "actor-b".to_string(),
        subscriptions: vec!["topic:2".to_string()],
        publications: vec!["topic:1".to_string()],
    })
    .await;

    let actors = bus.list_announced_actors().await;
    assert_eq!(actors.len(), 2);
    let names: Vec<_> = actors.iter().map(|a| a.id.clone()).collect();
    assert!(names.contains(&"actor-a".to_string()));
    assert!(names.contains(&"actor-b".to_string()));
}

#[tokio::test]
async fn bus_spawn_actor() {
    let bus = Bus::new();
    bus.create_topic("actor:out", 100).await;

    let echo = EchoActor::new();
    let handle = bus.spawn_actor(echo).await.unwrap();
    let actors = bus.list_announced_actors().await;
    assert!(actors.iter().any(|a| a.id == "echo-actor"));

    handle.stop().await;
}

// =========================================================================
// Message tests
// =========================================================================

#[test]
fn message_new() {
    let msg = Message::new("topic:1", "src", "payload");
    assert_eq!(msg.topic, "topic:1");
    assert_eq!(msg.source, "src");
    assert_eq!(msg.payload, serde_json::Value::String("payload".to_string()));
    assert!(!msg.reference.is_empty());
    assert_eq!(msg.meta, serde_json::Value::Null);
}

#[test]
fn message_text() {
    let msg = Message::text("topic:1", "src", "hello");
    assert_eq!(msg.topic, "topic:1");
    assert_eq!(msg.source, "src");
    assert_eq!(
        msg.payload,
        serde_json::Value::String("hello".to_string())
    );
}

#[test]
fn message_with_type() {
    let msg = Message::text("t", "s", "body").with_type("LlmStream");
    assert_eq!(msg.meta, serde_json::json!({"type": "LlmStream"}));
}

#[test]
fn message_with_stream_id() {
    let msg = Message::text("t", "s", "body")
        .with_type("LlmStream")
        .with_stream_id("abc-123".to_string());
    assert_eq!(msg.meta["type"], "LlmStream");
    assert_eq!(msg.meta["stream_id"], "abc-123");
}

#[test]
fn message_serde_roundtrip() {
    let msg = Message::new("topic:1", "src", serde_json::json!({"key": 42}))
        .with_type("Test")
        .with_stream_id("sid-1".to_string());
    let json = serde_json::to_string(&msg).unwrap();
    let decoded: Message = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.topic, msg.topic);
    assert_eq!(decoded.source, msg.source);
    assert_eq!(decoded.payload, msg.payload);
    assert_eq!(decoded.meta, msg.meta);
}

// =========================================================================
// Config tests
// =========================================================================

#[test]
fn config_default() {
    let config = Config::default();
    assert!(!config.llm.model.is_empty());
    assert!(!config.llm.base_url.is_empty());
    assert!(config.llm.max_tokens > 0);
}

#[test]
fn config_from_toml_str() {
    let toml_str = r#"
[llm]
model = "test-model"
base_url = "http://localhost:8000/v1"
api_key = "test-key"
temperature = 0.5
max_tokens = 4096
request_timeout_secs = 60
stream_timeout_secs = 120
max_retries = 2
retry_base_delay_ms = 250

[server]
host = "0.0.0.0"
port = 8080
"#;
    let config = Config::from_str(toml_str).unwrap();
    assert_eq!(config.llm.model, "test-model");
    assert_eq!(config.llm.base_url, "http://localhost:8000/v1");
    assert_eq!(config.llm.api_key, "test-key");
    assert_eq!(config.llm.temperature, 0.5);
    assert_eq!(config.llm.max_tokens, 4096);
    assert_eq!(config.server.host, "0.0.0.0");
    assert_eq!(config.server.port, 8080);
}

#[test]
fn config_to_toml_roundtrip() {
    let config = Config::default();
    let toml_str = config.to_toml().unwrap();
    let parsed = Config::from_str(&toml_str).unwrap();
    assert_eq!(parsed.llm.model, config.llm.model);
    assert_eq!(parsed.server.port, config.server.port);
}

// =========================================================================
// Actor trait tests
// =========================================================================

#[tokio::test]
async fn actor_announce() {
    let echo = EchoActor::new();
    let ann = echo.announce();
    assert_eq!(ann.id, "echo-actor");
    assert!(ann.subscriptions.contains(&"echo:in".to_string()));
    assert!(ann.publications.contains(&"echo:out".to_string()));
}

#[tokio::test]
async fn actor_echo_roundtrip() {
    let bus = Bus::new();
    bus.create_topic("echo:in", 100).await;
    bus.create_topic("echo:out", 100).await;

    let echo = EchoActor::new();
    let handle = bus.spawn_actor(echo).await.unwrap();

    let bus2 = bus.clone();
    let collector = tokio::spawn(async move {
        let mut rx = bus2.subscribe("collector", "echo:out").await.unwrap();
        rx.recv().await.unwrap()
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    bus.register_announcement(ActorAnnouncement {
        id: "tester".to_string(),
        subscriptions: vec![],
        publications: vec!["echo:in".to_string()],
    })
    .await;
    bus.publish(
        "tester",
        Message::text("echo:in", "tester", "echo-me"),
    )
    .await
    .unwrap();

    let received = collector.await.unwrap();
    assert_eq!(
        received.payload,
        serde_json::Value::String("echo-me".to_string())
    );

    handle.stop().await;
}

// =========================================================================
// Helper actor for testing
// =========================================================================

pub struct EchoActor;

impl EchoActor {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Actor for EchoActor {
    fn id(&self) -> ActorId {
        "echo-actor".to_string()
    }

    fn announce(&self) -> ActorAnnouncement {
        ActorAnnouncement {
            id: self.id(),
            subscriptions: vec!["echo:in".to_string()],
            publications: vec!["echo:out".to_string()],
        }
    }

    async fn run(&self, bus: &Bus) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut rx = bus.subscribe(&self.id(), "echo:in").await?;
        let msg = rx.recv().await?;
        let text = match &msg.payload {
            serde_json::Value::String(s) => s.clone(),
            v => v.to_string(),
        };
        let out = Message::text("echo:out", &self.id(), text);
        bus.publish(&self.id(), out).await?;
        Ok(())
    }
}
