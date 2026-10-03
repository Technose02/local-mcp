//! Configuration: `config.toml` plus `clap` command-line overrides.

use std::path::PathBuf;

use clap::Parser;
use serde::Deserialize;

use crate::error::{AppError, ConfigError};

/// Example configuration, also reachable via `--print-example-config`.
pub const EXAMPLE_CONFIG: &str = include_str!("../config.toml.example");

/// Default browser-like user agent. Keyless scrapers (DuckDuckGo) need this.
pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36";

#[derive(Parser, Debug, Clone)]
#[command(
    name = "local-mcp",
    version,
    about = "Local MCP server: web search, page fetching and current date/time (Streamable HTTP)"
)]
pub struct Cli {
    /// Path to the TOML configuration file.
    #[arg(long, value_name = "PATH", default_value = "config.toml")]
    pub config: PathBuf,

    /// Override the bind address, e.g. `127.0.0.1:8000` or `0.0.0.0:8000`.
    #[arg(long, value_name = "ADDR")]
    pub bind: Option<String>,

    /// Override the log level (`error`, `warn`, `info`, `debug`, `trace`).
    #[arg(long, value_name = "LEVEL")]
    pub log_level: Option<String>,

    /// Override the ordered, comma-separated search provider chain.
    #[arg(long, value_name = "LIST", value_delimiter = ',')]
    pub providers: Option<Vec<String>>,

    /// Print an example configuration file and exit.
    #[arg(long)]
    pub print_example_config: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct AppConfig {
    pub server: ServerSettings,
    pub http: HttpSettings,
    pub tools: ToolSettings,
    pub usage: UsageSettings,
    pub providers: ProvidersSettings,
    pub time: TimeSettings,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerSettings {
    pub bind: String,
    pub path: String,
    pub json_response: bool,
    pub legacy_session_mode: bool,
    /// Browser origins allowed to call the server. `["*"]` allows any origin;
    /// an empty list disables CORS entirely.
    pub cors_origins: Vec<String>,
    pub tls: TlsSettings,
}

/// Optional HTTPS settings. Only usable when the binary is built with the `tls`
/// Cargo feature; the default build serves plain HTTP.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TlsSettings {
    pub enabled: bool,
    pub cert_path: Option<PathBuf>,
    pub key_path: Option<PathBuf>,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8000".to_string(),
            path: "/mcp".to_string(),
            json_response: true,
            legacy_session_mode: false,
            cors_origins: vec!["*".to_string()],
            tls: TlsSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct HttpSettings {
    pub timeout_ms: u64,
    pub user_agent: String,
}

impl Default for HttpSettings {
    fn default() -> Self {
        Self {
            timeout_ms: 15_000,
            user_agent: DEFAULT_USER_AGENT.to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ToolSettings {
    /// Tool names that stay registered but return an error when invoked.
    pub disabled: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UsageSettings {
    /// Disable providers that are not acceptable for commercial use.
    pub commercial: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TimeSettings {
    /// Timezone used when no reliable region clue is available.
    pub default_timezone: String,
}

impl Default for TimeSettings {
    fn default() -> Self {
        Self {
            default_timezone: "Europe/Berlin".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ProvidersSettings {
    /// Ordered search-provider fallback chain.
    pub order: Vec<String>,
    /// Ordered content-extraction chain.
    pub extract_order: Vec<String>,
    pub tavily: ProviderOptions,
    pub firecrawl: ProviderOptions,
    pub duckduckgo: ProviderOptions,
    pub wikipedia: ProviderOptions,
}

impl Default for ProvidersSettings {
    fn default() -> Self {
        Self {
            order: vec!["tavily".into(), "duckduckgo".into(), "wikipedia".into()],
            extract_order: vec!["tavily".into(), "firecrawl".into(), "direct".into()],
            tavily: ProviderOptions::default(),
            firecrawl: ProviderOptions::default(),
            duckduckgo: ProviderOptions::default(),
            wikipedia: ProviderOptions::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ProviderOptions {
    pub enabled: bool,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}

impl Default for ProviderOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key: None,
            base_url: None,
        }
    }
}

impl ProviderOptions {
    /// Return the configured API key, falling back to an environment variable.
    pub fn resolved_key(&self, env_var: &str) -> Option<String> {
        self.api_key
            .as_ref()
            .map(|key| key.trim())
            .filter(|key| !key.is_empty())
            .map(str::to_string)
            .or_else(|| {
                std::env::var(env_var)
                    .ok()
                    .map(|key| key.trim().to_string())
                    .filter(|key| !key.is_empty())
            })
    }
}

/// Load configuration from disk and apply CLI overrides.
pub fn load_config(cli: &Cli) -> Result<AppConfig, AppError> {
    let mut config = if cli.config.exists() {
        let text = std::fs::read_to_string(&cli.config)
            .map_err(|error| ConfigError::read(&cli.config, error))?;
        toml::from_str::<AppConfig>(&text)
            .map_err(|error| ConfigError::parse(&cli.config, error))?
    } else {
        tracing::warn!(path = %cli.config.display(), "config file not found; using defaults");
        AppConfig::default()
    };

    if let Some(bind) = &cli.bind {
        config.server.bind = bind.clone();
    }
    if let Some(order) = &cli.providers {
        let order: Vec<String> = order
            .iter()
            .map(|name| name.trim().to_lowercase())
            .filter(|n| !n.is_empty())
            .collect();
        if !order.is_empty() {
            config.providers.order = order;
        }
    }
    if config.providers.extract_order.is_empty() {
        config.providers.extract_order = ProvidersSettings::default().extract_order;
    }

    Ok(config)
}
