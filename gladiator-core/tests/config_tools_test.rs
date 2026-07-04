use gladiator_core::*;

#[test]
fn config_tools_toggle_defaults_all_on() {
    let config = Config::default();
    assert!(config.tools.bash);
    assert!(config.tools.read);
    assert!(config.tools.write);
    assert!(config.tools.edit);
    assert!(config.tools.glob);
    assert!(config.tools.grep);
    assert!(config.tools.fixme);
}

#[test]
fn config_mcp_servers_from_toml() {
    let toml_str = r#"
[tools]
bash = true
read = false
write = true
edit = false
glob = true
grep = true

[mcp_servers.read-file]
command = ["../mcp-read-file/target/release/mcp-read-file"]
default = true

[mcp_servers.run-command]
command = ["../mcp-run-command/target/release/mcp-run-command"]
default = false
expose = ["run_command"]
"#;
    let config = Config::from_str(toml_str).unwrap();
    assert!(config.tools.bash);
    assert!(!config.tools.read);
    assert!(config.tools.write);
    assert!(!config.tools.edit);

    assert_eq!(config.mcp_servers.len(), 2);
    assert!(config.mcp_servers.contains_key("read-file"));
    assert!(config.mcp_servers.contains_key("run-command"));

    let read_file = config.mcp_servers.get("read-file").unwrap();
    assert!(read_file.default);
    assert!(read_file.expose.is_empty());

    let run_cmd = config.mcp_servers.get("run-command").unwrap();
    assert!(!run_cmd.default);
    assert_eq!(run_cmd.expose, vec!["run_command".to_string()]);
}

#[test]
fn config_tools_roundtrip() {
    let mut config = Config::default();
    config.tools.read = false;
    config.tools.edit = false;
    let toml_str = config.to_toml().unwrap();
    let parsed = Config::from_str(&toml_str).unwrap();
    assert!(!parsed.tools.read);
    assert!(!parsed.tools.edit);
    assert!(parsed.tools.bash);
}
