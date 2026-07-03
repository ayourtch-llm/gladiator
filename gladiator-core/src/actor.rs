use tokio::task::JoinHandle;

use crate::bus::Bus;

pub type ActorId = String;

#[derive(Debug, Clone)]
pub struct ActorAnnouncement {
    pub id: ActorId,
    pub subscriptions: Vec<String>,
    pub publications: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TopicAnnouncement {
    pub name: String,
    pub subscribers: Vec<ActorId>,
    pub publishers: Vec<ActorId>,
}

#[async_trait::async_trait]
pub trait Actor: Send + Sized {
    fn id(&self) -> ActorId;

    fn announce(&self) -> ActorAnnouncement {
        ActorAnnouncement {
            id: self.id(),
            subscriptions: Vec::new(),
            publications: Vec::new(),
        }
    }

    async fn run(&self, bus: &Bus) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

pub struct ActorJoinHandle {
    id: ActorId,
    handle: Option<JoinHandle<()>>,
}

impl ActorJoinHandle {
    pub fn new(id: ActorId, handle: JoinHandle<()>) -> Self {
        Self {
            id,
            handle: Some(handle),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub async fn stop(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }

    pub async fn wait(mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(handle) = self.handle.take() {
            handle
                .await
                .map_err(|e| Box::new(std::io::Error::new(std::io::ErrorKind::Other, format!("Task panicked: {}", e))) as Box<dyn std::error::Error + Send + Sync>)?;
        }
        Ok(())
    }
}

impl Drop for ActorJoinHandle {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}
