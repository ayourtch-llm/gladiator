use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tracing::info;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    #[serde(default = "default_topics")]
    pub topics: TopicsConfig,
    #[serde(default = "default_server")]
    pub server: ServerConfig,
    #[serde(default = "default_ui")]
    pub ui: UiConfig,
    #[serde(default = "default_llm")]
    pub llm: LlmConfig,
    #[serde(default = "default_agent")]
    pub agent: AgentConfig,
    #[serde(default = "default_tools")]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            topics: TopicsConfig::default(),
            server: ServerConfig::default(),
            ui: UiConfig::default(),
            llm: LlmConfig::default(),
            agent: AgentConfig::default(),
            tools: ToolsConfig::default(),
            mcp_servers: HashMap::new(),
        }
    }
}

fn default_topics() -> TopicsConfig {
    TopicsConfig::default()
}
fn default_server() -> ServerConfig {
    ServerConfig::default()
}
fn default_ui() -> UiConfig {
    UiConfig::default()
}
fn default_llm() -> LlmConfig {
    LlmConfig::default()
}
fn default_agent() -> AgentConfig {
    AgentConfig::default()
}
fn default_tools() -> ToolsConfig {
    ToolsConfig::default()
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct TopicsConfig {
    #[serde(default = "default_log_topic")]
    pub log: String,
    #[serde(default = "default_input_topic")]
    pub input: String,
    #[serde(default = "default_agent_in_topic")]
    pub agent_in: String,
    #[serde(default = "default_agent_out_topic")]
    pub agent_out: String,
    #[serde(default = "default_agent_stream_topic")]
    pub agent_stream: String,
    #[serde(default = "default_llm_in_topic")]
    pub llm_in: String,
    #[serde(default = "default_llm_out_topic")]
    pub llm_out: String,
    #[serde(default = "default_llm_stream_topic")]
    pub llm_stream: String,
    #[serde(default = "default_llm_stats_topic")]
    pub llm_stats: String,
    #[serde(default = "default_llm_tool_calls_topic")]
    pub llm_tool_calls: String,
    #[serde(default = "default_tool_results_topic")]
    pub tool_results: String,
    #[serde(default = "default_user_control_topic")]
    pub user_control: String,
    #[serde(default = "default_ui_user_topic")]
    pub ui_user: String,
    #[serde(default = "default_ui_input_topic")]
    pub ui_input: String,
}

impl Default for TopicsConfig {
    fn default() -> Self {
        Self {
            log: default_log_topic(),
            input: default_input_topic(),
            agent_in: default_agent_in_topic(),
            agent_out: default_agent_out_topic(),
            agent_stream: default_agent_stream_topic(),
            llm_in: default_llm_in_topic(),
            llm_out: default_llm_out_topic(),
            llm_stream: default_llm_stream_topic(),
            llm_stats: default_llm_stats_topic(),
            llm_tool_calls: default_llm_tool_calls_topic(),
            tool_results: default_tool_results_topic(),
            user_control: default_user_control_topic(),
            ui_user: default_ui_user_topic(),
            ui_input: default_ui_input_topic(),
        }
    }
}

fn default_log_topic() -> String {
    "gladiator:log".to_string()
}
fn default_input_topic() -> String {
    "gladiator:input".to_string()
}
fn default_agent_in_topic() -> String {
    "agent:in".to_string()
}
fn default_agent_out_topic() -> String {
    "agent:out".to_string()
}
fn default_agent_stream_topic() -> String {
    "agent:stream".to_string()
}
fn default_llm_in_topic() -> String {
    "llm:in".to_string()
}
fn default_llm_out_topic() -> String {
    "llm:out".to_string()
}
fn default_llm_stream_topic() -> String {
    "llm:stream".to_string()
}
fn default_llm_stats_topic() -> String {
    "llm:stats".to_string()
}
fn default_llm_tool_calls_topic() -> String {
    "llm:tool_calls".to_string()
}
fn default_tool_results_topic() -> String {
    "tool:results".to_string()
}
fn default_user_control_topic() -> String {
    "user:control".to_string()
}
fn default_ui_user_topic() -> String {
    "ui:user".to_string()
}
fn default_ui_input_topic() -> String {
    "ui:input".to_string()
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
        }
    }
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}
fn default_port() -> u16 {
    3000
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct UiConfig {
    #[serde(default = "default_true")]
    pub show_bottom_panel: bool,
    #[serde(default = "default_theme")]
    pub theme: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            show_bottom_panel: default_true(),
            theme: default_theme(),
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_theme() -> String {
    "dark".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LlmConfig {
    #[serde(default = "default_llm_model")]
    pub model: String,
    #[serde(default = "default_llm_base_url")]
    pub base_url: String,
    #[serde(default = "default_llm_api_key")]
    pub api_key: String,
    #[serde(default = "default_llm_temperature")]
    pub temperature: f32,
    #[serde(default = "default_llm_max_tokens")]
    pub max_tokens: i32,
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
    #[serde(default = "default_stream_timeout_secs")]
    pub stream_timeout_secs: u64,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_retry_base_delay_ms")]
    pub retry_base_delay_ms: u64,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            model: default_llm_model(),
            base_url: default_llm_base_url(),
            api_key: default_llm_api_key(),
            temperature: default_llm_temperature(),
            max_tokens: default_llm_max_tokens(),
            request_timeout_secs: default_request_timeout_secs(),
            stream_timeout_secs: default_stream_timeout_secs(),
            max_retries: default_max_retries(),
            retry_base_delay_ms: default_retry_base_delay_ms(),
        }
    }
}

fn default_llm_model() -> String {
    "gpt-4o-mini".to_string()
}
fn default_llm_base_url() -> String {
    "http://ts-agent-gateway:4000/v1".to_string()
}
fn default_llm_api_key() -> String {
    String::new()
}
fn default_llm_temperature() -> f32 {
    0.7
}
fn default_llm_max_tokens() -> i32 {
    65536
}
fn default_request_timeout_secs() -> u64 {
    120
}
fn default_stream_timeout_secs() -> u64 {
    300
}
fn default_max_retries() -> u32 {
    3
}
fn default_retry_base_delay_ms() -> u64 {
    500
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentConfig {
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    #[serde(default = "default_system_message")]
    pub system_message: String,
    #[serde(default = "default_working_dir")]
    pub working_dir: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: default_max_iterations(),
            system_message: default_system_message(),
            working_dir: default_working_dir(),
        }
    }
}

fn default_max_iterations() -> u32 {
    50
}
fn default_system_message() -> String {
    "You are gladiator, an autonomous coding agent. You write code, run tests, and iterate until tests pass. Use the tools available to you to explore the codebase, make edits, run builds, and verify your work.".to_string()
}
fn default_working_dir() -> String {
    ".".to_string()
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ToolsConfig {
    #[serde(default = "default_tool_on")]
    pub bash: bool,
    #[serde(default = "default_tool_on")]
    pub read: bool,
    #[serde(default = "default_tool_on")]
    pub write: bool,
    #[serde(default = "default_tool_on")]
    pub edit: bool,
    #[serde(default = "default_tool_on")]
    pub glob: bool,
    #[serde(default = "default_tool_on")]
    pub grep: bool,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            bash: true,
            read: true,
            write: true,
            edit: true,
            glob: true,
            grep: true,
        }
    }
}

fn default_tool_on() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpServerConfig {
    pub command: Vec<String>,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub expose: Vec<String>,
}

impl Config {
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::ReadFailed(path.display().to_string(), e))?;
        let config: Config = toml::from_str(&content).map_err(ConfigError::ParseFailed)?;
        info!("Loaded config from: {}", path.display());
        Ok(config)
    }

    pub fn from_str(content: &str) -> Result<Self, ConfigError> {
        let config: Config = toml::from_str(content).map_err(ConfigError::ParseFailed)?;
        Ok(config)
    }

    pub fn to_toml(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(self).map_err(|e| ConfigError::SerializeFailed(e.to_string()))
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ConfigError {
    #[error("Failed to read config file '{0}': {1}")]
    ReadFailed(String, std::io::Error),
    #[error("Failed to parse config: {0}")]
    ParseFailed(#[from] toml::de::Error),
    #[error("Failed to serialize config: {0}")]
    SerializeFailed(String),
}
