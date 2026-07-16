use crate::auth::Auth;
use crate::endpoint::Endpoint;
use crate::error::{is_retryable_reqwest_error, LlmError};

pub struct PreparedRequest {
    pub url: String,
    pub body: serde_json::Value,
    pub headers: Vec<(String, String)>,
}

pub struct HttpTransport {
    client: reqwest::Client,
}

impl HttpTransport {
    pub fn new() -> Result<Self, LlmError> {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| LlmError::Other(format!("Failed to create HTTP client: {}", e)))?;
        Ok(Self { client })
    }

    pub fn from_client(client: reqwest::Client) -> Self {
        Self { client }
    }

    pub async fn send_with_retry(
        &self,
        endpoint: &Endpoint,
        auth: &dyn Auth,
        body: &serde_json::Value,
        config: &gladiator_core::LlmConfig,
    ) -> Result<reqwest::Response, LlmError> {
        let url = endpoint.url();
        let mut attempt = 0u32;
        let mut delay = std::time::Duration::from_millis(config.retry_base_delay_ms);

        loop {
            let request = self
                .client
                .post(&url)
                .header("Content-Type", "application/json")
                .json(body);

            let request = auth.apply(request);

            match tokio::time::timeout(
                std::time::Duration::from_secs(config.request_timeout_secs),
                request.send(),
            )
            .await
            {
                Ok(Ok(response)) => {
                    if !response.status().is_success() {
                        let status = response.status();
                        let body = response.text().await.unwrap_or_default();
                        let err = LlmError::Api {
                            status: status.as_u16(),
                            body,
                        };
                        if err.is_retryable() && attempt < config.max_retries {
                            tracing::warn!(
                                "Request failed (attempt {}/{}, status {}), retrying in {:?}",
                                attempt + 1,
                                config.max_retries,
                                status.as_u16(),
                                delay
                            );
                            tokio::time::sleep(delay).await;
                            delay = delay.saturating_mul(2);
                            attempt += 1;
                            continue;
                        }
                        return Err(err);
                    }
                    return Ok(response);
                }
                Ok(Err(e)) => {
                    // Include the full source chain: reqwest's Display hides the
                    // underlying cause (DNS vs connect vs body), Debug shows it.
                    let err_str = format!("{} ({:?})", e, e);
                    if is_retryable_reqwest_error(&e) && attempt < config.max_retries {
                        tracing::warn!(
                            "Request failed (attempt {}/{}), retrying in {:?}: {}",
                            attempt + 1,
                            config.max_retries,
                            delay,
                            err_str
                        );
                        tokio::time::sleep(delay).await;
                        delay = delay.saturating_mul(2);
                        attempt += 1;
                        continue;
                    }
                    return Err(LlmError::Network(err_str));
                }
                Err(_) => {
                    if attempt < config.max_retries {
                        tracing::warn!(
                            "Request timed out after {}s (attempt {}/{}), retrying in {:?}",
                            config.request_timeout_secs,
                            attempt + 1,
                            config.max_retries,
                            delay
                        );
                        tokio::time::sleep(delay).await;
                        delay = delay.saturating_mul(2);
                        attempt += 1;
                        continue;
                    }
                    return Err(LlmError::Timeout(config.request_timeout_secs));
                }
            }
        }
    }
}
