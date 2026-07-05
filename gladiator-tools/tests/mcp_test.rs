use gladiator_tools::mcp::McpServerRunner;
use gladiator_core::config::McpServerConfig;
use std::collections::HashMap;
use std::path::PathBuf;

/// Locate the mcp-random binary built by this workspace.
///
/// Integration-test binaries live under `<workspace>/target/<profile>/deps/`,
/// so walking up from `current_exe` finds a sibling `mcp-random` in
/// `<workspace>/target/<profile>/`.
fn mcp_random_binary() -> PathBuf {
    let exe = std::env::current_exe().expect("cannot get current exe path");
    for dir in exe.ancestors() {
        let candidate = dir.join("mcp-random");
        if candidate.is_file() {
            return candidate;
        }
    }

    // Fallback: derive from CARGO_MANIFEST_DIR of gladiator-tools.
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    PathBuf::from(manifest_dir)
        .parent()
        .unwrap_or(&PathBuf::from("."))
        .join("target")
        .join(if cfg!(debug_assertions) { "debug" } else { "release" })
        .join("mcp-random")
}

fn make_config() -> McpServerConfig {
    let bin = mcp_random_binary();
    assert!(
        bin.exists(),
        "mcp-random binary not found at {:?}. Run `cargo build -p mcp-random` first.",
        bin
    );
    McpServerConfig {
        command: vec![bin.to_string_lossy().to_string()],
        default: true,
        expose: vec![],
        env: HashMap::new(),
    }
}

#[tokio::test]
async fn test_mcp_tool_call() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let runner = McpServerRunner::new("test".to_string(), make_config());
    let handle = runner.spawn().await?;

    println!("Tools discovered: {:?}", handle.tool_names());

    let tools = handle.tool_actors();
    if let Some(tool) = tools.iter().find(|t| t.name() == "random_integer") {
        println!("Calling random_integer...");
        let result = tool.execute(&serde_json::json!({})).await;
        assert!(result.is_ok(), "tool call failed: {:?}", result.err());
    } else {
        panic!("random_integer tool not found");
    }

    handle.shutdown();
    Ok(())
}

#[tokio::test]
async fn test_mcp_tool_discovery() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let runner = McpServerRunner::new("test".to_string(), make_config());
    let handle = runner.spawn().await?;

    assert_eq!(handle.tool_names().len(), 7, "expected 7 tools");

    for name in &[
        "random_integer",
        "random_float",
        "random_string",
        "random_uuid",
        "random_choice",
        "random_bytes",
        "random_sample",
    ] {
        assert!(
            handle.tool_names().iter().any(|n| n == *name),
            "missing tool: {}",
            name
        );
    }

    handle.shutdown();
    Ok(())
}

#[tokio::test]
async fn test_mcp_tool_random_uuid() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let runner = McpServerRunner::new("test".to_string(), make_config());
    let handle = runner.spawn().await?;

    let tools = handle.tool_actors();
    if let Some(tool) = tools.iter().find(|t| t.name() == "random_uuid") {
        let result = tool.execute(&serde_json::json!({})).await;
        assert!(result.is_ok(), "tool call failed: {:?}", result.err());
        let uuid_str = result.unwrap();
        // UUID v4 format: 8-4-4-4-12 hex chars (36 total with dashes)
        assert_eq!(
            uuid_str.len(),
            36,
            "expected 36-char UUID, got {:?}",
            uuid_str
        );
    } else {
        panic!("random_uuid tool not found");
    }

    handle.shutdown();
    Ok(())
}

#[tokio::test]
async fn test_mcp_tool_random_string_params() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let runner = McpServerRunner::new("test".to_string(), make_config());
    let handle = runner.spawn().await?;

    let tools = handle.tool_actors();
    if let Some(tool) = tools.iter().find(|t| t.name() == "random_string") {
        let result = tool
            .execute(&serde_json::json!({
                "length": 8,
                "charset": "hex",
                "count": 3,
            }))
            .await;
        assert!(result.is_ok(), "tool call failed: {:?}", result.err());
    } else {
        panic!("random_string tool not found");
    }

    handle.shutdown();
    Ok(())
}

#[tokio::test]
async fn test_mcp_expose_filter() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let bin = mcp_random_binary();
    // With default=false and expose=["random_integer"], only random_integer
    // should be exposed via tool_actors().
    let config = McpServerConfig {
        command: vec![bin.to_string_lossy().to_string()],
        default: false,
        expose: vec!["random_integer".to_string()],
        env: HashMap::new(),
    };

    let runner = McpServerRunner::new("test".to_string(), config);
    let handle = runner.spawn().await?;

    // tool_names() returns ALL tools discovered, but tool_actors()
    // filters by default/expose.
    assert_eq!(handle.tool_names().len(), 7, "discovery should see all 7");
    let actors = handle.tool_actors();
    assert_eq!(
        actors.len(),
        1,
        "expose filter should yield exactly 1 tool"
    );
    assert_eq!(actors[0].name(), "random_integer");

    handle.shutdown();
    Ok(())
}

#[tokio::test]
async fn test_mcp_tool_error_handling() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let runner = McpServerRunner::new("test".to_string(), make_config());
    let handle = runner.spawn().await?;

    // Call random_choice with empty items — should return an error.
    let tools = handle.tool_actors();
    if let Some(tool) = tools.iter().find(|t| t.name() == "random_choice") {
        let result = tool
            .execute(&serde_json::json!({
                "items": [],
            }))
            .await;
        assert!(
            result.is_err(),
            "expected error for empty items, got: {:?}",
            result.ok()
        );
    } else {
        panic!("random_choice tool not found");
    }

    handle.shutdown();
    Ok(())
}

#[tokio::test]
async fn test_mcp_tool_parameters_json() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let runner = McpServerRunner::new("test".to_string(), make_config());
    let handle = runner.spawn().await?;

    // Verify that the tool parameters() JSON schema is valid and has expected
    // structure for random_integer.
    let tools = handle.tool_actors();
    if let Some(tool) = tools.iter().find(|t| t.name() == "random_integer") {
        let params = tool.parameters();
        assert!(params.is_object(), "parameters should be a JSON object");
        println!(
            "random_integer parameters: {}",
            serde_json::to_string_pretty(&params).unwrap()
        );
    } else {
        panic!("random_integer tool not found");
    }

    handle.shutdown();
    Ok(())
}
