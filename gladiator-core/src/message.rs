use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum UiMessageType {
    UserInput,
    LlmThinking,
    LlmStream,
    LlmStreamEnd,
    LlmToolCall,
    LlmToolResult,
    LlmToolCalls,
    LlmDump,
    Echo,
    Log,
    Info,
    Error,
    Warning,
    Continue,
    Interrupt,
    StreamStats,
}

impl fmt::Display for UiMessageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UiMessageType::UserInput => write!(f, "UserInput"),
            UiMessageType::LlmThinking => write!(f, "LlmThinking"),
            UiMessageType::LlmStream => write!(f, "LlmStream"),
            UiMessageType::LlmStreamEnd => write!(f, "LlmStreamEnd"),
            UiMessageType::LlmToolCall => write!(f, "LlmToolCall"),
            UiMessageType::LlmToolResult => write!(f, "LlmToolResult"),
            UiMessageType::LlmToolCalls => write!(f, "LlmToolCalls"),
            UiMessageType::LlmDump => write!(f, "LlmDump"),
            UiMessageType::Echo => write!(f, "Echo"),
            UiMessageType::Log => write!(f, "Log"),
            UiMessageType::Info => write!(f, "Info"),
            UiMessageType::Error => write!(f, "Error"),
            UiMessageType::Warning => write!(f, "Warning"),
            UiMessageType::Continue => write!(f, "Continue"),
            UiMessageType::Interrupt => write!(f, "Interrupt"),
            UiMessageType::StreamStats => write!(f, "StreamStats"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    #[serde(default)]
    pub reference: String,
    pub topic: String,
    pub source: String,
    pub payload: serde_json::Value,
    #[serde(default)]
    pub meta: serde_json::Value,
}

impl Message {
    pub fn new(
        topic: impl Into<String>,
        source: impl Into<String>,
        payload: impl Into<serde_json::Value>,
    ) -> Self {
        Self {
            reference: uuid::Uuid::new_v4().to_string(),
            topic: topic.into(),
            source: source.into(),
            payload: payload.into(),
            meta: serde_json::Value::Null,
        }
    }

    pub fn with_reference(
        reference: String,
        topic: impl Into<String>,
        source: impl Into<String>,
        payload: impl Into<serde_json::Value>,
    ) -> Self {
        Self {
            reference,
            topic: topic.into(),
            source: source.into(),
            payload: payload.into(),
            meta: serde_json::Value::Null,
        }
    }

    pub fn text(
        topic: impl Into<String>,
        source: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self::new(topic, source, text.into())
    }

    pub fn with_type(mut self, item_type: impl Into<String>) -> Self {
        if let serde_json::Value::Object(ref mut obj) = self.meta {
            obj.insert("type".to_string(), serde_json::Value::String(item_type.into().to_string()));
        } else {
            self.meta = serde_json::json!({"type": item_type.into().to_string()});
        }
        self
    }

    pub fn with_stream_id(mut self, stream_id: String) -> Self {
        if let serde_json::Value::Object(ref mut obj) = self.meta {
            obj.insert("stream_id".to_string(), serde_json::Value::String(stream_id));
        } else {
            self.meta = serde_json::json!({"stream_id": stream_id});
        }
        self
    }

    pub fn meta_type(&self) -> Option<&str> {
        self.meta.get("type").and_then(|t| t.as_str())
    }

    /// Set the subagent indentation depth on this message's metadata.
    /// Used by the agent actor to mark messages emitted during nested
    /// call_subagent execution so the TUI can prefix lines with "| ".
    pub fn with_depth(mut self, depth: usize) -> Self {
        if !self.meta.is_object() {
            self.meta = serde_json::json!({});
        }
        if let serde_json::Value::Object(ref mut obj) = self.meta {
            obj.insert("depth".to_string(), serde_json::json!(depth));
        }
        self
    }

    /// Retrieve the subagent indentation depth from metadata, or 0 (top-level).
    pub fn meta_depth(&self) -> usize {
        self.meta.get("depth").and_then(|d| d.as_u64()).unwrap_or(0) as usize
    }

    pub fn stream_id(&self) -> Option<String> {
        self.meta.get("stream_id").and_then(|s| s.as_str()).map(|s| s.to_string())
    }

    pub fn payload_str(&self) -> Option<String> {
        match &self.payload {
            serde_json::Value::String(s) => Some(s.clone()),
            _ => None,
        }
    }

    pub fn payload_as_json<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_value(self.payload.clone())
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {} -> {}", self.topic, self.source, self.payload)
    }
}
