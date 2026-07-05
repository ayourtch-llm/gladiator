use std::collections::HashSet;

use anyhow::Result;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use clap::Parser;
use rand::Rng;
use rand_distr::Distribution;
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::*,
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::Deserialize;
use tracing_subscriber::EnvFilter;

// ── CLI ───────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "mcp-random", about = "MCP server for generating random values")]
struct Cli {
    /// Disable specific tools by name (can be repeated).
    /// Available tools: random_integer, random_float, random_string,
    /// random_uuid, random_choice, random_bytes, random_sample
    #[arg(long = "disable", value_name = "TOOL")]
    disabled_tools: Vec<String>,
}

// ── Helpers ───────────────────────────────────────────────────────

fn tool_error(msg: impl std::fmt::Display) -> CallToolResult {
    CallToolResult::error(vec![Content::text(msg.to_string())])
}

macro_rules! try_tool {
    ($expr:expr, $msg:literal) => {
        match $expr {
            Ok(v) => v,
            Err(e) => return Ok(tool_error(format!("{}: {}", $msg, e))),
        }
    };
}

// ── Constants ─────────────────────────────────────────────────────

const MAX_COUNT: u32 = 1000;
const MAX_STRING_LENGTH: u32 = 10000;
const MAX_BYTES: u32 = 10000;

const CHARSET_ALPHANUMERIC: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
const CHARSET_ALPHA: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
const CHARSET_LOWERCASE: &str = "abcdefghijklmnopqrstuvwxyz";
const CHARSET_UPPERCASE: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const CHARSET_DIGITS: &str = "0123456789";
const CHARSET_HEX: &str = "0123456789abcdef";

// ── Parameter structs ─────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RandomIntegerParams {
    /// Minimum value (inclusive). Default: 0
    #[serde(default)]
    min: Option<i64>,
    /// Maximum value (inclusive). Default: 100
    #[serde(default)]
    max: Option<i64>,
    /// Number of values to generate (1-1000). Default: 1
    #[serde(default)]
    count: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RandomFloatParams {
    /// Minimum value (inclusive). Default: 0.0. Used for "uniform" distribution.
    #[serde(default)]
    min: Option<f64>,
    /// Maximum value (exclusive). Default: 1.0. Used for "uniform" distribution.
    #[serde(default)]
    max: Option<f64>,
    /// Distribution: "uniform" (default), "normal".
    /// For "normal": uses mean and std_dev parameters.
    #[serde(default)]
    distribution: Option<String>,
    /// Mean for normal distribution. Default: 0.0
    #[serde(default)]
    mean: Option<f64>,
    /// Standard deviation for normal distribution. Default: 1.0
    #[serde(default)]
    std_dev: Option<f64>,
    /// Number of values to generate (1-1000). Default: 1
    #[serde(default)]
    count: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RandomStringParams {
    /// Length of the string. Default: 16
    #[serde(default)]
    length: Option<u32>,
    /// Character set: "alphanumeric" (default), "alpha", "lowercase",
    /// "uppercase", "digits", "hex", or a custom string of characters to pick from.
    #[serde(default)]
    charset: Option<String>,
    /// Number of strings to generate (1-1000). Default: 1
    #[serde(default)]
    count: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RandomUuidParams {
    /// Number of UUIDs to generate (1-1000). Default: 1
    #[serde(default)]
    count: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RandomChoiceParams {
    /// List of items to choose from.
    items: Vec<String>,
    /// Number of items to pick (1-1000). Default: 1.
    /// Items may repeat (sampling with replacement).
    #[serde(default)]
    count: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RandomBytesParams {
    /// Number of random bytes to generate. Default: 16
    #[serde(default)]
    num_bytes: Option<u32>,
    /// Output encoding: "hex" (default) or "base64".
    #[serde(default)]
    encoding: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RandomSampleParams {
    /// Distribution to sample from: "normal", "exponential", "bernoulli",
    /// "poisson", "binomial", "log_normal".
    distribution: String,
    /// Mean (for normal, log_normal). Default: 0.0
    #[serde(default)]
    mean: Option<f64>,
    /// Standard deviation (for normal, log_normal). Default: 1.0
    #[serde(default)]
    std_dev: Option<f64>,
    /// Rate parameter lambda (for exponential, poisson). Default: 1.0
    #[serde(default)]
    lambda: Option<f64>,
    /// Probability of success (for bernoulli, binomial). Default: 0.5
    #[serde(default)]
    p: Option<f64>,
    /// Number of trials (for binomial). Default: 10
    #[serde(default)]
    n: Option<u64>,
    /// Number of samples to generate (1-1000). Default: 1
    #[serde(default)]
    count: Option<u32>,
}

// ── Core generation functions ─────────────────────────────────────

fn validate_count(count: Option<u32>) -> std::result::Result<u32, String> {
    let c = count.unwrap_or(1);
    if c == 0 {
        return Err("count must be at least 1".to_string());
    }
    if c > MAX_COUNT {
        return Err(format!("count must be at most {}", MAX_COUNT));
    }
    Ok(c)
}

fn generate_integer(rng: &mut impl Rng, min: i64, max: i64) -> std::result::Result<i64, String> {
    if min > max {
        return Err(format!("min ({}) must be <= max ({})", min, max));
    }
    Ok(rng.random_range(min..=max))
}

fn generate_float(rng: &mut impl Rng, min: f64, max: f64) -> std::result::Result<f64, String> {
    if min >= max {
        return Err(format!("min ({}) must be < max ({})", min, max));
    }
    if !min.is_finite() || !max.is_finite() {
        return Err("min and max must be finite".to_string());
    }
    Ok(rng.random_range(min..max))
}

fn generate_normal(
    rng: &mut impl Rng,
    mean: f64,
    std_dev: f64,
) -> std::result::Result<f64, String> {
    if !mean.is_finite() {
        return Err("mean must be finite".to_string());
    }
    if !std_dev.is_finite() || std_dev < 0.0 {
        return Err("std_dev must be finite and non-negative".to_string());
    }
    let dist = rand_distr::Normal::new(mean, std_dev)
        .map_err(|e| format!("invalid normal parameters: {}", e))?;
    Ok(dist.sample(rng))
}

fn generate_sample(
    rng: &mut impl Rng,
    distribution: &str,
    mean: f64,
    std_dev: f64,
    lambda: f64,
    p: f64,
    n: u64,
) -> std::result::Result<String, String> {
    match distribution {
        "normal" => {
            let v = generate_normal(rng, mean, std_dev)?;
            Ok(format!("{}", v))
        }
        "exponential" => {
            if !lambda.is_finite() || lambda <= 0.0 {
                return Err("lambda must be finite and positive".to_string());
            }
            let dist = rand_distr::Exp::new(lambda)
                .map_err(|e| format!("invalid exponential parameters: {}", e))?;
            Ok(format!("{}", dist.sample(rng)))
        }
        "bernoulli" => {
            if !(0.0..=1.0).contains(&p) {
                return Err("p must be between 0.0 and 1.0".to_string());
            }
            let dist = rand_distr::Bernoulli::new(p)
                .map_err(|e| format!("invalid bernoulli parameters: {}", e))?;
            Ok(format!("{}", dist.sample(rng)))
        }
        "poisson" => {
            if !lambda.is_finite() || lambda <= 0.0 {
                return Err("lambda must be finite and positive".to_string());
            }
            let dist = rand_distr::Poisson::new(lambda)
                .map_err(|e| format!("invalid poisson parameters: {}", e))?;
            let v: f64 = dist.sample(rng);
            Ok(format!("{}", v as u64))
        }
        "binomial" => {
            if !(0.0..=1.0).contains(&p) {
                return Err("p must be between 0.0 and 1.0".to_string());
            }
            let dist = rand_distr::Binomial::new(n, p)
                .map_err(|e| format!("invalid binomial parameters: {}", e))?;
            Ok(format!("{}", dist.sample(rng)))
        }
        "log_normal" => {
            if !mean.is_finite() {
                return Err("mean (mu) must be finite".to_string());
            }
            if !std_dev.is_finite() || std_dev < 0.0 {
                return Err("std_dev (sigma) must be finite and non-negative".to_string());
            }
            let dist = rand_distr::LogNormal::new(mean, std_dev)
                .map_err(|e| format!("invalid log_normal parameters: {}", e))?;
            Ok(format!("{}", dist.sample(rng)))
        }
        other => Err(format!(
            "unknown distribution '{}'. Supported: normal, exponential, bernoulli, poisson, binomial, log_normal",
            other
        )),
    }
}

fn resolve_charset(charset: Option<&str>) -> std::result::Result<Vec<char>, String> {
    let chars: Vec<char> = match charset.unwrap_or("alphanumeric") {
        "alphanumeric" => CHARSET_ALPHANUMERIC.chars().collect(),
        "alpha" => CHARSET_ALPHA.chars().collect(),
        "lowercase" => CHARSET_LOWERCASE.chars().collect(),
        "uppercase" => CHARSET_UPPERCASE.chars().collect(),
        "digits" => CHARSET_DIGITS.chars().collect(),
        "hex" => CHARSET_HEX.chars().collect(),
        custom => custom.chars().collect(),
    };
    if chars.is_empty() {
        return Err("charset must not be empty".to_string());
    }
    Ok(chars)
}

fn generate_string(
    rng: &mut impl Rng,
    length: u32,
    charset: &[char],
) -> std::result::Result<String, String> {
    if length > MAX_STRING_LENGTH {
        return Err(format!("length must be at most {}", MAX_STRING_LENGTH));
    }
    if charset.is_empty() {
        return Err("charset must not be empty".to_string());
    }
    let s: String = (0..length)
        .map(|_| charset[rng.random_range(0..charset.len())])
        .collect();
    Ok(s)
}

fn generate_choice<'a>(
    rng: &mut impl Rng,
    items: &'a [String],
) -> std::result::Result<&'a str, String> {
    if items.is_empty() {
        return Err("items must not be empty".to_string());
    }
    let idx = rng.random_range(0..items.len());
    Ok(&items[idx])
}

fn generate_bytes(
    rng: &mut impl Rng,
    num_bytes: u32,
    encoding: &str,
) -> std::result::Result<String, String> {
    if num_bytes > MAX_BYTES {
        return Err(format!("num_bytes must be at most {}", MAX_BYTES));
    }
    if num_bytes == 0 {
        return Err("num_bytes must be at least 1".to_string());
    }
    let mut buf = vec![0u8; num_bytes as usize];
    rng.fill(&mut buf[..]);
    match encoding {
        "hex" => Ok(hex_encode(&buf)),
        "base64" => Ok(BASE64.encode(&buf)),
        other => Err(format!("unknown encoding '{}', use 'hex' or 'base64'", other)),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Format results: single value as plain text, multiple as JSON array.
fn format_results(values: &[String]) -> String {
    if values.len() == 1 {
        values[0].clone()
    } else {
        serde_json::to_string_pretty(values).unwrap_or_else(|_| values.join("\n"))
    }
}

// ── Server ────────────────────────────────────────────────────────

#[derive(Clone)]
struct RandomServer {
    tool_router: ToolRouter<Self>,
}

impl RandomServer {
    fn new(disabled: &HashSet<String>) -> Self {
        let mut router = Self::tool_router();
        for name in disabled {
            router.remove_route(name);
        }
        Self {
            tool_router: router,
        }
    }
}

// ── Tool implementations ──────────────────────────────────────────

#[tool_router]
impl RandomServer {
    /// Generate random integers within a range.
    #[tool(
        description = "Generate one or more random integers. Parameters: min (default 0), max (default 100), count (default 1). Returns a single number or a JSON array."
    )]
    async fn random_integer(
        &self,
        Parameters(params): Parameters<RandomIntegerParams>,
    ) -> Result<CallToolResult, McpError> {
        let min = params.min.unwrap_or(0);
        let max = params.max.unwrap_or(100);
        let count = try_tool!(validate_count(params.count), "Invalid count");

        let mut rng = rand::rng();
        let mut values = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let v = try_tool!(generate_integer(&mut rng, min, max), "Generation failed");
            values.push(v.to_string());
        }

        Ok(CallToolResult::success(vec![Content::text(format_results(
            &values,
        ))]))
    }

    /// Generate random floating-point numbers, optionally from a distribution.
    #[tool(
        description = "Generate one or more random floats. For uniform distribution (default): min (default 0.0), max (default 1.0). For normal distribution: set distribution=\"normal\", mean (default 0.0), std_dev (default 1.0). count (default 1)."
    )]
    async fn random_float(
        &self,
        Parameters(params): Parameters<RandomFloatParams>,
    ) -> Result<CallToolResult, McpError> {
        let count = try_tool!(validate_count(params.count), "Invalid count");
        let dist = params.distribution.as_deref().unwrap_or("uniform");

        let mut rng = rand::rng();
        let mut values = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let v = match dist {
                "uniform" => {
                    let min = params.min.unwrap_or(0.0);
                    let max = params.max.unwrap_or(1.0);
                    try_tool!(generate_float(&mut rng, min, max), "Generation failed")
                }
                "normal" => {
                    let mean = params.mean.unwrap_or(0.0);
                    let std_dev = params.std_dev.unwrap_or(1.0);
                    try_tool!(generate_normal(&mut rng, mean, std_dev), "Generation failed")
                }
                other => {
                    return Ok(tool_error(format!(
                        "Unknown distribution '{}'. Use 'uniform' or 'normal'.",
                        other
                    )));
                }
            };
            values.push(format!("{}", v));
        }

        Ok(CallToolResult::success(vec![Content::text(format_results(
            &values,
        ))]))
    }

    /// Generate random strings from a configurable character set.
    #[tool(
        description = "Generate one or more random strings. Parameters: length (default 16), charset (\"alphanumeric\", \"alpha\", \"lowercase\", \"uppercase\", \"digits\", \"hex\", or a custom string of characters; default \"alphanumeric\"), count (default 1)."
    )]
    async fn random_string(
        &self,
        Parameters(params): Parameters<RandomStringParams>,
    ) -> Result<CallToolResult, McpError> {
        let length = params.length.unwrap_or(16);
        let charset = try_tool!(
            resolve_charset(params.charset.as_deref()),
            "Invalid charset"
        );
        let count = try_tool!(validate_count(params.count), "Invalid count");

        let mut rng = rand::rng();
        let mut values = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let v = try_tool!(
                generate_string(&mut rng, length, &charset),
                "Generation failed"
            );
            values.push(v);
        }

        Ok(CallToolResult::success(vec![Content::text(format_results(
            &values,
        ))]))
    }

    /// Generate random v4 UUIDs.
    #[tool(description = "Generate one or more random v4 UUIDs. Parameters: count (default 1).")]
    async fn random_uuid(
        &self,
        Parameters(params): Parameters<RandomUuidParams>,
    ) -> Result<CallToolResult, McpError> {
        let count = try_tool!(validate_count(params.count), "Invalid count");

        let values: Vec<String> = (0..count).map(|_| uuid::Uuid::new_v4().to_string()).collect();

        Ok(CallToolResult::success(vec![Content::text(format_results(
            &values,
        ))]))
    }

    /// Pick random items from a list (with replacement).
    #[tool(
        description = "Pick one or more random items from a list (with replacement). Parameters: items (array of strings, required), count (default 1)."
    )]
    async fn random_choice(
        &self,
        Parameters(params): Parameters<RandomChoiceParams>,
    ) -> Result<CallToolResult, McpError> {
        let count = try_tool!(validate_count(params.count), "Invalid count");

        let mut rng = rand::rng();
        let mut values = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let v = try_tool!(
                generate_choice(&mut rng, &params.items),
                "Generation failed"
            );
            values.push(v.to_string());
        }

        Ok(CallToolResult::success(vec![Content::text(format_results(
            &values,
        ))]))
    }

    /// Generate random bytes encoded as hex or base64.
    #[tool(
        description = "Generate random bytes. Parameters: num_bytes (default 16), encoding (\"hex\" or \"base64\", default \"hex\")."
    )]
    async fn random_bytes(
        &self,
        Parameters(params): Parameters<RandomBytesParams>,
    ) -> Result<CallToolResult, McpError> {
        let num_bytes = params.num_bytes.unwrap_or(16);
        let encoding = params.encoding.as_deref().unwrap_or("hex");

        let mut rng = rand::rng();
        let result = try_tool!(
            generate_bytes(&mut rng, num_bytes, encoding),
            "Generation failed"
        );

        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    /// Sample from a named probability distribution.
    #[tool(
        description = "Sample from a probability distribution. Required: distribution (\"normal\", \"exponential\", \"bernoulli\", \"poisson\", \"binomial\", \"log_normal\"). Parameters vary by distribution: normal/log_normal use mean (default 0.0) and std_dev (default 1.0); exponential/poisson use lambda (default 1.0); bernoulli uses p (default 0.5); binomial uses n (default 10) and p (default 0.5). count (default 1)."
    )]
    async fn random_sample(
        &self,
        Parameters(params): Parameters<RandomSampleParams>,
    ) -> Result<CallToolResult, McpError> {
        let count = try_tool!(validate_count(params.count), "Invalid count");

        let mean = params.mean.unwrap_or(0.0);
        let std_dev = params.std_dev.unwrap_or(1.0);
        let lambda = params.lambda.unwrap_or(1.0);
        let p = params.p.unwrap_or(0.5);
        let n = params.n.unwrap_or(10);

        let mut rng = rand::rng();
        let mut values = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let v = try_tool!(
                generate_sample(&mut rng, &params.distribution, mean, std_dev, lambda, p, n),
                "Sampling failed"
            );
            values.push(v);
        }

        Ok(CallToolResult::success(vec![Content::text(format_results(
            &values,
        ))]))
    }
}

// ── ServerHandler ─────────────────────────────────────────────────

#[tool_handler]
impl ServerHandler for RandomServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::default(),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(
                "MCP server for generating random values: integers, floats, strings, UUIDs, choices, bytes, and distribution samples."
                    .to_string(),
            ),
        }
    }
}

// ── Main ──────────────────────────────────────────────────────────

#[cfg(not(tarpaulin_include))]
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let disabled: HashSet<String> = cli.disabled_tools.into_iter().collect();
    if !disabled.is_empty() {
        tracing::info!("Disabled tools: {:?}", disabled);
    }

    tracing::info!("Starting mcp-random server");

    let server = RandomServer::new(&disabled);
    let service = server.serve(stdio()).await?;
    service.waiting().await?;

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn seeded_rng() -> StdRng {
        StdRng::seed_from_u64(42)
    }

    fn extract_text(result: &CallToolResult) -> String {
        match &result.content[0].raw {
            RawContent::Text(t) => t.text.clone(),
            _ => panic!("expected text content"),
        }
    }

    // ── validate_count ────────────────────────────────────────────

    #[test]
    fn count_none_defaults_to_1() {
        assert_eq!(validate_count(None).unwrap(), 1);
    }

    #[test]
    fn count_zero_is_error() {
        assert!(validate_count(Some(0)).unwrap_err().contains("at least 1"));
    }

    #[test]
    fn count_over_max_is_error() {
        assert!(validate_count(Some(MAX_COUNT + 1))
            .unwrap_err()
            .contains("at most"));
    }

    #[test]
    fn count_at_max_is_ok() {
        assert_eq!(validate_count(Some(MAX_COUNT)).unwrap(), MAX_COUNT);
    }

    // ── generate_integer ──────────────────────────────────────────

    #[test]
    fn integer_within_range() {
        let mut rng = seeded_rng();
        for _ in 0..100 {
            let v = generate_integer(&mut rng, 10, 20).unwrap();
            assert!((10..=20).contains(&v));
        }
    }

    #[test]
    fn integer_min_equals_max() {
        let mut rng = seeded_rng();
        assert_eq!(generate_integer(&mut rng, 5, 5).unwrap(), 5);
    }

    #[test]
    fn integer_min_greater_than_max() {
        let mut rng = seeded_rng();
        assert!(generate_integer(&mut rng, 10, 5).unwrap_err().contains("min"));
    }

    #[test]
    fn integer_negative_range() {
        let mut rng = seeded_rng();
        for _ in 0..100 {
            let v = generate_integer(&mut rng, -100, -50).unwrap();
            assert!((-100..=-50).contains(&v));
        }
    }

    // ── generate_float ────────────────────────────────────────────

    #[test]
    fn float_within_range() {
        let mut rng = seeded_rng();
        for _ in 0..100 {
            let v = generate_float(&mut rng, 1.0, 2.0).unwrap();
            assert!((1.0..2.0).contains(&v));
        }
    }

    #[test]
    fn float_min_equals_max_is_error() {
        let mut rng = seeded_rng();
        assert!(generate_float(&mut rng, 1.0, 1.0).unwrap_err().contains("min"));
    }

    #[test]
    fn float_min_greater_than_max() {
        let mut rng = seeded_rng();
        assert!(generate_float(&mut rng, 2.0, 1.0).is_err());
    }

    #[test]
    fn float_infinity_is_error() {
        let mut rng = seeded_rng();
        assert!(generate_float(&mut rng, 0.0, f64::INFINITY).unwrap_err().contains("finite"));
    }

    #[test]
    fn float_nan_is_error() {
        let mut rng = seeded_rng();
        assert!(generate_float(&mut rng, f64::NAN, 1.0).unwrap_err().contains("finite"));
    }

    // ── resolve_charset ───────────────────────────────────────────

    #[test]
    fn charset_default_is_alphanumeric() {
        let chars = resolve_charset(None).unwrap();
        assert_eq!(chars.len(), 62); // 26+26+10
    }

    #[test]
    fn charset_named_presets() {
        assert_eq!(resolve_charset(Some("alpha")).unwrap().len(), 52);
        assert_eq!(resolve_charset(Some("lowercase")).unwrap().len(), 26);
        assert_eq!(resolve_charset(Some("uppercase")).unwrap().len(), 26);
        assert_eq!(resolve_charset(Some("digits")).unwrap().len(), 10);
        assert_eq!(resolve_charset(Some("hex")).unwrap().len(), 16);
    }

    #[test]
    fn charset_custom() {
        let chars = resolve_charset(Some("abc")).unwrap();
        assert_eq!(chars, vec!['a', 'b', 'c']);
    }

    #[test]
    fn charset_empty_is_error() {
        assert!(resolve_charset(Some("")).unwrap_err().contains("empty"));
    }

    // ── generate_string ───────────────────────────────────────────

    #[test]
    fn string_correct_length() {
        let mut rng = seeded_rng();
        let charset: Vec<char> = "abc".chars().collect();
        let s = generate_string(&mut rng, 20, &charset).unwrap();
        assert_eq!(s.len(), 20);
    }

    #[test]
    fn string_uses_only_charset_chars() {
        let mut rng = seeded_rng();
        let charset: Vec<char> = "xy".chars().collect();
        let s = generate_string(&mut rng, 100, &charset).unwrap();
        assert!(s.chars().all(|c| c == 'x' || c == 'y'));
    }

    #[test]
    fn string_zero_length() {
        let mut rng = seeded_rng();
        let charset: Vec<char> = "a".chars().collect();
        let s = generate_string(&mut rng, 0, &charset).unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn string_exceeds_max_length() {
        let mut rng = seeded_rng();
        let charset: Vec<char> = "a".chars().collect();
        assert!(generate_string(&mut rng, MAX_STRING_LENGTH + 1, &charset)
            .unwrap_err()
            .contains("at most"));
    }

    #[test]
    fn string_empty_charset_is_error() {
        let mut rng = seeded_rng();
        assert!(generate_string(&mut rng, 5, &[]).unwrap_err().contains("empty"));
    }

    // ── generate_choice ───────────────────────────────────────────

    #[test]
    fn choice_returns_item_from_list() {
        let mut rng = seeded_rng();
        let items: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        for _ in 0..100 {
            let v = generate_choice(&mut rng, &items).unwrap();
            assert!(["a", "b", "c"].contains(&v));
        }
    }

    #[test]
    fn choice_single_item() {
        let mut rng = seeded_rng();
        let items: Vec<String> = vec!["only".into()];
        assert_eq!(generate_choice(&mut rng, &items).unwrap(), "only");
    }

    #[test]
    fn choice_empty_items_is_error() {
        let mut rng = seeded_rng();
        let items: Vec<String> = vec![];
        assert!(generate_choice(&mut rng, &items).unwrap_err().contains("empty"));
    }

    // ── generate_bytes ────────────────────────────────────────────

    #[test]
    fn bytes_hex_correct_length() {
        let mut rng = seeded_rng();
        let s = generate_bytes(&mut rng, 16, "hex").unwrap();
        assert_eq!(s.len(), 32); // 16 bytes = 32 hex chars
    }

    #[test]
    fn bytes_hex_valid_chars() {
        let mut rng = seeded_rng();
        let s = generate_bytes(&mut rng, 8, "hex").unwrap();
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn bytes_base64_decodable() {
        let mut rng = seeded_rng();
        let s = generate_bytes(&mut rng, 16, "base64").unwrap();
        assert!(BASE64.decode(&s).is_ok());
        assert_eq!(BASE64.decode(&s).unwrap().len(), 16);
    }

    #[test]
    fn bytes_unknown_encoding_is_error() {
        let mut rng = seeded_rng();
        assert!(generate_bytes(&mut rng, 8, "utf8")
            .unwrap_err()
            .contains("unknown encoding"));
    }

    #[test]
    fn bytes_zero_is_error() {
        let mut rng = seeded_rng();
        assert!(generate_bytes(&mut rng, 0, "hex")
            .unwrap_err()
            .contains("at least 1"));
    }

    #[test]
    fn bytes_exceeds_max_is_error() {
        let mut rng = seeded_rng();
        assert!(generate_bytes(&mut rng, MAX_BYTES + 1, "hex")
            .unwrap_err()
            .contains("at most"));
    }

    // ── hex_encode ────────────────────────────────────────────────

    #[test]
    fn hex_encode_known_values() {
        assert_eq!(hex_encode(&[0x00, 0xff, 0xab]), "00ffab");
        assert_eq!(hex_encode(&[]), "");
    }

    // ── format_results ────────────────────────────────────────────

    #[test]
    fn format_single_value() {
        assert_eq!(format_results(&["hello".to_string()]), "hello");
    }

    #[test]
    fn format_multiple_values_is_json() {
        let result = format_results(&["a".to_string(), "b".to_string()]);
        let parsed: Vec<String> = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed, vec!["a", "b"]);
    }

    // ── tool_error helper ─────────────────────────────────────────

    #[test]
    fn tool_error_creates_error_result() {
        let result = tool_error("bad");
        assert!(result.is_error.unwrap_or(false));
        assert!(extract_text(&result).contains("bad"));
    }

    // ── server construction & info ────────────────────────────────

    #[test]
    fn server_constructs_with_no_disabled() {
        let server = RandomServer::new(&HashSet::new());
        let tools = server.tool_router.list_all();
        assert_eq!(tools.len(), 7);
    }

    #[test]
    fn server_info_has_tools_capability() {
        let server = RandomServer::new(&HashSet::new());
        let info = server.get_info();
        assert!(info.capabilities.tools.is_some());
        assert!(info.instructions.unwrap().contains("random"));
    }

    // ── tool disabling ────────────────────────────────────────────

    #[test]
    fn disable_one_tool() {
        let disabled: HashSet<String> = ["random_uuid".to_string()].into();
        let server = RandomServer::new(&disabled);
        let tools = server.tool_router.list_all();
        assert_eq!(tools.len(), 6);
        assert!(!tools.iter().any(|t| t.name.as_ref() == "random_uuid"));
    }

    #[test]
    fn disable_multiple_tools() {
        let disabled: HashSet<String> =
            ["random_uuid".to_string(), "random_bytes".to_string(), "random_float".to_string()]
                .into();
        let server = RandomServer::new(&disabled);
        let tools = server.tool_router.list_all();
        assert_eq!(tools.len(), 4);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert!(names.contains(&"random_integer"));
        assert!(names.contains(&"random_string"));
        assert!(names.contains(&"random_choice"));
    }

    #[test]
    fn disable_nonexistent_tool_is_harmless() {
        let disabled: HashSet<String> = ["nonexistent_tool".to_string()].into();
        let server = RandomServer::new(&disabled);
        let tools = server.tool_router.list_all();
        assert_eq!(tools.len(), 7);
    }

    #[test]
    fn disable_all_tools() {
        let disabled: HashSet<String> = [
            "random_integer".to_string(),
            "random_float".to_string(),
            "random_string".to_string(),
            "random_uuid".to_string(),
            "random_choice".to_string(),
            "random_bytes".to_string(),
            "random_sample".to_string(),
        ]
        .into();
        let server = RandomServer::new(&disabled);
        let tools = server.tool_router.list_all();
        assert_eq!(tools.len(), 0);
    }

    // ── MCP tool: random_integer ──────────────────────────────────

    #[tokio::test]
    async fn tool_random_integer_defaults() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomIntegerParams {
            min: None,
            max: None,
            count: None,
        };
        let result = server.random_integer(Parameters(params)).await.unwrap();
        assert!(!result.is_error.unwrap_or(false));
        let text = extract_text(&result);
        let v: i64 = text.parse().unwrap();
        assert!((0..=100).contains(&v));
    }

    #[tokio::test]
    async fn tool_random_integer_custom_range() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomIntegerParams {
            min: Some(50),
            max: Some(50),
            count: None,
        };
        let result = server.random_integer(Parameters(params)).await.unwrap();
        assert_eq!(extract_text(&result), "50");
    }

    #[tokio::test]
    async fn tool_random_integer_multiple() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomIntegerParams {
            min: Some(1),
            max: Some(6),
            count: Some(5),
        };
        let result = server.random_integer(Parameters(params)).await.unwrap();
        let text = extract_text(&result);
        let values: Vec<String> = serde_json::from_str(&text).unwrap();
        assert_eq!(values.len(), 5);
        assert!(values.iter().all(|s| {
            let v: i64 = s.parse().unwrap();
            (1..=6).contains(&v)
        }));
    }

    #[tokio::test]
    async fn tool_random_integer_invalid_range() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomIntegerParams {
            min: Some(10),
            max: Some(5),
            count: None,
        };
        let result = server.random_integer(Parameters(params)).await.unwrap();
        assert!(result.is_error.unwrap_or(false));
    }

    // ── MCP tool: random_float ────────────────────────────────────

    #[tokio::test]
    async fn tool_random_float_defaults() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomFloatParams {
            min: None,
            max: None,
            distribution: None,
            mean: None,
            std_dev: None,
            count: None,
        };
        let result = server.random_float(Parameters(params)).await.unwrap();
        assert!(!result.is_error.unwrap_or(false));
        let v: f64 = extract_text(&result).parse().unwrap();
        assert!((0.0..1.0).contains(&v));
    }

    #[tokio::test]
    async fn tool_random_float_custom_range() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomFloatParams {
            min: Some(10.0),
            max: Some(20.0),
            distribution: None,
            mean: None,
            std_dev: None,
            count: None,
        };
        let result = server.random_float(Parameters(params)).await.unwrap();
        let v: f64 = extract_text(&result).parse().unwrap();
        assert!((10.0..20.0).contains(&v));
    }

    // ── MCP tool: random_string ───────────────────────────────────

    #[tokio::test]
    async fn tool_random_string_defaults() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomStringParams {
            length: None,
            charset: None,
            count: None,
        };
        let result = server.random_string(Parameters(params)).await.unwrap();
        assert!(!result.is_error.unwrap_or(false));
        let text = extract_text(&result);
        assert_eq!(text.len(), 16);
        assert!(text.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[tokio::test]
    async fn tool_random_string_hex() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomStringParams {
            length: Some(8),
            charset: Some("hex".to_string()),
            count: None,
        };
        let result = server.random_string(Parameters(params)).await.unwrap();
        let text = extract_text(&result);
        assert_eq!(text.len(), 8);
        assert!(text.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn tool_random_string_custom_charset() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomStringParams {
            length: Some(50),
            charset: Some("01".to_string()),
            count: None,
        };
        let result = server.random_string(Parameters(params)).await.unwrap();
        let text = extract_text(&result);
        assert_eq!(text.len(), 50);
        assert!(text.chars().all(|c| c == '0' || c == '1'));
    }

    #[tokio::test]
    async fn tool_random_string_multiple() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomStringParams {
            length: Some(4),
            charset: Some("digits".to_string()),
            count: Some(3),
        };
        let result = server.random_string(Parameters(params)).await.unwrap();
        let values: Vec<String> = serde_json::from_str(&extract_text(&result)).unwrap();
        assert_eq!(values.len(), 3);
        assert!(values.iter().all(|s| s.len() == 4));
    }

    // ── MCP tool: random_uuid ─────────────────────────────────────

    #[tokio::test]
    async fn tool_random_uuid_format() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomUuidParams { count: None };
        let result = server.random_uuid(Parameters(params)).await.unwrap();
        let text = extract_text(&result);
        assert!(uuid::Uuid::parse_str(&text).is_ok());
    }

    #[tokio::test]
    async fn tool_random_uuid_multiple() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomUuidParams { count: Some(3) };
        let result = server.random_uuid(Parameters(params)).await.unwrap();
        let values: Vec<String> = serde_json::from_str(&extract_text(&result)).unwrap();
        assert_eq!(values.len(), 3);
        // All should be valid UUIDs and unique
        let mut seen = HashSet::new();
        for v in &values {
            assert!(uuid::Uuid::parse_str(v).is_ok());
            assert!(seen.insert(v.clone()));
        }
    }

    // ── MCP tool: random_choice ───────────────────────────────────

    #[tokio::test]
    async fn tool_random_choice_single() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomChoiceParams {
            items: vec!["red".into(), "green".into(), "blue".into()],
            count: None,
        };
        let result = server.random_choice(Parameters(params)).await.unwrap();
        let text = extract_text(&result);
        assert!(["red", "green", "blue"].contains(&text.as_str()));
    }

    #[tokio::test]
    async fn tool_random_choice_multiple() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomChoiceParams {
            items: vec!["a".into(), "b".into()],
            count: Some(10),
        };
        let result = server.random_choice(Parameters(params)).await.unwrap();
        let values: Vec<String> = serde_json::from_str(&extract_text(&result)).unwrap();
        assert_eq!(values.len(), 10);
        assert!(values.iter().all(|v| v == "a" || v == "b"));
    }

    #[tokio::test]
    async fn tool_random_choice_empty_items() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomChoiceParams {
            items: vec![],
            count: None,
        };
        let result = server.random_choice(Parameters(params)).await.unwrap();
        assert!(result.is_error.unwrap_or(false));
    }

    // ── MCP tool: random_bytes ────────────────────────────────────

    #[tokio::test]
    async fn tool_random_bytes_hex_default() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomBytesParams {
            num_bytes: None,
            encoding: None,
        };
        let result = server.random_bytes(Parameters(params)).await.unwrap();
        let text = extract_text(&result);
        assert_eq!(text.len(), 32); // 16 bytes = 32 hex chars
        assert!(text.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn tool_random_bytes_base64() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomBytesParams {
            num_bytes: Some(32),
            encoding: Some("base64".to_string()),
        };
        let result = server.random_bytes(Parameters(params)).await.unwrap();
        let text = extract_text(&result);
        let decoded = BASE64.decode(&text).unwrap();
        assert_eq!(decoded.len(), 32);
    }

    #[tokio::test]
    async fn tool_random_bytes_invalid_encoding() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomBytesParams {
            num_bytes: Some(8),
            encoding: Some("utf8".to_string()),
        };
        let result = server.random_bytes(Parameters(params)).await.unwrap();
        assert!(result.is_error.unwrap_or(false));
    }

    // ── generate_normal ────────────────────────────────────────────

    #[test]
    fn normal_produces_finite_value() {
        let mut rng = seeded_rng();
        let v = generate_normal(&mut rng, 0.0, 1.0).unwrap();
        assert!(v.is_finite());
    }

    #[test]
    fn normal_zero_std_dev_returns_mean() {
        let mut rng = seeded_rng();
        let v = generate_normal(&mut rng, 42.0, 0.0).unwrap();
        assert!((v - 42.0).abs() < f64::EPSILON);
    }

    #[test]
    fn normal_negative_std_dev_is_error() {
        let mut rng = seeded_rng();
        assert!(generate_normal(&mut rng, 0.0, -1.0)
            .unwrap_err()
            .contains("non-negative"));
    }

    #[test]
    fn normal_infinite_mean_is_error() {
        let mut rng = seeded_rng();
        assert!(generate_normal(&mut rng, f64::INFINITY, 1.0)
            .unwrap_err()
            .contains("finite"));
    }

    // ── generate_sample ──────────────────────────────────────────

    #[test]
    fn sample_normal() {
        let mut rng = seeded_rng();
        let v = generate_sample(&mut rng, "normal", 10.0, 2.0, 1.0, 0.5, 10).unwrap();
        let f: f64 = v.parse().unwrap();
        assert!(f.is_finite());
    }

    #[test]
    fn sample_exponential() {
        let mut rng = seeded_rng();
        let v = generate_sample(&mut rng, "exponential", 0.0, 1.0, 2.0, 0.5, 10).unwrap();
        let f: f64 = v.parse().unwrap();
        assert!(f >= 0.0);
    }

    #[test]
    fn sample_exponential_zero_lambda_is_error() {
        let mut rng = seeded_rng();
        assert!(generate_sample(&mut rng, "exponential", 0.0, 1.0, 0.0, 0.5, 10)
            .unwrap_err()
            .contains("positive"));
    }

    #[test]
    fn sample_bernoulli() {
        let mut rng = seeded_rng();
        for _ in 0..100 {
            let v = generate_sample(&mut rng, "bernoulli", 0.0, 1.0, 1.0, 0.5, 10).unwrap();
            assert!(v == "true" || v == "false");
        }
    }

    #[test]
    fn sample_bernoulli_invalid_p() {
        let mut rng = seeded_rng();
        assert!(generate_sample(&mut rng, "bernoulli", 0.0, 1.0, 1.0, 1.5, 10).is_err());
        assert!(generate_sample(&mut rng, "bernoulli", 0.0, 1.0, 1.0, -0.1, 10).is_err());
    }

    #[test]
    fn sample_poisson() {
        let mut rng = seeded_rng();
        let v = generate_sample(&mut rng, "poisson", 0.0, 1.0, 5.0, 0.5, 10).unwrap();
        let n: u64 = v.parse().unwrap();
        // Poisson with lambda=5 should produce small non-negative integers
        assert!(n < 100);
    }

    #[test]
    fn sample_binomial() {
        let mut rng = seeded_rng();
        for _ in 0..100 {
            let v = generate_sample(&mut rng, "binomial", 0.0, 1.0, 1.0, 0.5, 20).unwrap();
            let n: u64 = v.parse().unwrap();
            assert!(n <= 20);
        }
    }

    #[test]
    fn sample_log_normal() {
        let mut rng = seeded_rng();
        let v = generate_sample(&mut rng, "log_normal", 0.0, 1.0, 1.0, 0.5, 10).unwrap();
        let f: f64 = v.parse().unwrap();
        assert!(f > 0.0);
    }

    #[test]
    fn sample_unknown_distribution() {
        let mut rng = seeded_rng();
        assert!(generate_sample(&mut rng, "cauchy", 0.0, 1.0, 1.0, 0.5, 10)
            .unwrap_err()
            .contains("unknown distribution"));
    }

    // ── MCP tool: random_float with distributions ────────────────

    #[tokio::test]
    async fn tool_random_float_normal() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomFloatParams {
            min: None,
            max: None,
            distribution: Some("normal".to_string()),
            mean: Some(100.0),
            std_dev: Some(10.0),
            count: None,
        };
        let result = server.random_float(Parameters(params)).await.unwrap();
        assert!(!result.is_error.unwrap_or(false));
        let v: f64 = extract_text(&result).parse().unwrap();
        assert!(v.is_finite());
    }

    #[tokio::test]
    async fn tool_random_float_normal_multiple() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomFloatParams {
            min: None,
            max: None,
            distribution: Some("normal".to_string()),
            mean: Some(0.0),
            std_dev: Some(1.0),
            count: Some(5),
        };
        let result = server.random_float(Parameters(params)).await.unwrap();
        let values: Vec<String> = serde_json::from_str(&extract_text(&result)).unwrap();
        assert_eq!(values.len(), 5);
        assert!(values.iter().all(|s| s.parse::<f64>().unwrap().is_finite()));
    }

    #[tokio::test]
    async fn tool_random_float_unknown_distribution() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomFloatParams {
            min: None,
            max: None,
            distribution: Some("gamma".to_string()),
            mean: None,
            std_dev: None,
            count: None,
        };
        let result = server.random_float(Parameters(params)).await.unwrap();
        assert!(result.is_error.unwrap_or(false));
    }

    // ── MCP tool: random_sample ──────────────────────────────────

    #[tokio::test]
    async fn tool_random_sample_normal() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomSampleParams {
            distribution: "normal".to_string(),
            mean: Some(50.0),
            std_dev: Some(10.0),
            lambda: None,
            p: None,
            n: None,
            count: None,
        };
        let result = server.random_sample(Parameters(params)).await.unwrap();
        assert!(!result.is_error.unwrap_or(false));
        let v: f64 = extract_text(&result).parse().unwrap();
        assert!(v.is_finite());
    }

    #[tokio::test]
    async fn tool_random_sample_exponential() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomSampleParams {
            distribution: "exponential".to_string(),
            mean: None,
            std_dev: None,
            lambda: Some(0.5),
            p: None,
            n: None,
            count: None,
        };
        let result = server.random_sample(Parameters(params)).await.unwrap();
        assert!(!result.is_error.unwrap_or(false));
        let v: f64 = extract_text(&result).parse().unwrap();
        assert!(v >= 0.0);
    }

    #[tokio::test]
    async fn tool_random_sample_bernoulli_multiple() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomSampleParams {
            distribution: "bernoulli".to_string(),
            mean: None,
            std_dev: None,
            lambda: None,
            p: Some(0.7),
            n: None,
            count: Some(10),
        };
        let result = server.random_sample(Parameters(params)).await.unwrap();
        let values: Vec<String> = serde_json::from_str(&extract_text(&result)).unwrap();
        assert_eq!(values.len(), 10);
        assert!(values.iter().all(|v| v == "true" || v == "false"));
    }

    #[tokio::test]
    async fn tool_random_sample_poisson() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomSampleParams {
            distribution: "poisson".to_string(),
            mean: None,
            std_dev: None,
            lambda: Some(3.0),
            p: None,
            n: None,
            count: None,
        };
        let result = server.random_sample(Parameters(params)).await.unwrap();
        assert!(!result.is_error.unwrap_or(false));
        let _v: u64 = extract_text(&result).parse().unwrap();
    }

    #[tokio::test]
    async fn tool_random_sample_binomial() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomSampleParams {
            distribution: "binomial".to_string(),
            mean: None,
            std_dev: None,
            lambda: None,
            p: Some(0.3),
            n: Some(100),
            count: Some(5),
        };
        let result = server.random_sample(Parameters(params)).await.unwrap();
        let values: Vec<String> = serde_json::from_str(&extract_text(&result)).unwrap();
        assert_eq!(values.len(), 5);
        assert!(values.iter().all(|s| {
            let v: u64 = s.parse().unwrap();
            v <= 100
        }));
    }

    #[tokio::test]
    async fn tool_random_sample_log_normal() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomSampleParams {
            distribution: "log_normal".to_string(),
            mean: Some(0.0),
            std_dev: Some(0.5),
            lambda: None,
            p: None,
            n: None,
            count: None,
        };
        let result = server.random_sample(Parameters(params)).await.unwrap();
        let v: f64 = extract_text(&result).parse().unwrap();
        assert!(v > 0.0);
    }

    #[tokio::test]
    async fn tool_random_sample_unknown() {
        let server = RandomServer::new(&HashSet::new());
        let params = RandomSampleParams {
            distribution: "weibull".to_string(),
            mean: None,
            std_dev: None,
            lambda: None,
            p: None,
            n: None,
            count: None,
        };
        let result = server.random_sample(Parameters(params)).await.unwrap();
        assert!(result.is_error.unwrap_or(false));
    }

    // ── CLI parsing ───────────────────────────────────────────────

    #[test]
    fn cli_no_args() {
        let cli = Cli::parse_from(["mcp-random"]);
        assert!(cli.disabled_tools.is_empty());
    }

    #[test]
    fn cli_disable_one() {
        let cli = Cli::parse_from(["mcp-random", "--disable", "random_uuid"]);
        assert_eq!(cli.disabled_tools, vec!["random_uuid"]);
    }

    #[test]
    fn cli_disable_multiple() {
        let cli = Cli::parse_from([
            "mcp-random",
            "--disable",
            "random_uuid",
            "--disable",
            "random_bytes",
        ]);
        assert_eq!(cli.disabled_tools, vec!["random_uuid", "random_bytes"]);
    }
}
