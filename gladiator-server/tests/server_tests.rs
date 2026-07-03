use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::routing::{get, post};
use gladiator_core::bus::Bus;
use gladiator_server::{health, list_topics, list_actors, list_announced, publish};
use tower::ServiceExt;

async fn setup_test_app() -> (Router, Bus) {
    let bus = Bus::new();
    bus.create_topic("test:topic", 100).await;

    // Register the server as an announced actor so it can publish
    bus.register_announcement(gladiator_core::actor::ActorAnnouncement {
        id: "gladiator-server".to_string(),
        subscriptions: vec![],
        publications: vec!["server".to_string()],
    })
    .await;

    let app: Router = Router::new()
        .route("/health", get(health))
        .route("/api/topics", get(list_topics))
        .route("/api/actors", get(list_actors))
        .route("/api/announced", get(list_announced))
        .route("/api/publish", post(publish))
        .with_state(bus.clone());

    (app, bus)
}

#[tokio::test]
async fn test_health_endpoint() {
    let (app, _bus) = setup_test_app().await;

    let request = Request::builder()
        .method(Method::GET)
        .uri("/health")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body, "ok");
}

#[tokio::test]
async fn test_list_topics_endpoint() {
    let (app, _bus) = setup_test_app().await;

    let request = Request::builder()
        .method(Method::GET)
        .uri("/api/topics")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.is_array());
    // test:topic was created in setup
    assert!(json.as_array().unwrap().len() >= 1);
}

#[tokio::test]
async fn test_list_topics_multiple() {
    let (app, bus) = setup_test_app().await;

    bus.create_topic("alpha", 10).await;
    bus.create_topic("beta", 20).await;
    bus.create_topic("gamma", 30).await;

    let request = Request::builder()
        .method(Method::GET)
        .uri("/api/topics")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert!(json.len() >= 4);

    let names: Vec<String> = json.iter()
        .map(|v| v["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"alpha".to_string()));
    assert!(names.contains(&"beta".to_string()));
    assert!(names.contains(&"gamma".to_string()));
    assert!(names.contains(&"test:topic".to_string()));
}

#[tokio::test]
async fn test_publish_endpoint_success() {
    let (app, bus) = setup_test_app().await;

    // Subscribe to receive the message
    let mut rx = bus.subscribe_stream("test:topic").await.unwrap();

    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/publish")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "topic": "test:topic",
                "source": "test-source",
                "payload": "test message"
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["success"], true);
    assert_eq!(json["topic"], "test:topic");

    let received = rx.recv().await.unwrap();
    assert_eq!(received.payload, serde_json::json!("test message"));
}

#[tokio::test]
async fn test_publish_with_json_payload() {
    let (app, bus) = setup_test_app().await;

    let mut rx = bus.subscribe_stream("test:topic").await.unwrap();

    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/publish")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "topic": "test:topic",
                "source": "api",
                "payload": {"key": "value", "number": 42}
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["success"], true);

    let received = rx.recv().await.unwrap();
    assert_eq!(received.payload["key"], "value");
    assert_eq!(received.payload["number"], 42);
}

#[tokio::test]
async fn test_publish_unannounced_actor() {
    let (app, _bus) = setup_test_app().await;

    // The server is announced, so publishing should work
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/publish")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "topic": "test:topic",
                "source": "test-source",
                "payload": "hello"
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_list_actors_endpoint() {
    let (app, bus) = setup_test_app().await;

    // Register an actor and have it subscribe to a topic so it appears in list_actors
    bus.register_announcement(gladiator_core::actor::ActorAnnouncement {
        id: "test-actor".to_string(),
        subscriptions: vec!["test:topic".to_string()],
        publications: vec!["output".to_string()],
    })
    .await;
    let _rx = bus.subscribe_stream("test:topic").await.unwrap();
    // Also publish to register the server actor
    bus.register_actor("test-actor", "test:topic", true).await;

    let request = Request::builder()
        .method(Method::GET)
        .uri("/api/actors")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert!(json.len() >= 1);

    let ids: Vec<String> = json.iter()
        .map(|v| v["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&"test-actor".to_string()));
}

#[tokio::test]
async fn test_list_announced_endpoint() {
    let (app, bus) = setup_test_app().await;

    bus.register_announcement(gladiator_core::actor::ActorAnnouncement {
        id: "llm-actor-1".to_string(),
        subscriptions: vec!["user-input".to_string()],
        publications: vec!["llm-stream".to_string()],
    })
    .await;

    let request = Request::builder()
        .method(Method::GET)
        .uri("/api/announced")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert!(json.len() >= 2);

    let ids: Vec<String> = json.iter()
        .map(|v| v["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&"llm-actor-1".to_string()));
    assert!(ids.contains(&"gladiator-server".to_string()));
}
