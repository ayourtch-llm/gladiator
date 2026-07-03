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

    async fn publish_result(
        &self,
        bus: &Bus,
        tool_call_id: &str,
        success: bool,
        result: String,
        error: Option<String>,
    ) {
        let result_msg = ToolResultMessage {
            tool_call_id: tool_call_id.to_string(),
            tool_name: self.tool.name().to_string(),
            success,
            result,
            error,
        };

        let msg = Message::new(
            "tool:results",
            &format!("tool-{}", self.tool.name()),
            serde_json::to_value(&result_msg).unwrap(),
        );

        if let Err(e) = bus.publish(&format!("tool-{}", self.tool.name()), msg).await {
            error!("Failed to publish tool result for '{}': {}", self.tool.name(), e);
        }
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

                    let result = tool.execute(&args).await;

                    let (success, result_text, error_text) = match result {
                        Ok(r) => (true, r, None),
                        Err(e) => (false, String::new(), Some(e)),
                    };

                    self.publish_result(bus, &call_id, success, result_text, error_text)
                        .await;
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
