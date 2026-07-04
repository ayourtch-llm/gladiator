//! Probe `/v1/models` (or `/v1/models/{id}`) at startup to discover a model's
//! context window when it isn't supplied via config.
//!
//! Different OpenAI-compatible providers expose the context length in
//! different shapes:
//!
//! - **OpenRouter** `/v1/models`: top-level `context_length` per model.
//! - **LiteLLM / lmdeploy / vLLM**: often `context_length`, sometimes under
//!   `top_provider`, `model_info`, or `capabilities`.
//! - **OpenAI** `/v1/models`: returns no context length at all — we just
//!   return `None` and let the caller leave the metric unreported.
//!
//! Rather than hard-code provider quirks, we recursively scan the JSON for
//! any numeric field whose name contains both "context" and ("length"|"window"
//! |"size"). First match wins; ties break toward shallower depth so a
//! top-level value is preferred over a nested duplicate.

use crate::auth::{Auth, BearerAuth};
use crate::endpoint::Endpoint;
use std::time::Duration;

/// Best-effort probe. Network / parse errors return `None` (logged at warn);
/// callers treat `None` as "unknown, skip the metric".
pub async fn fetch_context_window(
    base_url: &str,
    api_key: &str,
    model: &str,
) -> Option<usize> {
    if model.is_empty() {
        return None;
    }

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()
        .ok()?;

    // Try `/v1/models/{id}` first — most providers that support per-model
    // metadata answer here, and it's a single round-trip.
    let endpoints = [
        Endpoint::new(base_url, &format!("/models/{}", model)),
        Endpoint::new(base_url, "/models"),
    ];

    for endpoint in &endpoints {
        let url = endpoint.url();
        tracing::debug!("[llm] probing model context window at {}", url);

        let mut req = client
            .get(&url)
            .header("Content-Type", "application/json")
            .timeout(Duration::from_secs(15));
        let auth = BearerAuth {
            token: api_key.to_string(),
        };
        req = auth.apply(req);

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(
                    "[llm] context-window probe {} failed: {}",
                    url,
                    e
                );
                continue;
            }
        };

        if !resp.status().is_success() {
            tracing::debug!(
                "[llm] context-window probe {} returned status {}",
                url,
                resp.status()
            );
            continue;
        }

        let json: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("[llm] context-window probe {} body parse failed: {}", url, e);
                continue;
            }
        };

        if let Some(window) = extract_context_window(&json, model) {
            tracing::info!(
                "[llm] discovered context_window={} for model '{}' via {}",
                window,
                model,
                url
            );
            return Some(window);
        }
    }

    tracing::warn!(
        "[llm] could not discover context window for '{}' from /v1/models; \
         context-remaining metric will be unavailable. Set \
         [llm].context_window in gladiator.toml to enable it.",
        model
    );
    None
}

/// Scan a `/v1/models` response for the model's context window.
///
/// Handles three response shapes:
/// 1. Single object (from `/models/{id}`): scan the object directly.
/// 2. OpenAI-style `{ "data": [...] }`: find the array element with matching
///    `id`, falling back to the first element if `model` matches nothing.
/// 3. Bare array: same logic as the `data` case.
fn extract_context_window(root: &serde_json::Value, model: &str) -> Option<usize> {
    // Shape 1: object directly describing the model. Trigger when there's no
    // `data`/array wrapping, AND the object either has an `id` field or
    // already carries a context-length-like key at the top level.
    if root.is_object()
        && root.get("data").is_none()
        && (root.get("id").is_some() || top_level_context_field(root).is_some())
    {
        return find_context_numeric(root);
    }

    // Shape 2/3: list/array of model objects.
    let candidates: Vec<&serde_json::Value> = if let Some(arr) = root.get("data").and_then(|d| d.as_array()) {
        arr.iter().collect()
    } else if let Some(arr) = root.as_array() {
        arr.iter().collect()
    } else {
        return None;
    };

    if candidates.is_empty() {
        return None;
    }

    // Prefer the entry whose id matches `model`.
    if let Some(matched) = candidates.iter().find(|v| {
        v.get("id").and_then(|i| i.as_str()).map(|s| s == model).unwrap_or(false)
    }) {
        if let Some(w) = find_context_numeric(matched) {
            return Some(w);
        }
    }
    // Fall back to the first entry — some gateways return a single-element list
    // without a meaningful id field.
    find_context_numeric(candidates[0])
}

/// Does this object expose a context-length-like key at the top level? Used to
/// decide whether to treat a single object as a model descriptor (shape 1)
/// rather than falling through to the array path.
fn top_level_context_field(obj: &serde_json::Value) -> Option<String> {
    let map = obj.as_object()?;
    for k in map.keys() {
        if looks_like_context_field(k) {
            return Some(k.clone());
        }
    }
    None
}

/// Recursive search for a numeric field whose name suggests context length.
/// Prefers shallower depths so a top-level `context_length` wins over a nested
/// duplicate (which some providers emit under multiple paths).
fn find_context_numeric(v: &serde_json::Value) -> Option<usize> {
    fn walk(v: &serde_json::Value, depth: usize) -> Option<(usize, usize)> {
        match v {
            serde_json::Value::Object(map) => {
                let mut best: Option<(usize, usize)> = None;
                for (k, val) in map {
                    if looks_like_context_field(k) {
                        if let Some(n) = val.as_u64().filter(|n| *n > 0) {
                            // Reject absurd values (>2M tokens) — those are
                            // almost certainly not a context window.
                            if n <= 2_000_000 {
                                let candidate = (n as usize, depth);
                                best = match best {
                                    Some(prev) if prev.1 <= candidate.1 => Some(prev),
                                    _ => Some(candidate),
                                };
                                continue;
                            }
                        }
                    }
                    if let Some(found) = walk(val, depth + 1) {
                        best = match best {
                            Some(prev) if prev.1 <= found.1 => Some(prev),
                            _ => Some(found),
                        };
                    }
                }
                best
            }
            serde_json::Value::Array(arr) => {
                let mut best: Option<(usize, usize)> = None;
                for item in arr {
                    if let Some(found) = walk(item, depth + 1) {
                        best = match best {
                            Some(prev) if prev.1 <= found.1 => Some(prev),
                            _ => Some(found),
                        };
                    }
                }
                best
            }
            _ => None,
        }
    }
    walk(v, 0).map(|(n, _)| n)
}

/// Does this key name look like a context-window declaration? Matches
/// "context" + ("length" | "window" | "size"), case-insensitive.
fn looks_like_context_field(key: &str) -> bool {
    let k = key.to_lowercase();
    k.contains("context")
        && (k.contains("length") || k.contains("window") || k.contains("size"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openrouter_single_object_shape() {
        let body = serde_json::json!({
            "id": "anthropic/claude-3.5-sonnet",
            "context_length": 200000,
            "pricing": {"prompt": "0.000003"}
        });
        assert_eq!(extract_context_window(&body, "anthropic/claude-3.5-sonnet"), Some(200000));
    }

    #[test]
    fn litellm_data_array_with_matching_id() {
        let body = serde_json::json!({
            "data": [
                {"id": "wrong-model", "context_length": 4096},
                {"id": "custom/glm-5.2", "context_length": 131072}
            ]
        });
        assert_eq!(extract_context_window(&body, "custom/glm-5.2"), Some(131072));
    }

    #[test]
    fn openai_data_array_no_context_field_returns_none() {
        // Real OpenAI /v1/models returns objects with no context info at all.
        let body = serde_json::json!({
            "data": [
                {"id": "gpt-4o-mini", "object": "model", "created": 1234, "owned_by": "openai"}
            ]
        });
        assert_eq!(extract_context_window(&body, "gpt-4o-mini"), None);
    }

    #[test]
    fn bare_array_falls_back_to_first() {
        let body = serde_json::json!([
            {"context_length": 8192},
            {"context_length": 16384}
        ]);
        assert_eq!(extract_context_window(&body, "no-such-id"), Some(8192));
    }

    #[test]
    fn prefers_shallower_match() {
        // Both top-level and nested have a context-length-like key; top wins.
        let body = serde_json::json!({
            "context_length": 128000,
            "metadata": {"context_length": 8192}
        });
        assert_eq!(extract_context_window(&body, "x"), Some(128000));
    }

    #[test]
    fn nested_under_top_provider_openrouter_variant() {
        let body = serde_json::json!({
            "id": "x",
            "top_provider": {"context_length": 65536}
        });
        assert_eq!(extract_context_window(&body, "x"), Some(65536));
    }

    #[test]
    fn rejects_absurd_values() {
        // >2M tokens is almost certainly a misread (e.g. a price in micro-cents
        // matched by accident). Walk should skip it.
        let body = serde_json::json!({
            "context_length": 100_000_000
        });
        assert_eq!(extract_context_window(&body, "x"), None);
    }

    #[test]
    fn empty_data_returns_none() {
        let body = serde_json::json!({"data": []});
        assert_eq!(extract_context_window(&body, "x"), None);
    }

    #[test]
    fn field_name_matcher_variants() {
        assert!(looks_like_context_field("context_length"));
        assert!(looks_like_context_field("context_window"));
        assert!(looks_like_context_field("contextSize"));
        assert!(looks_like_context_field("max-context-length"));
        assert!(!looks_like_context_field("context"));
        assert!(!looks_like_context_field("length"));
        assert!(!looks_like_context_field("max_tokens"));
    }
}
