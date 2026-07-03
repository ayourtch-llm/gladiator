use gladiator_core::{Actor, ActorAnnouncement, Bus, Message};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, error, info};

/// PersistenceActor mediates save/load of agent state.
///
/// It subscribes to:
/// - `command_topic` — receives save/load requests from the TUI
/// - `state_topic` — receives state dumps from agents
///
/// It publishes to:
/// - `response_topic` — sends success/failure confirmations to the TUI
/// - `state_control_topic` — sends dump_state / load_state commands to agents
///
/// Multi-agent support: each command includes an `agent_id` field so the
/// persistence actor can route state dumps to the correct pending save.
#[derive(Debug)]
pub struct PersistenceActor {
    pub index: usize,
    pub command_topic: String,
    pub response_topic: String,
    pub state_control_topic: String,
    pub state_topic: String,
}

impl PersistenceActor {
    pub fn new(
        index: usize,
        command_topic: String,
        response_topic: String,
        state_control_topic: String,
        state_topic: String,
    ) -> Self {
        Self {
            index,
            command_topic,
            response_topic,
            state_control_topic,
            state_topic,
        }
    }

    async fn publish_response(&self, bus: &Bus, success: bool, message: &str) {
        let msg = Message::new(
            &self.response_topic,
            &self.id(),
            serde_json::json!({"success": success, "message": message}),
        )
        .with_type("PersistenceResponse");
        if let Err(e) = bus.publish(&self.id(), msg).await {
            error!("PersistenceActor failed to publish response: {}", e);
        }
    }
}

#[async_trait::async_trait]
impl Actor for PersistenceActor {
    fn id(&self) -> gladiator_core::ActorId {
        format!("gladiator-persistence-{}", self.index)
    }

    fn announce(&self) -> ActorAnnouncement {
        ActorAnnouncement {
            id: self.id(),
            subscriptions: vec![
                self.command_topic.clone(),
                self.state_topic.clone(),
            ],
            publications: vec![
                self.response_topic.clone(),
                self.state_control_topic.clone(),
            ],
        }
    }

    async fn run(&self, bus: &Bus) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut command_rx = bus.subscribe(&self.id(), &self.command_topic).await?;
        let mut state_rx = bus.subscribe(&self.id(), &self.state_topic).await?;

        // Pending saves: agent_id -> filename
        let pending_saves: Arc<tokio::sync::Mutex<HashMap<String, String>>> =
            Arc::new(tokio::sync::Mutex::new(HashMap::new()));

        info!(
            "PersistenceActor {} listening on command='{}' state='{}'",
            self.index, self.command_topic, self.state_topic
        );

        loop {
            tokio::select! {
                result = command_rx.recv() => {
                    match result {
                        Ok(msg) => {
                            let action = msg.payload.get("action").and_then(|v| v.as_str()).unwrap_or("");
                            let filename = msg.payload.get("filename").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let agent_id = msg.payload.get("agent_id").and_then(|v| v.as_str()).unwrap_or("").to_string();

                            match action {
                                "save" => {
                                    if filename.is_empty() || agent_id.is_empty() {
                                        self.publish_response(bus, false, "Missing filename or agent_id").await;
                                        continue;
                                    }
                                    debug!("PersistenceActor: save {} for {}", filename, agent_id);
                                    pending_saves.lock().await.insert(agent_id.clone(), filename.clone());
                                    let dump_msg = Message::new(
                                        &self.state_control_topic,
                                        &self.id(),
                                        serde_json::json!({"type": "dump_state", "agent_id": agent_id}),
                                    );
                                    if let Err(e) = bus.publish(&self.id(), dump_msg).await {
                                        error!("PersistenceActor: failed to publish dump_state: {}", e);
                                        pending_saves.lock().await.remove(&agent_id);
                                        self.publish_response(bus, false, &format!("Failed to request state: {}", e)).await;
                                    }
                                }
                                "load" => {
                                    if filename.is_empty() || agent_id.is_empty() {
                                        self.publish_response(bus, false, "Missing filename or agent_id").await;
                                        continue;
                                    }
                                    debug!("PersistenceActor: load {} for {}", filename, agent_id);
                                    match std::fs::read_to_string(&filename) {
                                        Ok(content) => {
                                            match serde_json::from_str::<serde_json::Value>(&content) {
                                                Ok(json) => {
                                                    let state = json.get("state").unwrap_or(&json).clone();
                                                    let load_msg = Message::new(
                                                        &self.state_control_topic,
                                                        &self.id(),
                                                        serde_json::json!({"type": "load_state", "agent_id": agent_id, "state": state}),
                                                    );
                                                    if let Err(e) = bus.publish(&self.id(), load_msg).await {
                                                        self.publish_response(bus, false, &format!("Failed to publish load_state: {}", e)).await;
                                                    } else {
                                                        self.publish_response(bus, true, &format!("Loaded state from {}", filename)).await;
                                                    }
                                                }
                                                Err(e) => {
                                                    self.publish_response(bus, false, &format!("Failed to parse state file: {}", e)).await;
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            self.publish_response(bus, false, &format!("Failed to read file: {}", e)).await;
                                        }
                                    }
                                }
                                _ => {
                                    debug!("PersistenceActor: unknown action '{}'", action);
                                }
                            }
                        }
                        Err(RecvError::Lagged(_)) => {}
                        Err(RecvError::Closed) => break,
                    }
                }
                result = state_rx.recv() => {
                    match result {
                        Ok(msg) => {
                            let agent_id = msg.payload.get("agent_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let state = msg.payload.get("state").cloned().unwrap_or(serde_json::Value::Null);

                            let mut pending = pending_saves.lock().await;
                            if let Some(filename) = pending.remove(&agent_id) {
                                drop(pending);
                                let file_content = serde_json::json!({
                                    "version": 1,
                                    "agent_id": agent_id,
                                    "state": state,
                                });
                                match serde_json::to_string_pretty(&file_content) {
                                    Ok(json_str) => {
                                        match std::fs::write(&filename, json_str) {
                                            Ok(()) => {
                                                self.publish_response(bus, true, &format!("Saved state to {}", filename)).await;
                                            }
                                            Err(e) => {
                                                self.publish_response(bus, false, &format!("Failed to write file: {}", e)).await;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        self.publish_response(bus, false, &format!("Failed to serialize state: {}", e)).await;
                                    }
                                }
                            } else {
                                debug!("PersistenceActor: received state for unknown agent '{}'", agent_id);
                            }
                        }
                        Err(RecvError::Lagged(_)) => {}
                        Err(RecvError::Closed) => break,
                    }
                }
            }
        }

        Ok(())
    }
}
