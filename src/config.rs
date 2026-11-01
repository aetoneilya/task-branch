use crate::client::agent::AgentKind;
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
    /// CLI used for new conversations; existing conversations keep their owner.
    pub agent: AgentKind,
    /// Optional separate CLI for reviews.
    pub review_agent: Option<AgentKind>,
    /// An explicit --agent filter for this invocation only.
    #[serde(skip)]
    pub agent_override: Option<AgentKind>,
    /// Model for Claude reviews; Claude's default when empty.
    pub review_model: Option<String>,
    /// Model for Codex reviews; Codex's configured default when empty.
    pub codex_review_model: Option<String>,
    /// macOS app the TUI's `i` opens a worktree in.
    pub ide: String,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        WorkflowConfig {
            branch_prefix: String::new(),
            agent: AgentKind::Claude,
            review_agent: None,
            agent_override: None,
            review_model: None,
            codex_review_model: None,
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
    /// Default: [workflow] agent.
    agent: Option<AgentKind>,
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
                if let Some(agent) = p.agent {
                    workflow.agent = agent;
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

pub fn expand_home(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), dirs::home_dir()) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path.to_path_buf(),
    }
}

/// `$WORKTABLE_CONFIG`, else `worktable/config.toml` in the config dir.
fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .or_else(dirs::config_dir)
        .unwrap_or_else(|| PathBuf::from("."));

    std::env::var_os("WORKTABLE_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| base.join("worktable").join("config.toml"))
}

/// Every configured project, in the file's order.
pub fn load_projects() -> Result<Vec<Config>, ConfigError> {
    let config_path = config_path();
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

/// Global preferences edited in the TUI. Project and command-line overrides
/// still take precedence over these defaults.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    pub default_agent: AgentKind,
}

pub fn load_settings() -> Result<Settings, String> {
    let path = config_path();
    let text =
        fs::read_to_string(&path).map_err(|e| ConfigError::Read(path.clone(), e).to_string())?;
    let file: FileConfig =
        toml::from_str(&text).map_err(|e| ConfigError::Parse(path, e).to_string())?;
    Ok(Settings {
        default_agent: file.workflow.agent,
    })
}

/// Updates only the preferences; comments, unknown keys and project overrides
/// are kept. Return the effective project defaults for the running TUI.
pub fn save_settings(settings: Settings) -> Result<Vec<Config>, String> {
    save_settings_at(&config_path(), settings)
}

fn save_settings_at(path: &Path, settings: Settings) -> Result<Vec<Config>, String> {
    // A config managed through a symlink must keep pointing at the same file.
    let target =
        fs::canonicalize(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    let path = target.as_path();
    let text =
        fs::read_to_string(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("can't parse {}: {e}", path.display()))?;
    if !doc.contains_key("workflow") {
        doc["workflow"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let workflow = doc["workflow"]
        .as_table_like_mut()
        .ok_or("workflow must be a table")?;
    if let Some(value) = workflow
        .get_mut("agent")
        .and_then(toml_edit::Item::as_value_mut)
    {
        let decor = value.decor().clone();
        *value = toml_edit::Value::from(settings.default_agent.binary());
        *value.decor_mut() = decor;
    } else {
        workflow.insert("agent", toml_edit::value(settings.default_agent.binary()));
    }
    let text = doc.to_string();
    // Validate the whole config before touching the file.
    let file: FileConfig = toml::from_str(&text).map_err(|e| e.to_string())?;
    let projects = file.projects()?;
    if projects.is_empty() {
        return Err("no [[project]]".into());
    }
    let temp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
    let result = (|| {
        fs::write(&temp, text)?;
        fs::set_permissions(&temp, fs::metadata(path)?.permissions())?;
        fs::rename(&temp, path)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&temp);
        return Err(format!("can't save {}: {e}", path.display()));
    }
    Ok(projects)
}

/// A `[[project]]` with its notes in `task_dir` and, if given, one repo: the
/// config file gets it at its end, the rest of the file left as it is.
pub fn add_project(name: &str, task_dir: &str, repo: Option<&str>) -> Result<Config, String> {
    let path = config_path();
    let mut contents =
        fs::read_to_string(&path).map_err(|e| ConfigError::Read(path.clone(), e).to_string())?;
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents.push_str(&project_block(name, task_dir, repo));
    // checked before it is written: a file that no longer parses breaks every command
    let file: FileConfig =
        toml::from_str(&contents).map_err(|e| ConfigError::Parse(path.clone(), e).to_string())?;
    let config = file
        .projects()?
        .pop()
        .ok_or_else(|| "the project did not make it into the config".to_string())?;
    fs::write(&path, contents).map_err(|e| format!("can't write {}: {e}", path.display()))?;
    Ok(config)
}

fn project_block(name: &str, task_dir: &str, repo: Option<&str>) -> String {
    let quote = |s: &str| toml::Value::String(s.to_string()).to_string();
    let mut block = format!(
        "\n[[project]]\nname = {}\ntask_dirs = [{}]\n",
        quote(name),
        quote(task_dir)
    );
    if let Some(repo) = repo {
        block.push_str(&format!("\n[[project.repo]]\npath = {}\n", quote(repo)));
    }
    block
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

    struct SettingsFile(PathBuf);

    impl SettingsFile {
        fn new(text: &str) -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "wt-settings-{}-{}.toml",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::write(&path, text).unwrap();
            Self(path)
        }
    }

    impl Drop for SettingsFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn settings_preserve_comments_unknown_keys_and_project_overrides() {
        let text = "# my config\n[vault]\npath = '/v'\n[workflow]\nagent = 'claude' # keep this\nide = 'Editor'\ncustom = 42\n\n[[project]]\nname = 'shared'\ntask_dirs = ['tasks']\n\n[[project]]\nname = 'own'\ntask_dirs = ['other']\nagent = 'claude'\n";
        let file = SettingsFile::new(text);
        let projects = save_settings_at(
            &file.0,
            Settings {
                default_agent: AgentKind::Codex,
            },
        )
        .unwrap();
        let saved = fs::read_to_string(&file.0).unwrap();
        assert_eq!(
            saved,
            text.replacen("agent = 'claude'", "agent = \"codex\"", 1)
        );
        assert_eq!(projects[0].workflow.agent, AgentKind::Codex);
        assert_eq!(projects[1].workflow.agent, AgentKind::Claude);
        let reloaded: FileConfig = toml::from_str(&saved).unwrap();
        assert_eq!(reloaded.workflow.agent, AgentKind::Codex);
    }

    #[test]
    fn settings_support_missing_and_inline_workflow_tables() {
        for workflow in ["", "workflow = { ide = 'Editor' }\n"] {
            let file = SettingsFile::new(&format!(
                "{workflow}[vault]\npath = '/v'\n[[project]]\nname = 'p'\ntask_dirs = ['tasks']\n"
            ));
            let projects = save_settings_at(
                &file.0,
                Settings {
                    default_agent: AgentKind::Codex,
                },
            )
            .unwrap();
            assert_eq!(projects[0].workflow.agent, AgentKind::Codex);
            assert_eq!(
                projects[0].workflow.ide,
                if workflow.is_empty() {
                    "IntelliJ IDEA"
                } else {
                    "Editor"
                }
            );
        }
    }

    #[test]
    fn invalid_config_is_left_intact() {
        for text in [
            "[broken",
            "[vault]\npath = '/v'\nproject = []",
            "[vault]\npath = '/v'\n[[project]]\nname = 'p'\ntask_dirs = []",
        ] {
            let file = SettingsFile::new(text);
            assert!(
                save_settings_at(
                    &file.0,
                    Settings {
                        default_agent: AgentKind::Codex
                    }
                )
                .is_err()
            );
            assert_eq!(fs::read_to_string(&file.0).unwrap(), text);
        }
    }

    #[test]
    #[cfg(unix)]
    fn saving_through_a_symlink_keeps_the_link_and_file_permissions() {
        use std::os::unix::{fs::PermissionsExt, fs::symlink};
        let file = SettingsFile::new(
            "[vault]\npath = '/v'\n[[project]]\nname = 'p'\ntask_dirs = ['tasks']\n",
        );
        fs::set_permissions(&file.0, fs::Permissions::from_mode(0o600)).unwrap();
        let link = SettingsFile(file.0.with_extension("link"));
        symlink(&file.0, &link.0).unwrap();
        save_settings_at(
            &link.0,
            Settings {
                default_agent: AgentKind::Codex,
            },
        )
        .unwrap();
        assert!(fs::symlink_metadata(&link.0).unwrap().is_symlink());
        assert_eq!(
            fs::metadata(&file.0).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let reloaded: FileConfig = toml::from_str(&fs::read_to_string(&file.0).unwrap()).unwrap();
        assert_eq!(reloaded.workflow.agent, AgentKind::Codex);
    }

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

    #[test]
    fn agent_defaults_and_project_overrides_are_backward_compatible() {
        let text = r#"
            [vault]
            path = "/v"
            [[project]]
            name = "legacy"
            task_dirs = ["tasks"]
        "#;
        let file: FileConfig = toml::from_str(text).unwrap();
        assert_eq!(
            file.projects().unwrap()[0].workflow.agent,
            AgentKind::Claude
        );
        let text = r#"
            [vault]
            path = "/v"
            [workflow]
            agent = "codex"
            review_agent = "claude"
            review_model = "claude-review-model"
            codex_review_model = "codex-review-model"
            [[project]]
            name = "default"
            task_dirs = ["tasks"]
            [[project]]
            name = "override"
            task_dirs = ["other"]
            agent = "claude"
        "#;
        let file: FileConfig = toml::from_str(text).unwrap();
        let projects = file.projects().unwrap();
        assert_eq!(projects[0].workflow.agent, AgentKind::Codex);
        assert_eq!(projects[1].workflow.agent, AgentKind::Claude);
        assert_eq!(projects[0].workflow.review_agent, Some(AgentKind::Claude));
        assert_eq!(
            projects[0].workflow.codex_review_model.as_deref(),
            Some("codex-review-model")
        );
        assert!(
            toml::from_str::<FileConfig>(&text.replace("agent = \"codex\"", "agent = \"typo\""))
                .is_err()
        );
    }

    #[test]
    fn an_added_project_goes_last_with_its_repo() {
        let mut text =
            "[vault]\npath = \"/v\"\n\n[[project]]\nname = \"ucp\"\ntask_dirs = [\"t\"]\n"
                .to_string();
        text.push_str(&project_block(
            "kcal \"bot\"",
            "10/projects/kcal",
            Some("~/code/kcal"),
        ));
        text.push_str(&project_block("notes", "10/notes", None));
        let file: FileConfig = toml::from_str(&text).unwrap();
        let projects = file.projects().unwrap();
        let names: Vec<_> = projects.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["ucp", "kcal \"bot\"", "notes"]);
        assert_eq!(
            projects[1].vault.new_task_dir,
            PathBuf::from("10/projects/kcal")
        );
        assert!(projects[1].repos[0].path.ends_with("code/kcal"));
        assert!(projects[2].repos.is_empty());
    }
}
