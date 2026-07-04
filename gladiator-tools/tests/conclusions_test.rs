use gladiator_tools::conclusions::{
    ConclusionEntry, ConclusionStore, GetConclusionsTool, RecordConclusionTool,
};
use gladiator_tools::Tool;
use serde_json::json;
use std::fs;
use tempfile::TempDir;

// --- ConclusionStore tests ---

#[test]
fn conclusion_store_load_empty_when_no_file() {
    let tmp = TempDir::new().unwrap();
    let store = ConclusionStore::new(tmp.path().to_str().unwrap());
    let conclusions = store.load().unwrap();
    assert!(conclusions.is_empty());
}

#[test]
fn conclusion_store_add_and_load() {
    let tmp = TempDir::new().unwrap();
    let store = ConclusionStore::new(tmp.path().to_str().unwrap());

    let entry = store.add("root cause is dropped content").unwrap();
    assert_eq!(entry.text, "root cause is dropped content");
    assert!(!entry.id.is_empty());

    let conclusions = store.load().unwrap();
    assert_eq!(conclusions.len(), 1);
    assert_eq!(conclusions[0].text, "root cause is dropped content");
}

#[test]
fn conclusion_store_add_multiple_preserves_order() {
    let tmp = TempDir::new().unwrap();
    let store = ConclusionStore::new(tmp.path().to_str().unwrap());

    store.add("first decision").unwrap();
    store.add("second decision").unwrap();
    store.add("third decision").unwrap();

    let conclusions = store.load().unwrap();
    assert_eq!(conclusions.len(), 3);
    assert_eq!(conclusions[0].text, "first decision");
    assert_eq!(conclusions[1].text, "second decision");
    assert_eq!(conclusions[2].text, "third decision");
}

#[test]
fn conclusion_store_creates_parent_dir() {
    let tmp = TempDir::new().unwrap();
    let nested = tmp.path().join("nested");
    let store = ConclusionStore::new(nested.to_str().unwrap());

    let entry = store.add("test").unwrap();
    assert!(store.path().exists());
    assert_eq!(entry.text, "test");
}

#[test]
fn conclusion_store_atomic_save_uses_tmp_file() {
    let tmp = TempDir::new().unwrap();
    let store = ConclusionStore::new(tmp.path().to_str().unwrap());

    store.add("test").unwrap();

    let tmp_file = tmp.path().join("conclusions.json.tmp");
    assert!(!tmp_file.exists(), "temp file should be renamed, not left behind");
}

#[test]
fn conclusion_entry_serialization() {
    let entry = ConclusionEntry {
        id: "test-id".to_string(),
        text: "a decision".to_string(),
    };
    let json = serde_json::to_string(&entry).unwrap();
    let deserialized: ConclusionEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.id, "test-id");
    assert_eq!(deserialized.text, "a decision");
}

#[test]
fn conclusion_store_load_existing_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("conclusions.json");
    let entries = vec![
        ConclusionEntry {
            id: "id1".to_string(),
            text: "first".to_string(),
        },
        ConclusionEntry {
            id: "id2".to_string(),
            text: "second".to_string(),
        },
    ];
    fs::write(&path, serde_json::to_string_pretty(&entries).unwrap()).unwrap();

    let store = ConclusionStore::new(tmp.path().to_str().unwrap());
    let loaded = store.load().unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].id, "id1");
    assert_eq!(loaded[1].text, "second");
}

// --- Tool tests ---

#[tokio::test]
async fn test_record_conclusion_tool_persists() {
    let tmp = TempDir::new().unwrap();
    let tool = RecordConclusionTool::with_working_dir(tmp.path().to_str().unwrap());
    let args = json!({"text": "fix belongs in add_tool_calls"});
    let result = tool.execute(&args).await.unwrap();
    assert!(result.contains("recorded"));

    let store = ConclusionStore::new(tmp.path().to_str().unwrap());
    let all = store.get_all().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].text, "fix belongs in add_tool_calls");
}

#[tokio::test]
async fn test_record_conclusion_missing_text() {
    let tmp = TempDir::new().unwrap();
    let tool = RecordConclusionTool::with_working_dir(tmp.path().to_str().unwrap());
    let err = tool.execute(&json!({})).await.unwrap_err();
    assert!(err.contains("text"));
}

#[tokio::test]
async fn test_record_conclusion_empty_text_rejected() {
    let tmp = TempDir::new().unwrap();
    let tool = RecordConclusionTool::with_working_dir(tmp.path().to_str().unwrap());
    let err = tool.execute(&json!({"text": "   "})).await.unwrap_err();
    assert!(err.contains("empty"));
}

#[tokio::test]
async fn test_get_conclusions_tool() {
    let tmp = TempDir::new().unwrap();
    let store = ConclusionStore::new(tmp.path().to_str().unwrap());
    store.add("decision one").unwrap();
    store.add("decision two").unwrap();

    let tool = GetConclusionsTool::with_working_dir(tmp.path().to_str().unwrap());
    let result = tool.execute(&json!({})).await.unwrap();
    assert!(result.contains("decision one"));
    assert!(result.contains("decision two"));
}

#[tokio::test]
async fn test_get_conclusions_empty() {
    let tmp = TempDir::new().unwrap();
    let tool = GetConclusionsTool::with_working_dir(tmp.path().to_str().unwrap());
    let result = tool.execute(&json!({})).await.unwrap();
    assert!(result.contains("No conclusions"));
}

#[tokio::test]
async fn test_conclusion_tool_names() {
    let tmp = TempDir::new().unwrap();
    let record = RecordConclusionTool::with_working_dir(tmp.path().to_str().unwrap());
    let get = GetConclusionsTool::with_working_dir(tmp.path().to_str().unwrap());
    assert_eq!(record.name(), "record_conclusion");
    assert_eq!(get.name(), "get_conclusions");
}
