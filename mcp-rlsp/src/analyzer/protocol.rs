use serde_json::{Value, json};

/// Build a TextDocumentIdentifier from a file path.
pub fn create_text_document_identifier(file_path: &str) -> Value {
    json!({
        "uri": uri_from_path(file_path)
    })
}

/// Convert a local filesystem path to an LSP file:// URI.
pub fn uri_from_path(file_path: &str) -> String {
    // If already a URI, return as-is
    if file_path.starts_with("file://") || file_path.starts_with("http://") {
        return file_path.to_string();
    }
    let abs = std::fs::canonicalize(file_path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| file_path.to_string());
    format!("file://{abs}")
}

/// Build a TextDocumentPositionParams: { textDocument, position }.
pub fn create_text_document_position_params(file_path: &str, line: u32, character: u32) -> Value {
    json!({
        "textDocument": create_text_document_identifier(file_path),
        "position": { "line": line, "character": character }
    })
}

/// Build references params with includeDeclaration context.
pub fn create_references_params(file_path: &str, line: u32, character: u32) -> Value {
    json!({
        "textDocument": create_text_document_identifier(file_path),
        "position": { "line": line, "character": character },
        "context": { "includeDeclaration": true }
    })
}

/// Build workspace/symbol params.
pub fn create_workspace_symbol_params(query: &str) -> Value {
    json!({ "query": query })
}

/// Build rename params with newName.
pub fn create_rename_params(file_path: &str, line: u32, character: u32, new_name: &str) -> Value {
    json!({
        "textDocument": create_text_document_identifier(file_path),
        "position": { "line": line, "character": character },
        "newName": new_name
    })
}

/// Build a Range value from LSP 0-based line/char positions.
pub fn create_range(
    start_line: u32,
    start_character: u32,
    end_line: u32,
    end_character: u32,
) -> Value {
    json!({
        "start": { "line": start_line, "character": start_character },
        "end":   { "line": end_line,   "character": end_character }
    })
}

/// Build textDocument/codeAction params for a range.
pub fn create_code_action_params(
    file_path: &str,
    range: Value,
    context_kinds: &[&str],
) -> Value {
    let only: Vec<Value> = context_kinds.iter().map(|k| json!(k)).collect();
    json!({
        "textDocument": create_text_document_identifier(file_path),
        "range": range,
        "context": { "only": only, "diagnostics": [] }
    })
}

/// Build textDocument/codeAction params at a single position (zero-length range).
pub fn create_code_action_position_params(
    file_path: &str,
    line: u32,
    character: u32,
) -> Value {
    let zero_range = json!({
        "start": { "line": line, "character": character },
        "end":   { "line": line, "character": character }
    });
    create_code_action_params(file_path, zero_range, &[])
}

/// Build textDocument/prepareTypeHierarchy params.
pub fn prepare_type_hierarchy_params(
    file_path: &str,
    line: u32,
    charactar: u32,
) -> Value {
    json!({
        "textDocument": create_text_document_identifier(file_path),
        "position": { "line": line, "character": charactar }
    })
}

/// Build typeHierarchy/supertypes params from a prepare item's id.
pub fn supertypes_params(item_id: &Value) -> Value {
    json!({ "item": item_id })
}
