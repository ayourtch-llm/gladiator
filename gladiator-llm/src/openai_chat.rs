use crate::event::LlmEvent;
use crate::lifecycle::StreamLifecycle;
use crate::protocol::{Protocol, StreamState};
use crate::request::CanonicalRequest;

pub struct OpenAIChatProtocol;

impl Protocol for OpenAIChatProtocol {
    fn id(&self) -> &str {
        "openai-chat"
    }

    fn build_body(&self, request: &CanonicalRequest) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": request.model,
            "messages": request.messages,
            "temperature": request.temperature,
            "max_tokens": request.max_tokens,
            "stream": true,
            "stream_options": { "include_usage": true }
        });

        if let Some(tools) = &request.tools {
            body["tools"] = serde_json::to_value(tools).unwrap();
        }

        if let Some(grammar) = &request.grammar {
            body["grammar"] = serde_json::json!(grammar);
        }

        body
    }

    fn parse_event(
        &self,
        raw: &serde_json::Value,
        state: &mut StreamState,
    ) -> Vec<LlmEvent> {
        let mut events = Vec::new();
        let mut lifecycle = StreamLifecycle::new();

        if let Some(usage) = raw.get("usage") {
            state.usage = Some(crate::event::Usage {
                input_tokens: usage["prompt_tokens"].as_u64(),
                output_tokens: usage["completion_tokens"].as_u64(),
                total_tokens: usage["total_tokens"].as_u64(),
                reasoning_tokens: usage["completion_tokens_details"]["reasoning_tokens"].as_u64(),
            });
        }

        if let Some(choices) = raw.get("choices") {
            if let Some(choice) = choices.get(0) {
                if let Some(delta) = choice.get("delta") {
                    if let Some(reasoning) = delta["reasoning_content"].as_str() {
                        if !reasoning.is_empty() {
                            lifecycle.reasoning_delta(&mut events, reasoning);
                        }
                    }

                    if let Some(content) = delta["content"].as_str() {
                        if !content.is_empty() {
                            lifecycle.text_delta(&mut events, content);
                        }
                    }

                    if let Some(tool_calls) = delta["tool_calls"].as_array() {
                        lifecycle.reasoning_end(&mut events);
                        for tool_call in tool_calls {
                            if let Some(index) = tool_call["index"].as_u64() {
                                let idx = index as usize;
                                let name = tool_call["function"]["name"].as_str().unwrap_or("");
                                let args_delta = tool_call["function"]["arguments"].as_str().unwrap_or("");
                                let call_id = tool_call["id"].as_str().unwrap_or("");

                                // Grow state.tool_calls to fit this index
                                while state.tool_calls.len() <= idx {
                                    state.tool_calls.push(serde_json::json!({
                                        "id": "",
                                        "type": "function",
                                        "function": {
                                            "name": "",
                                            "arguments": "",
                                        }
                                    }));
                                }

                                let tc = &mut state.tool_calls[idx];

                                // Set id and name only when present (first chunk for this index)
                                if !call_id.is_empty() {
                                    tc["id"] = serde_json::json!(call_id);
                                }
                                if !name.is_empty() {
                                    tc["function"]["name"] = serde_json::json!(name);
                                }

                                // Append arguments delta
                                let current_args = tc["function"]["arguments"].as_str().unwrap_or("");
                                let combined = format!("{}{}", current_args, args_delta);
                                tc["function"]["arguments"] = serde_json::json!(combined);

                                // Emit events for TUI progress
                                let tc_id = tc["id"].as_str().unwrap_or("").to_string();
                                let tc_name = tc["function"]["name"].as_str().unwrap_or("").to_string();

                                if !call_id.is_empty() || !name.is_empty() {
                                    events.push(LlmEvent::ToolInputStart {
                                        index: idx,
                                        id: tc_id.clone(),
                                        name: tc_name.clone(),
                                    });
                                }
                                events.push(LlmEvent::ToolInputDelta {
                                    index: idx,
                                    id: tc_id,
                                    name: tc_name,
                                    text: combined,
                                });
                            }
                        }
                    }
                }

                if let Some(finish_reason) = choice["finish_reason"].as_str() {
                    lifecycle.finish(&mut events, finish_reason);
                    state.finish_reason = Some(finish_reason.to_string());
                }
            }
        }

        events
    }

    fn terminal_event(&self, state: &StreamState) -> bool {
        state.finish_reason.is_some()
    }
}
