use mcp_rlsp::analyzer::*;
use std::time::Duration;

/// Spawns rust-analyzer on /tmp/ra-deps — requires that project to exist.
#[tokio::test]
#[ignore]
async fn rust_client_on_ra_deps() {
    // Point RA at /tmp/ra-deps (simple project with deps that Python test confirmed works)
    std::env::set_current_dir("/tmp/ra-deps").unwrap();

    let mut client = RustAnalyzerClient::new();
    eprintln!("Starting RA on /tmp/ra-deps...");
    client.start().await.expect("RA should start");
    eprintln!("start() completed");

    // didOpen main.rs
    let path = "/tmp/ra-deps/src/main.rs";
    eprintln!("did_open {path}...");
    client.did_open(path).await.unwrap();
    eprintln!("did_open done, waiting 5s for analysis...");
    tokio::time::sleep(Duration::from_secs(2)).await;

    // workspace/symbol "Foo"
    let result = client.workspace_symbols("Foo").await;
    eprintln!("workspace_symbols('Foo') -> {:?}", result);
    assert!(result.is_ok(), "workspace_symbols should succeed");
    let sym_str = result.unwrap();
    assert!(!sym_str.contains("No symbols found") && !sym_str.contains("null"),
        "Expected Foo symbol, got: {sym_str}");

    // documentSymbol via find_definition at Foo::new() call (line 10 char 13)
    let def_result = client.find_definition("/tmp/ra-deps/src/main.rs", 10, 13).await;
    eprintln!("find_definition(L10:C13) -> {:?}", def_result);
}
