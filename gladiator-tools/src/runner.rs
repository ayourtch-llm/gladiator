use crate::tool::{Tool, ToolExecuteMessage, ToolResultMessage};
use gladiator_core::{Bus, Message};
use tokio::sync::broadcast::error::RecvError;
use tracing::{error, info, warn};

/// Runner that subscribes to a tool's execute topic on the bus and dispatches
/// tool calls. Results are published to the shared `tool:results` topic.
pub struct ToolActorRunner {
    tool: std::sync::Arc<dyn Tool>,
}

impl ToolActorRunner {
    pub fn new(tool: impl Tool + 'static) -> Self {
        Self {
            tool: std::sync::Arc::new(tool),
        }
    }

    pub fn from_arc(tool: std::sync::Arc<dyn Tool>) -> Self {
        Self { tool }
    }

    pub async fn run(&self, bus: &Bus) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let tool_name = self.tool.name().to_string();
        let execute_topic = format!("tool:{}:execute", tool_name);
        let results_topic = "tool:results";

        let actor_id = format!("tool-{}", tool_name);
        bus.register_announcement(gladiator_core::ActorAnnouncement {
            id: actor_id.clone(),
            subscriptions: vec![execute_topic.clone()],
            publications: vec![results_topic.to_string()],
        })
        .await;

        let mut execute_rx = bus.subscribe(&actor_id, &execute_topic).await?;

        info!("Tool actor '{}' listening on '{}'", tool_name, execute_topic);

        loop {
            match execute_rx.recv().await {
                Ok(msg) => {
                    let exec_msg: ToolExecuteMessage = match serde_json::from_value(msg.payload) {
                        Ok(m) => m,
                        Err(e) => {
                            error!("Failed to parse tool execute message for '{}': {}", tool_name, e);
                            continue;
                        }
                    };

                    info!(
                        "Tool '{}' executing call_id='{}'",
                        tool_name, exec_msg.tool_call_id
                    );

                    let tool = self.tool.clone();
                    let call_id = exec_msg.tool_call_id.clone();
                    let args = exec_msg.arguments.clone();
                    let bus_clone = bus.clone();
                    let tool_name_clone = tool_name.clone();

                    tokio::spawn(async move {
                        let result = tool.execute(&args).await;
                        let (success, result_text, error_text) = match result {
                            Ok(r) => (true, r, None),
                            Err(e) => (false, String::new(), Some(e)),
                        };

                        let result_msg = ToolResultMessage {
                            tool_call_id: call_id,
                            tool_name: tool_name_clone,
                            success,
                            result: result_text,
                            error: error_text,
                        };

                        let msg = Message::new(
                            "tool:results",
                            &format!("tool-{}", result_msg.tool_name),
                            serde_json::to_value(&result_msg).unwrap_or_else(|e| {
                                error!("Failed to serialize tool result: {}", e);
                                serde_json::json!({"error": format!("serialization failed: {}", e)})
                            }),
                        );

                        let publisher_id = format!("tool-{}", result_msg.tool_name);
                        if let Err(e) = bus_clone.publish(&publisher_id, msg).await {
                            error!("Failed to publish tool result: {}", e);
                        }
                    });
                }
                Err(RecvError::Lagged(n)) => {
                    warn!("Tool '{}' lagged behind, dropped {} messages", tool_name, n);
                }
                Err(RecvError::Closed) => {
                    info!("Tool '{}' execute topic closed", tool_name);
                    break;
                }
            }
        }

        Ok(())
    }
}
