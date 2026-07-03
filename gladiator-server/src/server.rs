use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Html,
    routing::{get, post},
    Json,
    Router,
};
use futures::stream::Stream;
use gladiator_core::actor::ActorAnnouncement;
use gladiator_core::bus::Bus;
use gladiator_core::message::Message;
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;
use tracing::{error, info};

const INDEX_HTML: &str = include_str!("index.html");

#[derive(Debug, Serialize)]
pub struct ActorInfoResponse {
    pub id: String,
    pub subscriptions: Vec<String>,
    pub publications: Vec<String>,
    pub announced: bool,
}

#[derive(Debug, Serialize)]
pub struct TopicAnnouncementResponse {
    pub name: String,
    pub subscribers: Vec<String>,
    pub publishers: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct TopicInfoResponse {
    pub name: String,
    pub subscribers: Vec<String>,
    pub publishers: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct PublishRequest {
    topic: String,
    source: String,
    payload: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct PublishResponse {
    success: bool,
    topic: String,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    error: String,
}

pub async fn health() -> &'static str {
    "ok"
}

pub async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

pub async fn list_topics(State(bus): State<Bus>) -> Json<Vec<TopicInfoResponse>> {
    let announced = bus.list_announced_topics().await;
    let runtime = bus.list_topic_info().await;
    let runtime_map: std::collections::HashMap<String, _> = runtime
        .into_iter()
        .map(|t| (t.name.clone(), t))
        .collect();

    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();

    for t in &announced {
        seen.insert(t.name.clone());
        let mut subs = t.subscribers.clone();
        let mut pubs = t.publishers.clone();
        if let Some(rt) = runtime_map.get(&t.name) {
            for s in &rt.subscribers {
                if !subs.contains(s) {
                    subs.push(s.clone());
                }
            }
            for p in &rt.publishers {
                if !pubs.contains(p) {
                    pubs.push(p.clone());
                }
            }
        }
        result.push(TopicInfoResponse {
            name: t.name.clone(),
            subscribers: subs,
            publishers: pubs,
        });
    }

    for (name, t) in runtime_map {
        if !seen.contains(&name) {
            result.push(TopicInfoResponse {
                name,
                subscribers: t.subscribers,
                publishers: t.publishers,
            });
        }
    }

    result.sort_by(|a, b| a.name.cmp(&b.name));
    Json(result)
}

pub async fn list_actors(State(bus): State<Bus>) -> Json<Vec<ActorInfoResponse>> {
    let announced = bus.list_announced_actors().await;
    let announced_map: std::collections::HashMap<String, _> = announced
        .into_iter()
        .map(|a| (a.id.clone(), a))
        .collect();

    let actors = bus.list_actors().await;
    Json(
        actors
            .into_iter()
            .map(|a| {
                let id = a.id.clone();
                let ann = announced_map.get(&id);
                let (subscriptions, publications) = if let Some(ann) = ann {
                    (ann.subscriptions.clone(), ann.publications.clone())
                } else {
                    (a.subscriptions.clone(), a.publications.clone())
                };
                ActorInfoResponse {
                    id,
                    subscriptions,
                    publications,
                    announced: ann.is_some(),
                }
            })
            .collect(),
    )
}

pub async fn list_announced(State(bus): State<Bus>) -> Json<Vec<ActorInfoResponse>> {
    let announced = bus.list_announced_actors().await;
    Json(
        announced
            .into_iter()
            .map(|a| ActorInfoResponse {
                id: a.id,
                subscriptions: a.subscriptions,
                publications: a.publications,
                announced: true,
            })
            .collect(),
    )
}

pub async fn list_announced_topics(State(bus): State<Bus>) -> Json<Vec<TopicAnnouncementResponse>> {
    let topics = bus.list_announced_topics().await;
    Json(
        topics
            .into_iter()
            .map(|t| TopicAnnouncementResponse {
                name: t.name,
                subscribers: t.subscribers,
                publishers: t.publishers,
            })
            .collect(),
    )
}

pub async fn publish(
    State(bus): State<Bus>,
    Json(req): Json<PublishRequest>,
) -> Result<Json<PublishResponse>, (StatusCode, Json<ErrorResponse>)> {
    let msg = Message::new(&req.topic, &req.source, req.payload);
    match bus
        .publish("gladiator-server", msg)
        .await
    {
        Ok(()) => {
            info!("Published to topic '{}'", req.topic);
            Ok(Json(PublishResponse {
                success: true,
                topic: req.topic,
            }))
        }
        Err(e) => {
            error!("Failed to publish: {}", e);
            Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            ))
        }
    }
}

pub async fn stream_topic(
    State(bus): State<Bus>,
    Path(topic): Path<String>,
) -> Result<
    axum::response::sse::Sse<
        impl Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
    >,
    (StatusCode, Json<ErrorResponse>),
> {
    let rx = bus
        .subscribe_stream(&topic)
        .await
        .map_err(|e| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;

    let stream = futures::stream::unfold(rx, |mut rx| async move {
        match rx.recv().await {
            Ok(msg) => {
                let payload = serde_json::to_string(&msg).unwrap_or_default();
                let event = axum::response::sse::Event::default().data(payload);
                Some((Ok::<_, std::convert::Infallible>(event), rx))
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                let event = axum::response::sse::Event::default().comment("skipped lagged");
                Some((Ok::<_, std::convert::Infallible>(event), rx))
            }
        }
    });

    Ok(axum::response::sse::Sse::new(stream))
}

pub async fn run_server(
    bus: Bus,
    host: String,
    port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    bus.register_announcement(ActorAnnouncement {
        id: "gladiator-server".to_string(),
        subscriptions: vec![],
        publications: vec!["server".to_string()],
    })
    .await;

    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/api/topics", get(list_topics))
        .route("/api/actors", get(list_actors))
        .route("/api/announced", get(list_announced))
        .route("/api/announced-topics", get(list_announced_topics))
        .route("/api/publish", post(publish))
        .route("/api/stream/{topic}", get(stream_topic))
        .with_state(bus)
        .layer(CorsLayer::permissive());

    let addr = format!("{}:{}", host, port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    info!("Server listening on {}", addr);
    axum::serve(listener, app).await?;

    Ok(())
}
