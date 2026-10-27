use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
pub struct GitLabConfig {
    pub host: String,
    pub project: String,
}

#[derive(Debug, Clone, Deserialize)]
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

/// Where a project's task notes live in the Obsidian vault.
#[derive(Debug, Clone)]
pub struct VaultConfig {
    pub path: PathBuf,
    /// Folders scanned for task notes, relative to `path`.
    pub task_dirs: Vec<PathBuf>,
    /// Folder for notes created by `wt start <name>`, relative to `path`.
    pub new_task_dir: PathBuf,
}

/// A repository whose worktrees and merge requests belong to tasks.
#[derive(Debug, Clone, Deserialize)]
pub struct RepoConfig {
    pub path: PathBuf,
    /// Where its merge requests are; none without GitLab.
    pub gitlab_project: Option<String>,
}

/// One project, as every command sees it: its notes, repos, and the Jira and
/// GitLab it has, if any.
#[derive(Debug, Clone)]
pub struct Config {
    /// Its tab in the TUI, `wt -p <name>`.
    pub name: String,
    pub jira: Option<JiraConfig>,
    pub gitlab: Option<GitLabConfig>,
    pub workflow: WorkflowConfig,
    pub vault: VaultConfig,
    pub repos: Vec<RepoConfig>,
}

/// The config file: the vault, defaults for every project, the projects.
#[derive(Debug, Deserialize)]
struct FileConfig {
    vault: VaultFile,
    #[serde(default)]
    workflow: WorkflowConfig,
    #[serde(rename = "project")]
    projects: Vec<ProjectFile>,
}

#[derive(Debug, Deserialize)]
struct VaultFile {
    path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct ProjectFile {
    name: String,
    jira: Option<JiraConfig>,
    gitlab: Option<GitLabConfig>,
    task_dirs: Vec<PathBuf>,
    /// Default: the first of `task_dirs`.
    new_task_dir: Option<PathBuf>,
    /// Default: `[workflow] branch_prefix`.
    branch_prefix: Option<String>,
    #[serde(default, rename = "repo")]
    repos: Vec<RepoConfig>,
}

impl FileConfig {
    /// Each project with the shared parts filled in and `~` expanded.
    fn projects(self) -> Result<Vec<Config>, String> {
        let vault = expand_home(&self.vault.path);
        self.projects
            .into_iter()
            .map(|p| {
                let new_task_dir = p
                    .new_task_dir
                    .or_else(|| p.task_dirs.first().cloned())
                    .ok_or_else(|| format!("project {}: no task_dirs", p.name))?;
                let mut workflow = self.workflow.clone();
                if let Some(prefix) = p.branch_prefix {
                    workflow.branch_prefix = prefix;
                }
                Ok(Config {
                    name: p.name,
                    jira: p.jira,
                    gitlab: p.gitlab,
                    workflow,
                    vault: VaultConfig {
                        path: vault.clone(),
                        task_dirs: p.task_dirs,
                        new_task_dir,
                    },
                    repos: p
                        .repos
                        .into_iter()
                        .map(|r| RepoConfig {
                            path: expand_home(&r.path),
                            ..r
                        })
                        .collect(),
                })
            })
            .collect()
    }
}

impl Config {
    /// A Jira client for the project; an error when it has no Jira.
    pub fn jira_client(&self) -> Result<crate::client::jira::JiraClient, String> {
        let jira = self
            .jira
            .as_ref()
            .ok_or_else(|| format!("project {} has no Jira", self.name))?;
        crate::client::jira::JiraClient::from_env(&jira.host)
    }
}

fn expand_home(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), dirs::home_dir()) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path.to_path_buf(),
    }
}

/// Every configured project, in the file's order.
pub fn load_projects() -> Result<Vec<Config>, ConfigError> {
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
    let file: FileConfig =
        toml::from_str(&contents).map_err(|e| ConfigError::Parse(config_path.clone(), e))?;
    let projects = file
        .projects()
        .map_err(|e| ConfigError::Invalid(config_path.clone(), e))?;
    if projects.is_empty() {
        return Err(ConfigError::Invalid(config_path, "no [[project]]".into()));
    }
    Ok(projects)
}

/// The project named `name`; without a name, the one whose repo `dir` is in,
/// else the first.
pub fn choose_project(
    projects: &[Config],
    name: Option<&str>,
    dir: Option<&Path>,
) -> Result<usize, String> {
    if let Some(name) = name {
        return projects
            .iter()
            .position(|p| p.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
                format!("no project \"{name}\": {}", names.join(", "))
            });
    }
    Ok(dir.and_then(|dir| project_of(projects, dir)).unwrap_or(0))
}

/// The project a repo of which, or a worktree under it, holds `dir`.
pub fn project_of(projects: &[Config], dir: &Path) -> Option<usize> {
    projects
        .iter()
        .position(|p| p.repos.iter().any(|r| dir.starts_with(&r.path)))
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
    #[error("config {}: {}", .0.display(), .1)]
    Invalid(PathBuf, String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_share_the_vault_and_defaults() {
        let file: FileConfig = toml::from_str(
            r#"
            [vault]
            path = "/v"
            [workflow]
            branch_prefix = "me/"

            [[project]]
            name = "ucp"
            task_dirs = ["30-work/tasks"]
            jira = { host = "https://jira" }
            [[project.repo]]
            path = "/code/ucp"
            gitlab_project = "ai/ucp"

            [[project]]
            name = "vpn"
            task_dirs = ["10/projects/vpn"]
            branch_prefix = ""
            [[project.repo]]
            path = "/code/vpn-bot"
            "#,
        )
        .unwrap();
        let projects = file.projects().unwrap();
        let (ucp, vpn) = (&projects[0], &projects[1]);
        assert_eq!(ucp.vault.new_task_dir, PathBuf::from("30-work/tasks"));
        assert_eq!(ucp.workflow.branch_prefix, "me/");
        assert!(ucp.jira.is_some() && ucp.gitlab.is_none());
        assert_eq!(vpn.vault.path, PathBuf::from("/v"));
        assert_eq!(vpn.workflow.branch_prefix, "");
        assert!(vpn.jira.is_none() && vpn.repos[0].gitlab_project.is_none());

        let dir = Path::new("/code/vpn-bot/.claude/worktrees/fix");
        assert_eq!(choose_project(&projects, None, Some(dir)), Ok(1));
        assert_eq!(choose_project(&projects, None, None), Ok(0));
        assert_eq!(choose_project(&projects, Some("VPN"), None), Ok(1));
        assert!(choose_project(&projects, Some("kitten"), None).is_err());
    }
}
