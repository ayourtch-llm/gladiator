use gladiator_agent::AgentActor;
use gladiator_core::config::Config;
use gladiator_core::{Bus, Message};
use gladiator_llm::LlmActor;
use gladiator_server::run_server;
use gladiator_tools::{ToolActorRunner, ToolRegistry};
use std::path::PathBuf;
use clap::Parser;

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
}

async fn spawn_mcp_servers(
    _bus: &Bus,
    config: &Config,
    registry: &mut ToolRegistry,
) -> Vec<gladiator_tools::McpServerHandle> {
    let mut handles = Vec::new();
    for (name, mcp_config) in &config.mcp_servers {
        if mcp_config.command.is_empty() {
            continue;
        }
        let runner = gladiator_tools::McpServerRunner::new(name.clone(), mcp_config.clone());
        match runner.spawn().await {
            Ok(handle) => {
                let tool_actors = handle.tool_actors();
                    for tool in tool_actors {
                        registry.add_arc(tool);
                    }
                    handles.push(handle);
            }
            Err(e) => {
                tracing::error!("Failed to spawn MCP server '{}': {}", name, e);
            }
        }
    }
    handles
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    let config = if let Some(path) = &cli.config {
        Config::from_file(path)?
    } else {
        Config::default()
    };

    let host = cli.host.clone();
    let port = cli.port;
    let no_tui = cli.no_tui;

    tracing::info!("Starting gladiator...");
    tracing::info!("LLM model: {}", config.llm.model);
    tracing::info!("LLM base_url: {}", config.llm.base_url);
    tracing::info!("Agent max_iterations: {}", config.agent.max_iterations);

    let bus = Bus::new();
    setup_topics(&bus, &config).await;

    // Build tool registry
    let mut registry = ToolRegistry::new();
    let _mcp_handles = spawn_mcp_servers(&bus, &config, &mut registry).await;
    tracing::info!("Tool registry: {} tools", registry.len());

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
    .with_tool_defs(registry.syntaxes().iter().map(|s| s.to_openai_json()).collect());

    let agent_handle = bus.spawn_actor(agent_actor).await?;

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
        subscriptions: vec![config.topics.agent_stream.clone()],
        publications: vec![config.topics.agent_in.clone()],
    })
    .await;

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
                let msg = Message::new(&agent_in_topic, "gladiator-tui", text)
                    .with_type("UserInput");
                if let Err(e) = bus_clone.publish("gladiator-tui", msg).await {
                    tracing::error!("Failed to publish user input: {}", e);
                }
            }
        });

        // Run TUI app
        match gladiator_tui::app::run_app(bus.clone(), user_input_tx).await {
            Ok(()) => {}
            Err(e) => tracing::error!("TUI error: {}", e),
        }
    }

    // Cleanup
    tracing::info!("Shutting down...");
    llm_handle.stop().await;
    agent_handle.stop().await;
    for handle in tool_runner_handles {
        handle.abort();
    }

    Ok(())
}
