pub use self::merge_config as merge_config_public;

pub fn merge_config(
    base: &gladiator_core::LlmConfig,
    request_config: Option<&gladiator_core::LlmConfig>,
) -> gladiator_core::LlmConfig {
    match request_config {
        Some(req) => gladiator_core::LlmConfig {
            model: if req.model.is_empty() {
                base.model.clone()
            } else {
                req.model.clone()
            },
            base_url: if req.base_url.is_empty() {
                base.base_url.clone()
            } else {
                req.base_url.clone()
            },
            api_key: if req.api_key.is_empty() {
                base.api_key.clone()
            } else {
                req.api_key.clone()
            },
            temperature: req.temperature,
            max_tokens: if req.max_tokens == 0 {
                base.max_tokens
            } else {
                req.max_tokens
            },
            request_timeout_secs: if req.request_timeout_secs == 0 {
                base.request_timeout_secs
            } else {
                req.request_timeout_secs
            },
            stream_timeout_secs: if req.stream_timeout_secs == 0 {
                base.stream_timeout_secs
            } else {
                req.stream_timeout_secs
            },
            max_retries: if req.max_retries == 0 {
                base.max_retries
            } else {
                req.max_retries
            },
            retry_base_delay_ms: if req.retry_base_delay_ms == 0 {
                base.retry_base_delay_ms
            } else {
                req.retry_base_delay_ms
            },
            context_window: req.context_window.or(base.context_window),
        },
        None => base.clone(),
    }
}
