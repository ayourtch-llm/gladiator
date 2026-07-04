use crate::tool::Tool;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// A single recorded conclusion / decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConclusionEntry {
    pub id: String,
    pub text: String,
}

/// Manages atomic read/write of the conclusions.json file.
pub struct ConclusionStore {
    path: PathBuf,
}

impl ConclusionStore {
    pub fn new(working_dir: &str) -> Self {
        let path = resolve_conclusions_path(working_dir);
        Self { path }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Load all conclusions from the file. Returns empty vec if file doesn't exist.
    pub fn load(&self) -> Result<Vec<ConclusionEntry>, String> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let content = std::fs::read_to_string(&self.path)
            .map_err(|e| format!("Failed to read conclusions file: {}", e))?;
        if content.trim().is_empty() {
            return Ok(Vec::new());
        }
        serde_json::from_str::<Vec<ConclusionEntry>>(&content)
            .map_err(|e| format!("Failed to parse conclusions file: {}", e))
    }

    /// Save conclusions to file atomically (write to temp file, then rename).
    pub fn save(&self, conclusions: &[ConclusionEntry]) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create conclusions dir: {}", e))?;
        }
        let json = serde_json::to_string_pretty(conclusions)
            .map_err(|e| format!("Failed to serialize conclusions: {}", e))?;
        let tmp_path = self.path.with_extension("json.tmp");
        std::fs::write(&tmp_path, &json)
            .map_err(|e| format!("Failed to write conclusions file: {}", e))?;
        std::fs::rename(&tmp_path, &self.path)
            .map_err(|e| format!("Failed to rename conclusions file: {}", e))?;
        Ok(())
    }

    /// Append a new conclusion with the given text. Returns the created entry.
    pub fn add(&self, text: &str) -> Result<ConclusionEntry, String> {
        let mut conclusions = self.load()?;
        let entry = ConclusionEntry {
            id: Uuid::new_v4().to_string(),
            text: text.to_string(),
        };
        conclusions.push(entry.clone());
        self.save(&conclusions)?;
        Ok(entry)
    }

    /// Get all conclusions.
    pub fn get_all(&self) -> Result<Vec<ConclusionEntry>, String> {
        self.load()
    }
}

fn resolve_conclusions_path(working_dir: &str) -> PathBuf {
    let dir = if working_dir == "." {
        std::env::current_dir()
            .map(|d| d.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    } else {
        working_dir.to_string()
    };
    PathBuf::from(dir).join("conclusions.json")
}

fn format_conclusions(conclusions: &[ConclusionEntry]) -> String {
    if conclusions.is_empty() {
        return "No conclusions recorded yet.".to_string();
    }
    serde_json::to_string_pretty(conclusions).unwrap_or_else(|e| format!("Failed to format: {}", e))
}

// --- RecordConclusionTool ---

pub struct RecordConclusionTool {
    store: ConclusionStore,
}

impl RecordConclusionTool {
    pub fn with_working_dir(working_dir: &str) -> Self {
        Self {
            store: ConclusionStore::new(working_dir),
        }
    }
}

#[async_trait]
impl Tool for RecordConclusionTool {
    fn name(&self) -> &str {
        "record_conclusion"
    }

    fn description(&self) -> &str {
        "Record a distilled conclusion, decision, or key finding so it becomes a durable part of the conversation and survives across turns. Call this at phase boundaries (after exploring/understanding something, before making significant edits, after verifying a result) to anchor what you have decided and why. Keep the text short and specific — a sentence or two, not a transcript of your thinking."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The distilled conclusion or decision to record. One or two sentences: what you concluded and why / what you will do next."
                }
            },
            "required": ["text"]
        })
    }

    async fn execute(&self, args: &serde_json::Value) -> Result<String, String> {
        let text = args
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'text' parameter")?;
        if text.trim().is_empty() {
            return Err("'text' must not be empty".to_string());
        }
        self.store.add(text)?;
        Ok("Conclusion recorded.".to_string())
    }
}

// --- GetConclusionsTool ---

pub struct GetConclusionsTool {
    store: ConclusionStore,
}

impl GetConclusionsTool {
    pub fn with_working_dir(working_dir: &str) -> Self {
        Self {
            store: ConclusionStore::new(working_dir),
        }
    }
}

#[async_trait]
impl Tool for GetConclusionsTool {
    fn name(&self) -> &str {
        "get_conclusions"
    }

    fn description(&self) -> &str {
        "Get all conclusions recorded so far from conclusions.json. Returns a JSON array of entries. Use this to recall earlier decisions, especially after a long stretch of tool use or when resuming a saved session."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(&self, _args: &serde_json::Value) -> Result<String, String> {
        let conclusions = self.store.get_all()?;
        Ok(format_conclusions(&conclusions))
    }
}
