use crate::auth::Auth;
use crate::endpoint::Endpoint;
use crate::protocol::Protocol;
use crate::transport::HttpTransport;

pub struct Route {
    pub id: String,
    pub provider: String,
    pub protocol: Box<dyn Protocol>,
    pub endpoint: Endpoint,
    pub auth: Box<dyn Auth>,
    pub transport: HttpTransport,
}

impl Route {
    pub fn new(
        id: &str,
        provider: &str,
        protocol: Box<dyn Protocol>,
        endpoint: Endpoint,
        auth: Box<dyn Auth>,
        transport: HttpTransport,
    ) -> Self {
        Self {
            id: id.to_string(),
            provider: provider.to_string(),
            protocol,
            endpoint,
            auth,
            transport,
        }
    }

    pub fn build_body(&self, request: &crate::request::CanonicalRequest) -> serde_json::Value {
        self.protocol.build_body(request)
    }

    pub async fn send(
        &self,
        request: &crate::request::CanonicalRequest,
        config: &gladiator_core::LlmConfig,
    ) -> Result<reqwest::Response, crate::error::LlmError> {
        let body = self.build_body(request);
        self.transport
            .send_with_retry(&self.endpoint, &*self.auth, &body, config)
            .await
    }
}
