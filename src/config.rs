use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Deserialize)]
pub struct JiraConfig {
    pub host: String,
}

#[derive(Debug, Deserialize)]
pub struct GitLabConfig {
    pub host: String,
    pub project: String,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct WorkflowConfig {
    /// Prepended to new task branches, e.g. "i.udalov/".
    pub branch_prefix: String,
    /// `jg start` asks for confirmation when this many agents are already live.
    pub max_agents: usize,
    /// Model for `jg review`; the claude default when empty.
    pub review_model: Option<String>,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        WorkflowConfig {
            branch_prefix: String::new(),
            max_agents: 4,
            review_model: None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub jira: JiraConfig,
    pub gitlab: GitLabConfig,
    #[serde(default)]
    pub workflow: WorkflowConfig,
}

pub fn load_config() -> Result<Config, ConfigError> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .or_else(|| dirs::config_dir())
        .unwrap_or_else(|| PathBuf::from("."));

    let config_path = base.join("jg").join("config.toml");

    let contents = fs::read_to_string(config_path)?;
    let config: Config = toml::from_str(&contents)?;
    Ok(config)
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to load config")]
    ConfigLoadError(#[from] std::io::Error),
    #[error("failed to deserialize config")]
    DeserializationError(#[from] toml::de::Error),
}
