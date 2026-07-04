use crate::tool::Tool;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// A single fixme note entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixmeEntry {
    pub id: String,
    pub phrase: String,
    pub done: bool,
}

/// Manages atomic read/write of the fixme.json file.
pub struct FixmeStore {
    path: PathBuf,
}

impl FixmeStore {
    pub fn new(working_dir: &str) -> Self {
        let path = resolve_fixme_path(working_dir);
        Self { path }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Load all fixmes from the file. Returns empty vec if file doesn't exist.
    pub fn load(&self) -> Result<Vec<FixmeEntry>, String> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let content = std::fs::read_to_string(&self.path)
            .map_err(|e| format!("Failed to read fixme file: {}", e))?;
        if content.trim().is_empty() {
            return Ok(Vec::new());
        }
        serde_json::from_str::<Vec<FixmeEntry>>(&content)
            .map_err(|e| format!("Failed to parse fixme file: {}", e))
    }

    /// Save fixmes to file atomically (write to temp file, then rename).
    pub fn save(&self, fixmes: &[FixmeEntry]) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create fixme dir: {}", e))?;
        }
        let json = serde_json::to_string_pretty(fixmes)
            .map_err(|e| format!("Failed to serialize fixmes: {}", e))?;
        let tmp_path = self.path.with_extension("json.tmp");
        std::fs::write(&tmp_path, &json)
            .map_err(|e| format!("Failed to write fixme file: {}", e))?;
        std::fs::rename(&tmp_path, &self.path)
            .map_err(|e| format!("Failed to rename fixme file: {}", e))?;
        Ok(())
    }

    /// Add a new fixme entry with the given phrase. Returns the created entry.
    pub fn add(&self, phrase: &str) -> Result<FixmeEntry, String> {
        let mut fixmes = self.load()?;
        let entry = FixmeEntry {
            id: Uuid::new_v4().to_string(),
            phrase: phrase.to_string(),
            done: false,
        };
        fixmes.push(entry.clone());
        self.save(&fixmes)?;
        Ok(entry)
    }

    /// Mark a fixme as done or not done by ID.
    pub fn mark_done(&self, id: &str, done: bool) -> Result<(), String> {
        let mut fixmes = self.load()?;
        let mut found = false;
        for fixme in &mut fixmes {
            if fixme.id == id {
                fixme.done = done;
                found = true;
                break;
            }
        }
        if !found {
            return Err(format!("Fixme with id '{}' not found", id));
        }
        self.save(&fixmes)
    }

    /// Get all fixmes.
    pub fn get_all(&self) -> Result<Vec<FixmeEntry>, String> {
        self.load()
    }

    /// Get open (not done) fixmes.
    pub fn get_open(&self) -> Result<Vec<FixmeEntry>, String> {
        let fixmes = self.load()?;
        Ok(fixmes.into_iter().filter(|f| !f.done).collect())
    }
}

fn resolve_fixme_path(working_dir: &str) -> PathBuf {
    let dir = if working_dir == "." {
        std::env::current_dir()
            .map(|d| d.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    } else {
        working_dir.to_string()
    };
    PathBuf::from(dir).join("fixme.json")
}

fn format_fixmes(fixmes: &[FixmeEntry]) -> String {
    if fixmes.is_empty() {
        return "No fixmes found.".to_string();
    }
    serde_json::to_string_pretty(fixmes).unwrap_or_else(|e| format!("Failed to format: {}", e))
}

// --- GetAllFixmesTool ---

pub struct GetAllFixmesTool {
    store: FixmeStore,
}

impl GetAllFixmesTool {
    pub fn with_working_dir(working_dir: &str) -> Self {
        Self {
            store: FixmeStore::new(working_dir),
        }
    }
}

#[async_trait]
impl Tool for GetAllFixmesTool {
    fn name(&self) -> &str {
        "get_all_fixmes"
    }

    fn description(&self) -> &str {
        "Get all fixme notes from fixme.json, regardless of done status. Returns a JSON array of all entries."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(&self, _args: &serde_json::Value) -> Result<String, String> {
        let fixmes = self.store.get_all()?;
        Ok(format_fixmes(&fixmes))
    }
}

// --- GetOpenFixmesTool ---

pub struct GetOpenFixmesTool {
    store: FixmeStore,
}

impl GetOpenFixmesTool {
    pub fn with_working_dir(working_dir: &str) -> Self {
        Self {
            store: FixmeStore::new(working_dir),
        }
    }
}

#[async_trait]
impl Tool for GetOpenFixmesTool {
    fn name(&self) -> &str {
        "get_open_fixmes"
    }

    fn description(&self) -> &str {
        "Get all open (not done) fixme notes from fixme.json. Returns a JSON array of entries with done: false."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(&self, _args: &serde_json::Value) -> Result<String, String> {
        let fixmes = self.store.get_open()?;
        Ok(format_fixmes(&fixmes))
    }
}

// --- CreateFixmeTool ---

pub struct CreateFixmeTool {
    store: FixmeStore,
}

impl CreateFixmeTool {
    pub fn with_working_dir(working_dir: &str) -> Self {
        Self {
            store: FixmeStore::new(working_dir),
        }
    }
}

#[async_trait]
impl Tool for CreateFixmeTool {
    fn name(&self) -> &str {
        "create_fixme"
    }

    fn description(&self) -> &str {
        "Create a new fixme note in fixme.json. Returns the created entry with its generated ID."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "phrase": {
                    "type": "string",
                    "description": "The fixme note text to add."
                }
            },
            "required": ["phrase"]
        })
    }

    async fn execute(&self, args: &serde_json::Value) -> Result<String, String> {
        let phrase = args
            .get("phrase")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'phrase' parameter")?;
        if phrase.trim().is_empty() {
            return Err("phrase must not be empty".to_string());
        }
        let entry = self.store.add(phrase)?;
        Ok(serde_json::to_string_pretty(&entry)
            .unwrap_or_else(|e| format!("Failed to format: {}", e)))
    }
}

// --- MarkFixmeDoneTool ---

pub struct MarkFixmeDoneTool {
    store: FixmeStore,
}

impl MarkFixmeDoneTool {
    pub fn with_working_dir(working_dir: &str) -> Self {
        Self {
            store: FixmeStore::new(working_dir),
        }
    }
}

#[async_trait]
impl Tool for MarkFixmeDoneTool {
    fn name(&self) -> &str {
        "mark_fixme_done"
    }

    fn description(&self) -> &str {
        "Mark a fixme note as done or not done by its ID. Updates fixme.json atomically."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "The ID of the fixme note to update."
                },
                "done": {
                    "type": "boolean",
                    "description": "Set to true to mark as done, false to mark as not done."
                }
            },
            "required": ["id", "done"]
        })
    }

    async fn execute(&self, args: &serde_json::Value) -> Result<String, String> {
        let id = args
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'id' parameter")?;
        let done = args
            .get("done")
            .and_then(|v| v.as_bool())
            .ok_or("Missing 'done' parameter")?;
        self.store.mark_done(id, done)?;
        Ok(format!("Fixme '{}' marked as {}", id, if done { "done" } else { "not done" }))
    }
}
