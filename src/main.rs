use gladiator_agent::AgentActor;
use gladiator_core::config::Config;
use gladiator_core::{Bus, Message};
use gladiator_llm::LlmActor;
use gladiator_server::run_server;
use gladiator_tools::builtin::{BashTool, EditFileTool, GlobTool, GrepTool, ReadFileTool, WriteFileTool};
use gladiator_tools::conclusions::{GetConclusionsTool, RecordConclusionTool};
use gladiator_tools::fixme::{CreateFixmeTool, GetAllFixmesTool, GetOpenFixmesTool, MarkFixmeDoneTool};
use gladiator_tools::{ToolActorRunner, ToolRegistry, WebFetchTool};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use clap::Parser;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

mod debug_layer;
use debug_layer::ChatMakeWriter;

#[derive(Parser)]
struct Cli {
    /// Config file path
    #[arg(long, short)]
    config: Option<PathBuf>,
    /// Host for the HTTP debug server
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// Port for the HTTP debug server
    #[arg(long, default_value_t = 3000)]
    port: u16,
    /// Disable the TUI (run headless)
    #[arg(long)]
    no_tui: bool,
}

async fn setup_topics(bus: &Bus, config: &Config) {
    let topics = &config.topics;
    bus.create_topic(&topics.log, 1000).await;
    bus.create_topic(&topics.input, 1000).await;
    bus.create_topic(&topics.agent_in, 1000).await;
    bus.create_topic(&topics.agent_out, 1000).await;
    bus.create_topic(&topics.agent_stream, 1000).await;
    bus.create_topic(&topics.llm_in, 1000).await;
    bus.create_topic(&topics.llm_out, 1000).await;
    bus.create_topic(&topics.llm_stream, 1000).await;
    bus.create_topic(&topics.llm_stats, 1000).await;
    bus.create_topic(&topics.llm_tool_calls, 1000).await;
    bus.create_topic(&topics.tool_results, 1000).await;
    bus.create_topic(&topics.user_control, 1000).await;
    bus.create_topic(&topics.ui_user, 1000).await;
    bus.create_topic(&topics.ui_input, 1000).await;
    bus.create_topic(&topics.agent_state_control, 1000).await;
    bus.create_topic(&topics.agent_state, 1000).await;
    bus.create_topic(&topics.persistence_command, 1000).await;
    bus.create_topic(&topics.persistence_response, 1000).await;
}

async fn spawn_mcp_servers(
    _bus: &Bus,
    config: &Config,
    registry: &mut ToolRegistry,
) -> std::sync::Arc<gladiator_tools::McpManager> {
    let (manager, tools) = gladiator_tools::McpManager::spawn_all(&config.mcp_servers).await;
    for tool in tools {
        registry.add_arc(tool);
    }
    // Register admin tools so the agent can inspect/restart/disable MCP servers.
    let manager_arc = std::sync::Arc::new(manager);
    registry.add(Box::new(gladiator_tools::McpStatusTool::new(manager_arc.clone())));
    registry.add(Box::new(gladiator_tools::McpRestartTool::new(manager_arc.clone())));
    let ret = manager_arc.clone();
    registry.add(Box::new(gladiator_tools::McpDisableTool::new(manager_arc)));
    ret
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cli = Cli::parse();

    let mut config = if let Some(path) = &cli.config {
        Config::from_file(path)?
    } else if std::path::Path::new("gladiator.toml").exists() {
        Config::from_file(std::path::Path::new("gladiator.toml"))?
    } else {
        Config::default()
    };

    // If system_message starts with "@", read the real system message from that file.
    if config.agent.system_message.starts_with('@') {
        let filename = config.agent.system_message[1..].trim().to_string();
        match std::fs::read_to_string(&filename) {
            Ok(content) => {
                config.agent.system_message = content;
            }
            Err(e) => {
                tracing::warn!("Failed to read system message file '{}': {}", filename, e);
            }
        }
    }

    let host = cli.host.clone();
    let port = cli.port;
    let no_tui = cli.no_tui;

    // Debug flag: when enabled, tracing events are routed into the chat via the bus.
    let debug_flag = Arc::new(AtomicBool::new(false));
    let (log_tx, mut log_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    // In TUI mode, redirect tracing to a file so it doesn't corrupt the terminal.
    // In headless mode, use stderr as normal.
    // In both modes, add a chat layer that routes events to the bus when debug is on.
    //
    // The file/stderr layer uses EnvFilter (default: info) to limit log file verbosity.
    // The chat layer uses LevelFilter::TRACE so ALL events reach the ChatMakeWriter,
    // which gates output via the debug_flag. This way /debug can surface debug-level
    // events that the EnvFilter would otherwise filter out.
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".into());
    let chat_make_writer = ChatMakeWriter::new(debug_flag.clone(), log_tx);
    use tracing_subscriber::filter::LevelFilter;
    use tracing_subscriber::layer::SubscriberExt;
    if !no_tui {
        match std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open("gladiator.log")
        {
            Ok(log_file) => {
                let log_writer = std::sync::Arc::new(log_file);
                let fmt_layer = tracing_subscriber::fmt::layer()
                    .with_writer(log_writer)
                    .with_filter(env_filter);
                let chat_layer = tracing_subscriber::fmt::layer()
                    .with_writer(chat_make_writer)
                    .with_filter(LevelFilter::TRACE);
                tracing_subscriber::registry()
                    .with(fmt_layer)
                    .with(chat_layer)
                    .init();
            }
            Err(_) => {
                let chat_layer = tracing_subscriber::fmt::layer()
                    .with_writer(chat_make_writer)
                    .with_filter(LevelFilter::TRACE);
                tracing_subscriber::registry()
                    .with(chat_layer)
                    .init();
            }
        }
    } else {
        let stderr_layer = tracing_subscriber::fmt::layer()
            .with_filter(env_filter);
        let chat_layer = tracing_subscriber::fmt::layer()
            .with_writer(chat_make_writer)
            .with_filter(LevelFilter::TRACE);
        tracing_subscriber::registry()
            .with(stderr_layer)
            .with(chat_layer)
            .init();
    }

    tracing::info!("Starting gladiator...");
    tracing::info!("LLM model: {}", config.llm.model);
    tracing::info!("LLM base_url: {}", config.llm.base_url);
    tracing::info!("Agent max_iterations: {}", config.agent.max_iterations);

    // Discover the model's context window when not supplied via config. The
    // agent uses it to report "tokens remaining" alongside per-turn usage. If
    // the probe fails the metric is simply left unavailable.
    if config.llm.context_window.is_none() {
        match gladiator_llm::fetch_context_window(
            &config.llm.base_url,
            &config.llm.api_key,
            &config.llm.model,
        )
        .await
        {
            Some(window) => {
                tracing::info!(
                    "LLM context_window probed: {} tokens (set [llm].context_window to override)",
                    window
                );
                config.llm.context_window = Some(window);
            }
            None => {
                tracing::warn!(
                    "LLM context_window unknown; context-remaining metric disabled. \
                     Set [llm].context_window in gladiator.toml to enable it."
                );
            }
        }
    } else {
        tracing::info!(
            "LLM context_window: {} tokens (from config)",
            config.llm.context_window.unwrap()
        );
    }

    let bus = Bus::new();
    setup_topics(&bus, &config).await;

    // Build tool registry — register built-in tools based on ToolsConfig toggles
    let mut registry = ToolRegistry::new();
    let working_dir = config.agent.working_dir.clone();

    // Shared runtime toggle for the bash sandbox. The TUI `/sandbox on|off`
    // command flips this atomically; BashTool reads it at execute time.
    let sandbox_toggle = Arc::new(AtomicBool::new(config.tools.sandbox.enabled));

    if config.tools.bash {
        registry.add(Box::new(BashTool::with_sandbox_toggle(
            &working_dir,
            config.tools.sandbox.clone(),
            sandbox_toggle.clone(),
        )));
    }
    if config.tools.read {
        registry.add(Box::new(ReadFileTool::with_working_dir(&working_dir)));
    }
    if config.tools.write {
        registry.add(Box::new(WriteFileTool::with_working_dir(&working_dir)));
    }
    if config.tools.edit {
        registry.add(Box::new(EditFileTool::with_working_dir(&working_dir)));
    }
    if config.tools.glob {
        registry.add(Box::new(GlobTool::with_working_dir(&working_dir)));
    }
    if config.tools.grep {
        registry.add(Box::new(GrepTool::with_working_dir(&working_dir)));
    }
    if config.tools.fixme {
        registry.add(Box::new(GetAllFixmesTool::with_working_dir(&working_dir)));
        registry.add(Box::new(GetOpenFixmesTool::with_working_dir(&working_dir)));
        registry.add(Box::new(MarkFixmeDoneTool::with_working_dir(&working_dir)));
        registry.add(Box::new(CreateFixmeTool::with_working_dir(&working_dir)));
    }
    if config.tools.conclusions {
        registry.add(Box::new(RecordConclusionTool::with_working_dir(&working_dir)));
        registry.add(Box::new(GetConclusionsTool::with_working_dir(&working_dir)));
    }
    if config.tools.web_fetch {
        registry.add(Box::new(WebFetchTool));
    }
    tracing::info!("Built-in tools registered: {} tools", registry.len());

    // Spawn MCP tool servers and add their tools to the registry
    let mcp_manager = spawn_mcp_servers(&bus, &config, &mut registry).await;
    tracing::info!("Tool registry (with MCP): {} tools", registry.len());

    // Create and spawn LLM actor
    let llm_actor = LlmActor::new(
        0,
        config.topics.llm_in.clone(),
        config.topics.llm_out.clone(),
        config.topics.llm_stream.clone(),
        config.topics.llm_stats.clone(),
        config.topics.llm_tool_calls.clone(),
        config.topics.user_control.clone(),
        config.llm.clone(),
    );
    let llm_handle = bus.spawn_actor(llm_actor).await?;

    // Create and spawn Agent actor
    let agent_actor = AgentActor::new(
        0,
        config.topics.agent_in.clone(),
        config.topics.llm_in.clone(),
        config.topics.llm_out.clone(),
        config.topics.llm_stream.clone(),
        config.topics.llm_tool_calls.clone(),
        config.topics.tool_results.clone(),
        config.topics.agent_stream.clone(),
        config.agent.clone(),
    )
    .with_tool_defs({
        let mut defs: Vec<serde_json::Value> =
            registry.syntaxes().iter().map(|s| s.to_openai_json()).collect();
        defs.extend(gladiator_agent::internal_tools::internal_tool_defs());
        defs
    })
    .with_state_topics(config.topics.agent_state_control.clone(), config.topics.agent_state.clone())
    .with_llm_stats_topic(config.topics.llm_stats.clone())
    .with_context_window(config.llm.context_window)
    .with_llm_config(config.llm.clone());

    let agent_handle = bus.spawn_actor(agent_actor).await?;

    // Create and spawn Persistence actor
    let persistence_actor = gladiator_agent::PersistenceActor::new(
        0,
        config.topics.persistence_command.clone(),
        config.topics.persistence_response.clone(),
        config.topics.agent_state_control.clone(),
        config.topics.agent_state.clone(),
    );
    let persistence_handle = bus.spawn_actor(persistence_actor).await?;

    // Spawn tool runners
    let mut tool_runner_handles = Vec::new();
    for tool in registry.iter() {
        let runner = ToolActorRunner::from_arc(tool.clone());
        let tool_name = tool.name().to_string();
        let bus_clone = bus.clone();
        let handle = tokio::spawn(async move {
            if let Err(e) = runner.run(&bus_clone).await {
                tracing::error!("Tool runner '{}' failed: {}", tool_name, e);
            }
        });
        tool_runner_handles.push(handle);
    }

    // Start HTTP debug server
    let server_bus = bus.clone();
    let server_host = host.clone();
    let server_port = port;
    tokio::spawn(async move {
        if let Err(e) = run_server(server_bus, server_host, server_port).await {
            tracing::error!("HTTP server error: {}", e);
        }
    });
    tracing::info!("HTTP debug server on http://{}:{}", host, port);

    // Register the agent input topic subscriber so the TUI can publish to it
    bus.register_announcement(gladiator_core::ActorAnnouncement {
        id: "gladiator-tui".to_string(),
        subscriptions: vec![config.topics.agent_stream.clone(), config.topics.persistence_response.clone(), config.topics.log.clone()],
        publications: vec![config.topics.agent_in.clone(), config.topics.user_control.clone(), config.topics.persistence_command.clone()],
    })
    .await;

    // Register the debug tracing publisher so it can publish log events to the bus
    bus.register_announcement(gladiator_core::ActorAnnouncement {
        id: "gladiator-debug".to_string(),
        subscriptions: vec![],
        publications: vec![config.topics.log.clone()],
    })
    .await;

    // Spawn background task to bridge tracing log lines → bus log topic
    {
        let log_bus = bus.clone();
        let log_topic = config.topics.log.clone();
        tokio::spawn(async move {
            while let Some(line) = log_rx.recv().await {
                let msg = Message::new(&log_topic, "gladiator-debug", line)
                    .with_type("Log");
                let _ = log_bus.publish("gladiator-debug", msg).await;
            }
        });
    }

    if no_tui {
        // Headless mode: just keep running
        tracing::info!("Running headless (no TUI). Press Ctrl+C to stop.");
        tokio::signal::ctrl_c().await?;
    } else {
        // Run TUI
        let (user_input_tx, mut user_input_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        // Bridge user input → agent:in
        let bus_clone = bus.clone();
        let agent_in_topic = config.topics.agent_in.clone();
        tokio::spawn(async move {
            while let Some(text) = user_input_rx.recv().await {
                tracing::info!(target: "gladiator", "User input published to {}: {}", agent_in_topic, text);
                let msg = Message::new(&agent_in_topic, "gladiator-tui", text)
                    .with_type("UserInput");
                if let Err(e) = bus_clone.publish("gladiator-tui", msg).await {
                    tracing::error!("Failed to publish user input: {}", e);
                }
            }
        });

        // Run TUI app
        match gladiator_tui::app::run_app(bus.clone(), user_input_tx, &config.topics, &config.agent.working_dir, debug_flag.clone(), sandbox_toggle.clone(), Some(mcp_manager.clone())).await {
            Ok(()) => {}
            Err(e) => tracing::error!("TUI error: {}", e),
        }
    }

    // Cleanup
    tracing::info!("Shutting down...");
    llm_handle.stop().await;
    agent_handle.stop().await;
    persistence_handle.stop().await;
    for handle in tool_runner_handles {
        handle.abort();
    }

    Ok(())
}
