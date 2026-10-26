use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Deserialize)]
pub struct JiraConfig {
    pub host: String,
    /// Where `wt start --new-ticket` creates tickets, e.g. "UCP".
    pub project: Option<String>,
    /// Their type, by name as the project shows it.
    #[serde(default = "default_issue_type")]
    pub issue_type: String,
}

fn default_issue_type() -> String {
    "Task".to_string()
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
    /// Model for `wt review`; the claude default when empty.
    pub review_model: Option<String>,
    /// macOS app the TUI's `i` opens a worktree in.
    pub ide: String,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        WorkflowConfig {
            branch_prefix: String::new(),
            review_model: None,
            ide: "IntelliJ IDEA".to_string(),
        }
    }
}

/// Where task notes live in the Obsidian vault.
#[derive(Debug, Deserialize)]
pub struct VaultConfig {
    pub path: PathBuf,
    /// Folders scanned for task notes, relative to `path`.
    pub task_dirs: Vec<PathBuf>,
    /// Folder for notes created by `wt start <name>`, relative to `path`.
    pub new_task_dir: PathBuf,
}

/// A repository whose worktrees and merge requests belong to tasks.
#[derive(Debug, Deserialize)]
pub struct RepoConfig {
    pub path: PathBuf,
    pub gitlab_project: String,
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub jira: JiraConfig,
    pub gitlab: GitLabConfig,
    #[serde(default)]
    pub workflow: WorkflowConfig,
    pub vault: Option<VaultConfig>,
    #[serde(default, rename = "repo")]
    pub repos: Vec<RepoConfig>,
}

impl Config {
    /// Paths with `~` expanded.
    fn expand(mut self) -> Self {
        if let Some(vault) = self.vault.as_mut() {
            vault.path = expand_home(&vault.path);
        }
        for repo in &mut self.repos {
            repo.path = expand_home(&repo.path);
        }
        self
    }
}

fn expand_home(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), dirs::home_dir()) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path.to_path_buf(),
    }
}

pub fn load_config() -> Result<Config, ConfigError> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .or_else(dirs::config_dir)
        .unwrap_or_else(|| PathBuf::from("."));

    let config_path = std::env::var_os("WORKTABLE_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| base.join("worktable").join("config.toml"));

    let contents =
        fs::read_to_string(&config_path).map_err(|e| ConfigError::Read(config_path.clone(), e))?;
    let config: Config =
        toml::from_str(&contents).map_err(|e| ConfigError::Parse(config_path, e))?;
    Ok(config.expand())
}

/// worktable's own cache: last Jira and GitLab answers, the session index, the
/// notes lock, `wt prepare` specs.
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("worktable")
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("can't read config {}: {}", .0.display(), .1)]
    Read(PathBuf, #[source] std::io::Error),
    #[error("can't parse config {}: {}", .0.display(), .1)]
    Parse(PathBuf, #[source] toml::de::Error),
}
