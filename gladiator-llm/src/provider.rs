use crate::auth::BearerAuth;
use crate::endpoint::Endpoint;
use crate::openai_chat::OpenAIChatProtocol;
use crate::route::Route;
use crate::transport::HttpTransport;

pub struct ProviderConfig {
    pub id: String,
    pub base_url: String,
    pub api_key: String,
}

impl ProviderConfig {
    pub fn new(id: &str, base_url: &str, api_key: &str) -> Self {
        Self {
            id: id.to_string(),
            base_url: base_url.to_string(),
            api_key: api_key.to_string(),
        }
    }

    pub fn openai_chat_route(&self) -> Route {
        let transport = HttpTransport::new().unwrap_or_else(|e| {
            tracing::error!("Failed to create HTTP transport: {}", e);
            HttpTransport::new().unwrap()
        });

        Route::new(
            "openai-chat",
            &self.id,
            Box::new(OpenAIChatProtocol),
            Endpoint::new(&self.base_url, "/chat/completions"),
            Box::new(BearerAuth {
                token: self.api_key.clone(),
            }),
            transport,
        )
    }
}
