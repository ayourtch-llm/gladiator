//! Agent-internal tools: handled inline against `ConversationState`, never
//! dispatched to a `ToolActorRunner`. Currently two families live here:
//!
//! - **todo_write / todo_read**: transient per-agent todo list (saved/restored
//!   with the conversation state, not a separate disk file).
//! - **restart_from_file**: snapshot current context to `/tmp`, wipe the
//!   conversation, and inject fresh instructions read from a file — used to
//!   shed a bloated/corrupted context and continue from a handoff note.
//!
//! All internal tools share the registry primitives below (`INTERNAL_TOOL_NAMES`,
//! `is_internal_tool`, `internal_tool_defs`); the agent's dispatch loop checks
//! `is_internal_tool` to short-circuit before publishing an execute message.

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
pub const INTERNAL_TOOL_NAMES: &[&str] =
    &["todo_write", "todo_read", "restart_from_file",
      "set_context_reminder", "schedule_wake_up"];

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
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "restart_from_file",
                "description": "Reset the conversation context and continue from a fresh instruction file. Saves a backup of the current context to /tmp/<pid>-<datetime>.json, clears ALL conversation history and todos, then injects the file's contents as a new user instruction with a directive to continue executing. Call this ONLY when context is bloated or corrupted and you have written a handoff note to a file. Do NOT batch this with other tool calls — invoke it alone.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "filename": {
                            "type": "string",
                            "description": "Path to the handoff/instruction file. Relative paths resolve against the agent working directory; absolute paths used as-is."
                        }
                    },
                    "required": ["filename"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "set_context_reminder",
                "description": "Set a one-shot context-usage reminder. When the agent's token usage crosses the specified threshold, the given message is injected once into the agent loop as a user message (e.g., 'Your context is at 150k tokens — please do a context refresh now'). The reminder fires only once per session and does not repeat.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "threshold_tokens": {
                            "type": "number",
                            "description": "The token count at which the reminder fires. When input_tokens exceeds this value, the message is injected."
                        },
                        "message": {
                            "type": "string",
                            "description": "The message to inject when the threshold is crossed."
                        }
                    },
                    "required": ["threshold_tokens", "message"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "schedule_wake_up",
                "description": "Schedule a wake-up message to be injected into the agent loop at a future time. Supports one-shot (fires once after delay_seconds) and recurring/cron mode (fires every interval_seconds). The message is only injected when the agent loop is idle; if busy, one-shot wake-ups are deferred until idle and cron wake-ups reschedule to the next interval.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "delay_seconds": {
                            "type": "number",
                            "description": "Seconds from now until the first firing."
                        },
                        "message": {
                            "type": "string",
                            "description": "The message to inject when the wake-up fires."
                        },
                        "interval_seconds": {
                            "type": "number",
                            "description": "If provided, the wake-up recurs every interval_seconds (cron mode). If omitted, it's one-shot."
                        }
                    },
                    "required": ["delay_seconds", "message"]
                }
            }
        }),
    ]
}

/// Outcome of an internal tool invocation. `context_reset` signals that the
/// handler rebuilt the conversation from scratch (currently only
/// `restart_from_file`): in that case the dispatch loop must NOT append a tool
/// result, because the assistant tool_calls message it would answer has been
/// wiped along with the rest of the history.
#[derive(Debug, Clone)]
pub struct InternalToolOutcome {
    pub result_text: String,
    pub success: bool,
    pub context_reset: bool,
}

impl InternalToolOutcome {
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            result_text: text.into(),
            success: true,
            context_reset: false,
        }
    }

    pub fn err(text: impl Into<String>) -> Self {
        Self {
            result_text: text.into(),
            success: false,
            context_reset: false,
        }
    }

    pub fn with_reset(mut self, text: impl Into<String>) -> Self {
        self.result_text = text.into();
        self.context_reset = true;
        self
    }
}

/// Build the `/tmp/<pid>-<datetime>.json` backup filename for a context dump.
/// `datetime` is UTC `YYYYmmdd-HHMMSS` so filenames sort chronologically and
/// stay readable without a decoder. Pure function over epoch seconds so it can
/// be unit-tested without touching the clock.
pub fn backup_filename(pid: u32, epoch_secs: u64) -> String {
    format!("{}-{}.json", pid, format_utc_datetime(epoch_secs))
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

/// Wrap the file contents read by `restart_from_file` into the injected user
/// instruction. Exported so the agent layer can call it after performing the
/// file read + state clear itself.
pub fn build_restart_instruction(file_content: &str) -> String {
    format!(
        "[Context restarted from file — prior history was backed up to /tmp.]\n\
         ---BEGIN HANDOFF---\n\
         {}\n\
         ---END HANDOFF---\n\
         Continue executing the work described above. Re-establish any needed \
         context from the handoff, check the todo list if relevant, and proceed.",
        file_content
    )
}

/// Format epoch seconds as UTC `YYYYmmdd-HHMMSS` using Howard Hinnant's
/// days-from-civil algorithm — no chrono dependency required.
fn format_utc_datetime(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let sod = secs % 86400; // seconds of day

    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };

    let hh = sod / 3600;
    let mm = (sod % 3600) / 60;
    let ss = sod % 60;
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        year, m, d, hh, mm, ss
    )
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
        assert_eq!(names, vec!["todo_write", "todo_read", "restart_from_file", "set_context_reminder", "schedule_wake_up"]);
    }

    #[test]
    fn is_internal_tool_matches_known_only() {
        assert!(is_internal_tool("todo_write"));
        assert!(is_internal_tool("todo_read"));
        assert!(is_internal_tool("restart_from_file"));
        assert!(is_internal_tool("set_context_reminder"));
        assert!(is_internal_tool("schedule_wake_up"));
        assert!(!is_internal_tool("bash"));
        assert!(!is_internal_tool("todo_list"));
    }

    #[test]
    fn format_utc_datetime_epoch_is_1970() {
        assert_eq!(format_utc_datetime(0), "19700101-000000");
    }

    #[test]
    fn format_utc_datetime_known_instant() {
        // 2025-01-01 00:00:00 UTC = 1735689600
        assert_eq!(format_utc_datetime(1735689600), "20250101-000000");
    }

    #[test]
    fn format_utc_datetime_midday() {
        // 2024-02-29 12:34:56 UTC (leap day) = 1709210096
        assert_eq!(format_utc_datetime(1709210096), "20240229-123456");
    }

    #[test]
    fn backup_filename_shape() {
        let name = backup_filename(12345, 1735689600);
        assert_eq!(name, "12345-20250101-000000.json");
        assert!(name.ends_with(".json"));
    }

    #[test]
    fn restart_instruction_wraps_and_directs_continuation() {
        let body = "## Goal\nShip feature X.";
        let instr = build_restart_instruction(body);
        assert!(instr.contains("[Context restarted from file"));
        assert!(instr.contains("## Goal\nShip feature X."));
        assert!(instr.contains("Continue executing"));
        assert!(instr.contains("---BEGIN HANDOFF---"));
        assert!(instr.contains("---END HANDOFF---"));
    }

    #[test]
    fn outcome_constructors_set_flags() {
        let ok = InternalToolOutcome::ok("done");
        assert!(ok.success);
        assert!(!ok.context_reset);
        assert_eq!(ok.result_text, "done");

        let err = InternalToolOutcome::err("boom");
        assert!(!err.success);
        assert!(!err.context_reset);

        let reset = InternalToolOutcome::ok("ignored").with_reset("cleared");
        assert!(reset.success);
        assert!(reset.context_reset);
        assert_eq!(reset.result_text, "cleared");
    }
}
