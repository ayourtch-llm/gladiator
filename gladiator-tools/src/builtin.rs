use crate::tool::Tool;
use async_trait::async_trait;
use std::path::Path;
use std::process::Stdio;


const MAX_READ_SIZE: usize = 1_048_576; // 1 MiB
const MAX_WRITE_SIZE: usize = 10_485_760; // 10 MiB
const DEFAULT_TIMEOUT_MS: u64 = 120_000; // 120s
const MAX_TIMEOUT_MS: u64 = 600_000; // 10 min

const DANGEROUS_PATTERNS: &[&str] = &[
    "rm -rf /",
    "sudo rm",
    ":(){ :|:& };:",
    "chmod -R 777 /",
    "dd if=",
];

// --- ReadFileTool ---

pub struct ReadFileTool {
    working_dir: String,
}

impl ReadFileTool {
    pub fn new() -> Self {
        Self {
            working_dir: ".".to_string(),
        }
    }

    pub fn with_working_dir(working_dir: impl Into<String>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file. Supports optional offset (1-based line number) and limit (line count) for reading specific ranges. Maximum file size: 1 MiB."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute or relative path to the file to read."
                },
                "offset": {
                    "type": "integer",
                    "description": "Optional 1-based starting line number. Defaults to 1."
                },
                "limit": {
                    "type": "integer",
                    "description": "Optional number of lines to read. Defaults to reading to end of file."
                }
            },
            "required": ["file_path"]
        })
    }

    async fn execute(&self, args: &serde_json::Value) -> Result<String, String> {
        let file_path = resolve_path(args, "file_path", &self.working_dir)?;

        let path = Path::new(&file_path);
        if !path.exists() {
            return Err(format!("Read failed: file not found: {}", file_path));
        }
        if path.is_dir() {
            return Err(format!("Read failed: path is a directory: {}", file_path));
        }

        let metadata = std::fs::metadata(path)
            .map_err(|e| format!("Read failed: {}", e))?;
        if metadata.len() as usize > MAX_READ_SIZE {
            return Err(format!(
                "Read failed: file size {} bytes exceeds max of {} bytes",
                metadata.len(),
                MAX_READ_SIZE
            ));
        }

        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Read failed: {}", e))?;

        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();

        let offset = args
            .get("offset")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(1)
            .max(1)
            .min(total.max(1));

        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        let end = match limit {
            Some(l) => (offset - 1 + l).min(total),
            None => total,
        };

        let result: Vec<&str> = lines[offset - 1..end].to_vec();
        Ok(result.join("\n"))
    }
}

// --- WriteFileTool ---

pub struct WriteFileTool {
    working_dir: String,
}

impl WriteFileTool {
    pub fn new() -> Self {
        Self {
            working_dir: ".".to_string(),
        }
    }

    pub fn with_working_dir(working_dir: impl Into<String>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file. Creates parent directories automatically. Overwrites existing file content. Maximum content size: 10 MiB."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The path to the file to write. Parent directories are created automatically."
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file. Overwrites any existing content."
                }
            },
            "required": ["file_path", "content"]
        })
    }

    async fn execute(&self, args: &serde_json::Value) -> Result<String, String> {
        let file_path = resolve_path(args, "file_path", &self.working_dir)?;
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or("Write failed: missing 'content' parameter")?
            .to_string();

        if content.len() > MAX_WRITE_SIZE {
            return Err(format!(
                "Write failed: content size {} bytes exceeds max of {} bytes",
                content.len(),
                MAX_WRITE_SIZE
            ));
        }

        let path = Path::new(&file_path);
        if path.exists() && path.is_dir() {
            return Err(format!("Write failed: path is a directory: {}", file_path));
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Write failed: {}", e))?;
        }

        std::fs::write(path, &content)
            .map_err(|e| format!("Write failed: {}", e))?;

        Ok(format!("Successfully wrote {} bytes to {}", content.len(), file_path))
    }
}

// --- EditFileTool ---

pub struct EditFileTool {
    working_dir: String,
}

impl EditFileTool {
    pub fn new() -> Self {
        Self {
            working_dir: ".".to_string(),
        }
    }

    pub fn with_working_dir(working_dir: impl Into<String>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }
}

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Edit a file by finding old_content and replacing it with new_content. Replaces all occurrences. old_content must not be empty and must differ from new_content."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the file to edit."
                },
                "old_content": {
                    "type": "string",
                    "description": "The existing content to find in the file."
                },
                "new_content": {
                    "type": "string",
                    "description": "The replacement content."
                }
            },
            "required": ["file_path", "old_content", "new_content"]
        })
    }

    async fn execute(&self, args: &serde_json::Value) -> Result<String, String> {
        let file_path = resolve_path(args, "file_path", &self.working_dir)?;
        let old_content = args
            .get("old_content")
            .and_then(|v| v.as_str())
            .ok_or("Edit failed: missing 'old_content' parameter")?
            .to_string();
        let new_content = args
            .get("new_content")
            .and_then(|v| v.as_str())
            .ok_or("Edit failed: missing 'new_content' parameter")?
            .to_string();

        if old_content.is_empty() {
            return Err("Edit failed: old_content must not be empty".to_string());
        }
        if old_content == new_content {
            return Err("Edit failed: old_content must differ from new_content".to_string());
        }

        let path = Path::new(&file_path);
        if !path.exists() {
            return Err(format!("Edit failed: file not found: {}", file_path));
        }
        if path.is_dir() {
            return Err(format!("Edit failed: path is a directory: {}", file_path));
        }

        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Edit failed: {}", e))?;

        if !content.contains(&old_content) {
            return Err(format!(
                "Edit failed: old_content not found in {}. Re-read the file to get the current content.",
                file_path
            ));
        }

        let count = content.matches(&old_content).count();
        let new_file_content = content.replace(&old_content, &new_content);

        std::fs::write(path, &new_file_content)
            .map_err(|e| format!("Edit failed: {}", e))?;

        Ok(format!(
            "Successfully edited {} ({} replacement(s))",
            file_path, count
        ))
    }
}

// --- BashTool ---

pub struct BashTool {
    working_dir: String,
}

impl BashTool {
    pub fn new() -> Self {
        Self {
            working_dir: ".".to_string(),
        }
    }

    pub fn with_working_dir(working_dir: impl Into<String>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a shell command via bash -c and return stdout, stderr, and exit code. Supports optional timeout (default 120s, max 10min) and working directory. Dangerous commands are blocked."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute. Runs via bash -c."
                },
                "timeout": {
                    "type": "integer",
                    "description": "Optional timeout in milliseconds. Default: 120000, max: 600000."
                },
                "working_dir": {
                    "type": "string",
                    "description": "Optional working directory for command execution."
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: &serde_json::Value) -> Result<String, String> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or("Execution failed: missing 'command' parameter")?;

        if command.trim().is_empty() {
            return Err("Execution failed: empty command".to_string());
        }

        for pattern in DANGEROUS_PATTERNS {
            if command.contains(pattern) {
                return Err(format!(
                    "Security check failed: Command blocked: contains dangerous pattern '{}'",
                    pattern
                ));
            }
        }

        let timeout_ms = args
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_MS);
        if timeout_ms == 0 {
            return Err("Invalid timeout: timeout must be > 0".to_string());
        }
        if timeout_ms > MAX_TIMEOUT_MS {
            return Err(format!(
                "Invalid timeout: timeout must be <= {} ms",
                MAX_TIMEOUT_MS
            ));
        }
        let timeout_secs = timeout_ms as f64 / 1000.0;

        let work_dir = args
            .get("working_dir")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.working_dir.clone());

        let work_path = Path::new(&work_dir);
        if !work_path.exists() {
            return Err(format!("Execution failed: working directory not found: {}", work_dir));
        }
        if !work_path.is_dir() {
            return Err(format!("Execution failed: working directory is not a directory: {}", work_dir));
        }

        let output = tokio::time::timeout(
            std::time::Duration::from_secs_f64(timeout_secs),
            tokio::process::Command::new("bash")
                .arg("-c")
                .arg(command)
                .current_dir(work_path)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .stdin(Stdio::null())
                .output(),
        )
        .await
        .map_err(|_| {
            format!("Execution failed: Command timed out after {} milliseconds", timeout_ms)
        })?
        .map_err(|e| format!("Execution failed: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);

        let result = if stderr.is_empty() {
            format!(
                "Command: {}\nExit code: {}\nSTDOUT:\n{}",
                command, exit_code, stdout
            )
        } else {
            format!(
                "Command: {}\nExit code: {}\nSTDOUT:\n{}\nSTDERR:\n{}",
                command, exit_code, stdout, stderr
            )
        };

        Ok(result)
    }
}

// --- GlobTool ---

pub struct GlobTool {
    working_dir: String,
}

impl GlobTool {
    pub fn new() -> Self {
        Self {
            working_dir: ".".to_string(),
        }
    }

    pub fn with_working_dir(working_dir: impl Into<String>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "List files matching a glob pattern. Respects .gitignore files. Limited to 1000 results. Use ** for recursive search."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern for matching files (e.g., 'src/**/*.rs', '**/*.json'). Use ** for recursive search."
                },
                "path": {
                    "type": "string",
                    "description": "The root directory to search in. Defaults to current working directory."
                }
            }
        })
    }

    async fn execute(&self, args: &serde_json::Value) -> Result<String, String> {
        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("*")
            .to_string();

        let search_root = args
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.working_dir.clone());

        let root_path = Path::new(&search_root);
        if !root_path.exists() {
            return Err(format!("Search failed: directory not found: {}", search_root));
        }
        if !root_path.is_dir() {
            return Err(format!("Search failed: path is not a directory: {}", search_root));
        }

        let glob_pattern = glob::Pattern::new(&pattern)
            .map_err(|e| format!("Search failed: Invalid glob pattern: {}", e))?;

        let match_opts = glob::MatchOptions {
            require_literal_separator: true,
            case_sensitive: true,
            ..Default::default()
        };

        let mut results: Vec<String> = Vec::new();
        let max_results = 1000;

        let walker = ignore::WalkBuilder::new(&search_root)
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .build();

        for entry in walker {
            if let Ok(entry) = entry {
                if entry.file_type().map_or(false, |t| t.is_dir()) {
                    continue;
                }

                let path = entry.path();
                let rel = path
                    .strip_prefix(root_path)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .to_string();

                if glob_pattern.matches_with(&rel, match_opts) {
                    if results.len() < max_results {
                        results.push(rel);
                    } else {
                        break;
                    }
                }
            }
        }

        results.sort();

        if results.is_empty() {
            Ok(format!(
                "No files found matching pattern: '{}'\nSearched in: {}\nTip: Use ** for recursive search (e.g., 'src/**/*.rs')",
                pattern, search_root
            ))
        } else {
            let files = results.join("\n");
            Ok(format!("Found {} file(s) matching '{}':\n{}", results.len(), pattern, files))
        }
    }
}

// --- GrepTool ---

pub struct GrepTool {
    working_dir: String,
}

impl GrepTool {
    pub fn new() -> Self {
        Self {
            working_dir: ".".to_string(),
        }
    }

    pub fn with_working_dir(working_dir: impl Into<String>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents using a regex pattern. Returns matching file paths and lines. Searches recursively."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for in file contents."
                },
                "path": {
                    "type": "string",
                    "description": "The root directory to search in. Defaults to current working directory."
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: &serde_json::Value) -> Result<String, String> {
        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or("Search failed: missing 'pattern' parameter")?;

        let search_root = args
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.working_dir.clone());

        let root_path = Path::new(&search_root);
        if !root_path.exists() {
            return Err(format!("Search failed: directory not found: {}", search_root));
        }
        if !root_path.is_dir() {
            return Err(format!("Search failed: path is not a directory: {}", search_root));
        }

        let regex = regex::Regex::new(pattern)
            .map_err(|e| format!("Search failed: invalid regex: {}", e))?;

        let mut results: Vec<String> = Vec::new();
        let max_results = 1000;

        let walker = ignore::WalkBuilder::new(&search_root)
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .build();

        for entry in walker {
            if let Ok(entry) = entry {
                if entry.file_type().map_or(true, |t| t.is_dir()) {
                    continue;
                }

                let path = entry.path();
                let rel = path
                    .strip_prefix(root_path)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .to_string();

                if let Ok(content) = std::fs::read_to_string(path) {
                    for (line_num, line) in content.lines().enumerate() {
                        if regex.is_match(line) {
                            let match_text = format!("{}:{}", rel, line_num + 1);
                            if results.len() < max_results {
                                results.push(format!("{}: {}", match_text, line.trim()));
                            } else {
                                break;
                            }
                        }
                    }
                }
            }
        }

        if results.is_empty() {
            Ok(format!("No matches found for pattern '{}' in {}", pattern, search_root))
        } else {
            let output = results.join("\n");
            Ok(format!("Found {} match(es):\n{}", results.len(), output))
        }
    }
}

// --- Helper functions ---

fn resolve_path(
    args: &serde_json::Value,
    key: &str,
    working_dir: &str,
) -> Result<String, String> {
    let path_str = args
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("Missing '{}' parameter", key))?;

    if path_str.starts_with('/') || path_str.starts_with("~") {
        // Absolute path or home directory
        Ok(path_str.to_string())
    } else {
        // Relative path - resolve against working_dir
        let wd = if working_dir == "." {
            std::env::current_dir()
                .map_err(|e| format!("Failed to get current dir: {}", e))?
                .to_string_lossy()
                .to_string()
        } else {
            working_dir.to_string()
        };
        Ok(format!("{}/{}", wd, path_str))
    }
}
