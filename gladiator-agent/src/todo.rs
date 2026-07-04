//! Transient agent-internal todo list.
//!
//! Unlike `fixme.json` (persisted to disk, shared across sessions), these todos
//! live inside `ConversationState`: they are saved/restored only when the user
//! explicitly dumps/loads agent state, and are otherwise in-memory and per
//! agent. The agent handles `todo_write` / `todo_read` calls inline against its
//! own state — they never reach a `ToolActorRunner` — so the bus carries no
//! execute messages for them.

use serde::{Deserialize, Serialize};

/// Lifecycle status of a single todo item.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl Default for TodoStatus {
    fn default() -> Self {
        TodoStatus::Pending
    }
}

impl TodoStatus {
    /// Parse from an arbitrary string, falling back to `Pending` for anything
    /// unknown. This keeps the tool resilient to slight model misspellings
    /// (e.g. "in-progress", "done") instead of failing the whole call.
    pub fn from_str_loose(s: &str) -> Self {
        let normalized = s.trim().to_lowercase();
        match normalized.as_str() {
            "completed" | "done" | "finished" => TodoStatus::Completed,
            "in_progress" | "in-progress" | "started" | "active" => TodoStatus::InProgress,
            _ => TodoStatus::Pending,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Completed => "completed",
        }
    }

    /// Single-char glyph for compact rendering.
    pub fn glyph(&self) -> &'static str {
        match self {
            TodoStatus::Pending => "[ ]",
            TodoStatus::InProgress => "[~]",
            TodoStatus::Completed => "[x]",
        }
    }
}

/// A single todo entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TodoEntry {
    pub content: String,
    #[serde(default)]
    pub status: TodoStatus,
    #[serde(default)]
    pub priority: String,
}

impl TodoEntry {
    /// Build a TodoEntry from a raw JSON value, tolerating missing/optional
    /// fields. `content` is required; unknown status strings coerce to Pending.
    pub fn from_json(v: &serde_json::Value) -> Result<Self, String> {
        let content = v
            .get("content")
            .and_then(|c| c.as_str())
            .ok_or_else(|| "todo item missing 'content'".to_string())?
            .trim()
            .to_string();
        if content.is_empty() {
            return Err("todo item 'content' must not be empty".to_string());
        }
        let status = v
            .get("status")
            .and_then(|s| s.as_str())
            .map(TodoStatus::from_str_loose)
            .unwrap_or_default();
        let priority = v
            .get("priority")
            .and_then(|p| p.as_str())
            .unwrap_or("medium")
            .to_string();
        Ok(TodoEntry {
            content,
            status,
            priority,
        })
    }
}

/// Names of tool calls handled internally by the agent, never dispatched to a
/// `ToolActorRunner`. Keep this set in sync with `internal_tool_defs`.
pub const INTERNAL_TOOL_NAMES: &[&str] = &["todo_write", "todo_read"];

pub fn is_internal_tool(name: &str) -> bool {
    INTERNAL_TOOL_NAMES.contains(&name)
}

/// OpenAI-compatible tool definitions for every internal tool.
/// Append these to the agent's tool_defs so the model discovers them.
pub fn internal_tool_defs() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "todo_write",
                "description": "Create or replace the agent's transient todo list. Use this to plan multi-step work, track progress, and mark items as completed. The provided list fully replaces any previous list — pass an empty array to clear. Status: 'pending', 'in_progress' (at most one), or 'completed'.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "todos": {
                            "type": "array",
                            "description": "The complete todo list. Each item needs at least 'content' and 'status'.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "content": {"type": "string", "description": "What needs to be done."},
                                    "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]},
                                    "priority": {"type": "string", "enum": ["high", "medium", "low"]}
                                },
                                "required": ["content", "status"]
                            }
                        }
                    },
                    "required": ["todos"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "todo_read",
                "description": "Read the agent's current transient todo list. Returns the full list with statuses. Useful to check progress after resuming a saved session.",
                "parameters": {"type": "object", "properties": {}}
            }
        }),
    ]
}

/// Render the todo list as a compact, human-readable block.
pub fn render_todos(todos: &[TodoEntry]) -> String {
    if todos.is_empty() {
        return "(todo list is empty)".to_string();
    }
    let mut out = String::new();
    for (i, t) in todos.iter().enumerate() {
        out.push_str(&format!(
            "{} {} {} — {}\n",
            t.status.glyph(),
            i + 1,
            t.priority,
            t.content
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_from_str_loose_accepts_variants() {
        assert_eq!(TodoStatus::from_str_loose("completed"), TodoStatus::Completed);
        assert_eq!(TodoStatus::from_str_loose("done"), TodoStatus::Completed);
        assert_eq!(TodoStatus::from_str_loose("In-Progress"), TodoStatus::InProgress);
        assert_eq!(TodoStatus::from_str_loose("pending"), TodoStatus::Pending);
        assert_eq!(TodoStatus::from_str_loose("garbage"), TodoStatus::Pending);
    }

    #[test]
    fn entry_from_json_requires_content() {
        assert!(TodoEntry::from_json(&serde_json::json!({"status":"pending"})).is_err());
        assert!(TodoEntry::from_json(&serde_json::json!({"content":"  "})).is_err());
    }

    #[test]
    fn entry_from_json_tolerates_missing_optional() {
        let e = TodoEntry::from_json(&serde_json::json!({"content":"do thing"})).unwrap();
        assert_eq!(e.status, TodoStatus::Pending);
        assert_eq!(e.priority, "medium");
    }

    #[test]
    fn entry_from_json_coerces_unknown_status() {
        let e = TodoEntry::from_json(&serde_json::json!({"content":"x","status":"finished"})).unwrap();
        assert_eq!(e.status, TodoStatus::Completed);
    }

    #[test]
    fn entry_from_json_full() {
        let e = TodoEntry::from_json(&serde_json::json!({
            "content": "write tests", "status": "in_progress", "priority": "high"
        })).unwrap();
        assert_eq!(e.content, "write tests");
        assert_eq!(e.status, TodoStatus::InProgress);
        assert_eq!(e.priority, "high");
    }

    #[test]
    fn render_empty_and_filled() {
        assert_eq!(render_todos(&[]), "(todo list is empty)");
        let list = vec![
            TodoEntry { content: "a".into(), status: TodoStatus::Completed, priority: "low".into() },
            TodoEntry { content: "b".into(), status: TodoStatus::InProgress, priority: "high".into() },
        ];
        let rendered = render_todos(&list);
        assert!(rendered.contains("[x] 1 low — a"));
        assert!(rendered.contains("[~] 2 high — b"));
    }

    #[test]
    fn entry_roundtrips_through_json() {
        let e = TodoEntry {
            content: "ship it".into(),
            status: TodoStatus::InProgress,
            priority: "high".into(),
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: TodoEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn internal_tool_defs_have_expected_names() {
        let defs = internal_tool_defs();
        let names: Vec<&str> = defs
            .iter()
            .map(|d| d["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["todo_write", "todo_read"]);
    }

    #[test]
    fn is_internal_tool_matches_known_only() {
        assert!(is_internal_tool("todo_write"));
        assert!(is_internal_tool("todo_read"));
        assert!(!is_internal_tool("bash"));
        assert!(!is_internal_tool("todo_list"));
    }
}
