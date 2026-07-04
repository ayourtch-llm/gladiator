#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("Network error: {0}")]
    Network(String),
    #[error("Request timeout after {0} seconds")]
    Timeout(u64),
    #[error("API error {status}: {body}")]
    Api { status: u16, body: String },
    #[error("Stream error: {0}")]
    Stream(String),
    #[error("Stream interrupted: partial response ({0} chars), last error: {1}")]
    StreamInterrupted(usize, String),
    #[error("Other: {0}")]
    Other(String),
}

impl LlmError {
    pub fn is_retryable(&self) -> bool {
        match self {
            LlmError::Network(_) => true,
            LlmError::Timeout(_) => true,
            LlmError::Api { status, .. } => *status == 429 || *status >= 500,
            LlmError::Stream(_) => true,
            LlmError::StreamInterrupted(_, _) => false,
            LlmError::Other(_) => false,
        }
    }
}

pub fn is_retryable_reqwest_error(err: &reqwest::Error) -> bool {
    if err.is_timeout() || err.is_connect() {
        return true;
    }
    if let Some(status) = err.status() {
        return status.as_u16() == 429 || status.as_u16() >= 500;
    }
    false
}
