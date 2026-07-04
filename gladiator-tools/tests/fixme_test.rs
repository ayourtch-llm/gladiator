use gladiator_tools::fixme::{
    CreateFixmeTool, FixmeEntry, FixmeStore, GetAllFixmesTool, GetOpenFixmesTool, MarkFixmeDoneTool,
};
use gladiator_tools::Tool;
use serde_json::json;
use std::fs;
use tempfile::TempDir;

// --- FixmeStore tests ---

#[test]
fn fixme_store_load_empty_when_no_file() {
    let tmp = TempDir::new().unwrap();
    let store = FixmeStore::new(tmp.path().to_str().unwrap());
    let fixmes = store.load().unwrap();
    assert!(fixmes.is_empty());
}

#[test]
fn fixme_store_add_and_load() {
    let tmp = TempDir::new().unwrap();
    let store = FixmeStore::new(tmp.path().to_str().unwrap());

    let entry = store.add("fix the bug").unwrap();
    assert_eq!(entry.phrase, "fix the bug");
    assert!(!entry.done);
    assert!(!entry.id.is_empty());

    let fixmes = store.load().unwrap();
    assert_eq!(fixmes.len(), 1);
    assert_eq!(fixmes[0].phrase, "fix the bug");
    assert!(!fixmes[0].done);
}

#[test]
fn fixme_store_add_multiple() {
    let tmp = TempDir::new().unwrap();
    let store = FixmeStore::new(tmp.path().to_str().unwrap());

    store.add("first issue").unwrap();
    store.add("second issue").unwrap();
    store.add("third issue").unwrap();

    let fixmes = store.load().unwrap();
    assert_eq!(fixmes.len(), 3);
    assert_eq!(fixmes[0].phrase, "first issue");
    assert_eq!(fixmes[1].phrase, "second issue");
    assert_eq!(fixmes[2].phrase, "third issue");
}

#[test]
fn fixme_store_mark_done() {
    let tmp = TempDir::new().unwrap();
    let store = FixmeStore::new(tmp.path().to_str().unwrap());

    let entry = store.add("fix me").unwrap();
    store.mark_done(&entry.id, true).unwrap();

    let fixmes = store.load().unwrap();
    assert_eq!(fixmes.len(), 1);
    assert!(fixmes[0].done);
}

#[test]
fn fixme_store_mark_undone() {
    let tmp = TempDir::new().unwrap();
    let store = FixmeStore::new(tmp.path().to_str().unwrap());

    let entry = store.add("fix me").unwrap();
    store.mark_done(&entry.id, true).unwrap();
    store.mark_done(&entry.id, false).unwrap();

    let fixmes = store.load().unwrap();
    assert!(!fixmes[0].done);
}

#[test]
fn fixme_store_mark_done_not_found() {
    let tmp = TempDir::new().unwrap();
    let store = FixmeStore::new(tmp.path().to_str().unwrap());

    let err = store.mark_done("nonexistent", true).unwrap_err();
    assert!(err.contains("not found"));
}

#[test]
fn fixme_store_get_all() {
    let tmp = TempDir::new().unwrap();
    let store = FixmeStore::new(tmp.path().to_str().unwrap());

    let e1 = store.add("first").unwrap();
    store.add("second").unwrap();
    store.mark_done(&e1.id, true).unwrap();

    let all = store.get_all().unwrap();
    assert_eq!(all.len(), 2);
    assert!(all[0].done);
    assert!(!all[1].done);
}

#[test]
fn fixme_store_get_open() {
    let tmp = TempDir::new().unwrap();
    let store = FixmeStore::new(tmp.path().to_str().unwrap());

    let e1 = store.add("first").unwrap();
    store.add("second").unwrap();
    store.add("third").unwrap();
    store.mark_done(&e1.id, true).unwrap();

    let open = store.get_open().unwrap();
    assert_eq!(open.len(), 2);
    assert!(!open[0].done);
    assert!(!open[1].done);
}

#[test]
fn fixme_store_creates_parent_dir() {
    let tmp = TempDir::new().unwrap();
    let nested = tmp.path().join("nested");
    let store = FixmeStore::new(nested.to_str().unwrap());

    let entry = store.add("test").unwrap();
    assert!(store.path().exists());
    assert_eq!(entry.phrase, "test");
}

#[test]
fn fixme_store_atomic_save_uses_tmp_file() {
    let tmp = TempDir::new().unwrap();
    let store = FixmeStore::new(tmp.path().to_str().unwrap());

    store.add("test").unwrap();

    let tmp_file = tmp.path().join("fixme.json.tmp");
    assert!(!tmp_file.exists(), "temp file should be renamed, not left behind");
}

#[test]
fn fixme_entry_serialization() {
    let entry = FixmeEntry {
        id: "test-id".to_string(),
        phrase: "test phrase".to_string(),
        done: false,
    };
    let json = serde_json::to_string(&entry).unwrap();
    let deserialized: FixmeEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.id, "test-id");
    assert_eq!(deserialized.phrase, "test phrase");
    assert!(!deserialized.done);
}

#[test]
fn fixme_store_load_existing_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("fixme.json");
    let entries = vec![
        FixmeEntry {
            id: "id1".to_string(),
            phrase: "first".to_string(),
            done: false,
        },
        FixmeEntry {
            id: "id2".to_string(),
            phrase: "second".to_string(),
            done: true,
        },
    ];
    fs::write(&path, serde_json::to_string_pretty(&entries).unwrap()).unwrap();

    let store = FixmeStore::new(tmp.path().to_str().unwrap());
    let loaded = store.load().unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].id, "id1");
    assert_eq!(loaded[1].phrase, "second");
    assert!(loaded[1].done);
}

// --- Tool tests ---

#[tokio::test]
async fn test_get_all_fixmes_tool() {
    let tmp = TempDir::new().unwrap();
    let store = FixmeStore::new(tmp.path().to_str().unwrap());
    store.add("fix the bug").unwrap();
    store.add("add tests").unwrap();

    let tool = GetAllFixmesTool::with_working_dir(tmp.path().to_str().unwrap());
    let args = json!({});
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("fix the bug"));
    assert!(result.contains("add tests"));
}

#[tokio::test]
async fn test_get_all_fixmes_empty() {
    let tmp = TempDir::new().unwrap();
    let tool = GetAllFixmesTool::with_working_dir(tmp.path().to_str().unwrap());
    let args = json!({});
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("No fixmes found"));
}

#[tokio::test]
async fn test_get_open_fixmes_tool() {
    let tmp = TempDir::new().unwrap();
    let store = FixmeStore::new(tmp.path().to_str().unwrap());
    let e1 = store.add("first").unwrap();
    store.add("second").unwrap();
    store.mark_done(&e1.id, true).unwrap();

    let tool = GetOpenFixmesTool::with_working_dir(tmp.path().to_str().unwrap());
    let args = json!({});
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("second"));
    assert!(!result.contains("\"first\""));
}

#[tokio::test]
async fn test_mark_fixme_done_tool() {
    let tmp = TempDir::new().unwrap();
    let store = FixmeStore::new(tmp.path().to_str().unwrap());
    let entry = store.add("fix me").unwrap();

    let tool = MarkFixmeDoneTool::with_working_dir(tmp.path().to_str().unwrap());
    let args = json!({"id": entry.id, "done": true});
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("done"));

    // Verify it was saved
    let all = store.get_all().unwrap();
    assert!(all[0].done);
}

#[tokio::test]
async fn test_mark_fixme_done_tool_not_found() {
    let tmp = TempDir::new().unwrap();
    let tool = MarkFixmeDoneTool::with_working_dir(tmp.path().to_str().unwrap());
    let args = json!({"id": "nonexistent", "done": true});
    let err = tool.execute(&args).await.unwrap_err();
    assert!(err.contains("not found"));
}

#[tokio::test]
async fn test_mark_fixme_undone_tool() {
    let tmp = TempDir::new().unwrap();
    let store = FixmeStore::new(tmp.path().to_str().unwrap());
    let entry = store.add("fix me").unwrap();
    store.mark_done(&entry.id, true).unwrap();

    let tool = MarkFixmeDoneTool::with_working_dir(tmp.path().to_str().unwrap());
    let args = json!({"id": entry.id, "done": false});
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("not done"));

    let all = store.get_all().unwrap();
    assert!(!all[0].done);
}

#[tokio::test]
async fn test_fixme_tool_names() {
    let tmp = TempDir::new().unwrap();
    let all_tool = GetAllFixmesTool::with_working_dir(tmp.path().to_str().unwrap());
    let open_tool = GetOpenFixmesTool::with_working_dir(tmp.path().to_str().unwrap());
    let mark_tool = MarkFixmeDoneTool::with_working_dir(tmp.path().to_str().unwrap());

    assert_eq!(all_tool.name(), "get_all_fixmes");
    assert_eq!(open_tool.name(), "get_open_fixmes");
    assert_eq!(mark_tool.name(), "mark_fixme_done");
}

#[tokio::test]
async fn test_create_fixme_tool() {
    let tmp = TempDir::new().unwrap();
    let tool = CreateFixmeTool::with_working_dir(tmp.path().to_str().unwrap());
    let args = json!({"phrase": "fix the bug"});
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("fix the bug"));
    assert!(result.contains("\"done\": false"));

    // Verify it was saved
    let store = FixmeStore::new(tmp.path().to_str().unwrap());
    let all = store.get_all().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].phrase, "fix the bug");
    assert!(!all[0].done);
}

#[tokio::test]
async fn test_create_fixme_tool_missing_phrase() {
    let tmp = TempDir::new().unwrap();
    let tool = CreateFixmeTool::with_working_dir(tmp.path().to_str().unwrap());
    let args = json!({});
    let err = tool.execute(&args).await.unwrap_err();
    assert!(err.contains("Missing 'phrase'"));
}

#[tokio::test]
async fn test_create_fixme_tool_empty_phrase() {
    let tmp = TempDir::new().unwrap();
    let tool = CreateFixmeTool::with_working_dir(tmp.path().to_str().unwrap());
    let args = json!({"phrase": "   "});
    let err = tool.execute(&args).await.unwrap_err();
    assert!(err.contains("empty"));
}

#[tokio::test]
async fn test_create_fixme_tool_name() {
    let tmp = TempDir::new().unwrap();
    let create_tool = CreateFixmeTool::with_working_dir(tmp.path().to_str().unwrap());
    assert_eq!(create_tool.name(), "create_fixme");
}
