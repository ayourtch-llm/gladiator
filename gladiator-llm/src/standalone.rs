//! Standalone one-shot LLM call helper.
//!
//! Accepts a provider config (base_url, api_key, model) plus an instruction
//! string and returns the full response text. This is a synchronous-style API
//! that wraps the streaming request machinery into a single await point,
//! suitable for ad-hoc calls from tools or other actors that need a quick LLM
//! completion without setting up a full bus-based conversation.

use crate::config::merge_config;
use crate::provider::ProviderConfig;
use crate::request::CanonicalRequest;
use gladiator_core::LlmConfig;

/// Call an LLM with the given instruction string and return its response.
///
/// `instruction` is sent as a single user message. The provider config
/// (base_url, api_key, model) determines which endpoint to hit. Returns
/// the full concatenated text response on success, or an error string on failure.
pub async fn llm_call(
    base_config: &LlmConfig,
    instruction: &str,
) -> Result<String, String> {
    let config = merge_config(base_config, None);

    if config.model.is_empty() {
        return Err("LLM model name is empty".to_string());
    }

    let canonical = CanonicalRequest {
        model: config.model.clone(),
        messages: vec![serde_json::json!({"role": "user", "content": instruction})],
        tools: None,
        tool_choice: None,
        temperature: Some(config.temperature),
        max_tokens: Some(config.max_tokens),
        stream: true,
        grammar: None,
    };

    let provider = ProviderConfig::new("openai", &config.base_url, &config.api_key);
    let route = provider.openai_chat_route();

    match route.send(&canonical, &config).await {
        Ok(response) => {
            // Stream the response to completion, collecting text deltas.
            use futures::StreamExt;
            let mut full_response = String::new();
            let mut stream = response.bytes_stream();
            let mut state = crate::protocol::StreamState::default();
            let stream_timeout =
                std::time::Duration::from_secs(config.stream_timeout_secs);

            loop {
                match tokio::time::timeout(stream_timeout, stream.next()).await {
                    Ok(Some(Ok(chunk))) => {
                        let payloads = crate::framing::decode_sse_chunk(&chunk);
                        for payload in payloads {
                            if let Ok(json) =
                                serde_json::from_str::<serde_json::Value>(&payload)
                            {
                                let events =
                                    route.protocol.parse_event(&json, &mut state);
                                for event in events {
                                    match event {
                                        crate::event::LlmEvent::TextDelta { text, .. } => {
                                            full_response.push_str(&text);
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                    Ok(Some(Err(e))) => {
                        return Err(format!("Stream error: {}", e));
                    }
                    Ok(None) => break,
                    Err(_) => {
                        return Err("Stream timeout".to_string());
                    }
                }
            }

            // If no text was streamed (e.g. tool-call-only response), fall back
            // to any accumulated content in the stream state.
            if full_response.is_empty() && !state.text.is_empty() {
                full_response = std::mem::take(&mut state.text);
            }

            Ok(full_response)
        }
        Err(e) => Err(format!("LLM request failed: {}", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_call_exists() {
        // Compile-time check that the function exists with correct types.
        let _f = llm_call;
    }
}
