use gladiator_tools::builtin::{
    BashTool, EditFileTool, GlobTool, GrepTool, ReadFileTool, WriteFileTool,
};
use gladiator_tools::Tool;
use serde_json::json;
use std::fs;
use tempfile::TempDir;

// --- ReadFileTool tests ---

#[tokio::test]
async fn test_read_file_basic() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("test.txt");
    fs::write(&path, "line1\nline2\nline3").unwrap();

    let tool = ReadFileTool::new();
    let args = json!({"file_path": path.to_str().unwrap()});
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("line1"));
    assert!(result.contains("line2"));
    assert!(result.contains("line3"));
}

#[tokio::test]
async fn test_read_file_with_offset() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("test.txt");
    fs::write(&path, "line1\nline2\nline3\nline4\nline5").unwrap();

    let tool = ReadFileTool::new();
    let args = json!({"file_path": path.to_str().unwrap(), "offset": 3});
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("line3"));
    assert!(result.contains("line4"));
    assert!(result.contains("line5"));
    assert!(!result.contains("line1"));
    assert!(!result.contains("line2"));
}

#[tokio::test]
async fn test_read_file_with_offset_and_limit() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("test.txt");
    fs::write(&path, "line1\nline2\nline3\nline4\nline5").unwrap();

    let tool = ReadFileTool::new();
    let args = json!({"file_path": path.to_str().unwrap(), "offset": 2, "limit": 2});
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("line2"));
    assert!(result.contains("line3"));
    assert!(!result.contains("line1"));
    assert!(!result.contains("line4"));
    assert!(!result.contains("line5"));
}

#[tokio::test]
async fn test_read_file_not_found() {
    let tool = ReadFileTool::new();
    let args = json!({"file_path": "/nonexistent/path/file.txt"});
    let result = tool.execute(&args).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("not found") || err.contains("Read failed"));
}

#[tokio::test]
async fn test_read_file_directory_error() {
    let tmp = TempDir::new().unwrap();
    let tool = ReadFileTool::new();
    let args = json!({"file_path": tmp.path().to_str().unwrap()});
    let result = tool.execute(&args).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_read_file_name() {
    let tool = ReadFileTool::new();
    assert_eq!(tool.name(), "read_file");
}

// --- WriteFileTool tests ---

#[tokio::test]
async fn test_write_file_basic() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("output.txt");

    let tool = WriteFileTool::new();
    let args = json!({"file_path": path.to_str().unwrap(), "content": "hello world"});
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("Successfully wrote"));

    let content = fs::read_to_string(&path).unwrap();
    assert_eq!(content, "hello world");
}

#[tokio::test]
async fn test_write_file_creates_parent_dirs() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("subdir1/subdir2/file.txt");

    let tool = WriteFileTool::new();
    let args = json!({"file_path": path.to_str().unwrap(), "content": "nested"});
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("Successfully wrote"));

    let content = fs::read_to_string(&path).unwrap();
    assert_eq!(content, "nested");
}

#[tokio::test]
async fn test_write_file_overwrites() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("file.txt");
    fs::write(&path, "old content").unwrap();

    let tool = WriteFileTool::new();
    let args = json!({"file_path": path.to_str().unwrap(), "content": "new content"});
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("Successfully wrote"));

    let content = fs::read_to_string(&path).unwrap();
    assert_eq!(content, "new content");
}

#[tokio::test]
async fn test_write_file_name() {
    let tool = WriteFileTool::new();
    assert_eq!(tool.name(), "write_file");
}

// --- EditFileTool tests ---

#[tokio::test]
async fn test_edit_file_basic() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("file.txt");
    fs::write(&path, "hello world\nfoo bar").unwrap();

    let tool = EditFileTool::new();
    let args = json!({
        "file_path": path.to_str().unwrap(),
        "old_content": "hello world",
        "new_content": "hello rust"
    });
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("Successfully edited"));

    let content = fs::read_to_string(&path).unwrap();
    assert!(content.contains("hello rust"));
    assert!(!content.contains("hello world"));
}

#[tokio::test]
async fn test_edit_file_not_found() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("file.txt");
    fs::write(&path, "hello world").unwrap();

    let tool = EditFileTool::new();
    let args = json!({
        "file_path": path.to_str().unwrap(),
        "old_content": "nonexistent text",
        "new_content": "replacement"
    });
    let result = tool.execute(&args).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not found"));
}

#[tokio::test]
async fn test_edit_file_replace_all() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("file.txt");
    fs::write(&path, "foo foo foo").unwrap();

    let tool = EditFileTool::new();
    let args = json!({
        "file_path": path.to_str().unwrap(),
        "old_content": "foo",
        "new_content": "bar"
    });
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("3"));

    let content = fs::read_to_string(&path).unwrap();
    assert_eq!(content, "bar bar bar");
}

#[tokio::test]
async fn test_edit_file_name() {
    let tool = EditFileTool::new();
    assert_eq!(tool.name(), "edit_file");
}

// --- BashTool tests ---

#[tokio::test]
async fn test_bash_echo() {
    let tool = BashTool::new();
    let args = json!({"command": "echo hello"});
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("hello"));
}

#[tokio::test]
async fn test_bash_exit_code() {
    let tool = BashTool::new();
    let args = json!({"command": "exit 3"});
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("Exit code: 3"));
}

#[tokio::test]
async fn test_bash_stderr() {
    let tool = BashTool::new();
    let args = json!({"command": "echo err >&2"});
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("err"));
}

#[tokio::test]
async fn test_bash_dangerous_blocked() {
    let tool = BashTool::new();
    let args = json!({"command": "rm -rf /"});
    let result = tool.execute(&args).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("blocked") || err.contains("dangerous"));
}

#[tokio::test]
async fn test_bash_working_dir() {
    let tmp = TempDir::new().unwrap();
    let tool = BashTool::new();
    let args = json!({
        "command": "pwd",
        "working_dir": tmp.path().to_str().unwrap()
    });
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains(tmp.path().to_str().unwrap()));
}

#[tokio::test]
async fn test_bash_name() {
    let tool = BashTool::new();
    assert_eq!(tool.name(), "bash");
}

// --- GlobTool tests ---

#[tokio::test]
async fn test_glob_find_files() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("a.txt"), "").unwrap();
    fs::write(tmp.path().join("b.txt"), "").unwrap();
    fs::write(tmp.path().join("c.rs"), "").unwrap();

    let tool = GlobTool::new();
    let args = json!({
        "pattern": "*.txt",
        "path": tmp.path().to_str().unwrap()
    });
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("a.txt"));
    assert!(result.contains("b.txt"));
    assert!(!result.contains("c.rs"));
}

#[tokio::test]
async fn test_glob_recursive() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("subdir")).unwrap();
    fs::write(tmp.path().join("subdir").join("deep.rs"), "").unwrap();
    fs::write(tmp.path().join("top.rs"), "").unwrap();

    let tool = GlobTool::new();
    let args = json!({
        "pattern": "**/*.rs",
        "path": tmp.path().to_str().unwrap()
    });
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("deep.rs"));
    assert!(result.contains("top.rs"));
}

#[tokio::test]
async fn test_glob_no_matches() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("a.txt"), "").unwrap();

    let tool = GlobTool::new();
    let args = json!({
        "pattern": "*.nonexistent",
        "path": tmp.path().to_str().unwrap()
    });
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("No files") || result.contains("0"));
}

#[tokio::test]
async fn test_glob_name() {
    let tool = GlobTool::new();
    assert_eq!(tool.name(), "glob");
}

// --- GrepTool tests ---

#[tokio::test]
async fn test_grep_basic() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("file1.txt"), "hello world\nfoo bar").unwrap();
    fs::write(tmp.path().join("file2.txt"), "nothing relevant").unwrap();

    let tool = GrepTool::new();
    let args = json!({
        "pattern": "hello",
        "path": tmp.path().to_str().unwrap()
    });
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("file1.txt"));
    assert!(!result.contains("file2.txt"));
}

#[tokio::test]
async fn test_grep_regex() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("file.txt"), "function foo()\nvar bar = 1").unwrap();

    let tool = GrepTool::new();
    let args = json!({
        "pattern": "function\\s+\\w+",
        "path": tmp.path().to_str().unwrap()
    });
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("function foo"));
}

#[tokio::test]
async fn test_grep_no_matches() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("file.txt"), "hello world").unwrap();

    let tool = GrepTool::new();
    let args = json!({
        "pattern": "nonexistent_text",
        "path": tmp.path().to_str().unwrap()
    });
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("No matches") || result.contains("0"));
}

#[tokio::test]
async fn test_grep_name() {
    let tool = GrepTool::new();
    assert_eq!(tool.name(), "grep");
}
