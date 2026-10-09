use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::path::Path;
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HerdrError {
    #[error("failed to execute herdr: {0}")]
    Io(#[from] std::io::Error),

    #[error("herdr command failed: {0}")]
    NonZeroExit(String),

    #[error("can't parse herdr response: {0}")]
    ParseError(#[from] serde_json::Error),
}

pub(crate) type Result<T> = std::result::Result<T, HerdrError>;

/// True when running inside a herdr-managed pane.
pub fn available() -> bool {
    std::env::var("HERDR_ENV").is_ok_and(|v| v == "1")
}

fn run_herdr(args: &[&str]) -> Result<serde_json::Value> {
    let output = Command::new("herdr").args(args).output()?;
    if !output.status.success() {
        return Err(HerdrError::NonZeroExit(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn result_field<T: DeserializeOwned>(value: serde_json::Value, pointer: &str) -> Result<T> {
    let field = value.pointer(pointer).cloned().unwrap_or_default();
    Ok(serde_json::from_value(field)?)
}

#[derive(Debug, Deserialize)]
pub struct Agent {
    pub name: Option<String>,
    pub agent_status: String,
    pub cwd: Option<String>,
    pub pane_id: String,
    pub tab_id: String,
}

impl Agent {
    pub fn label(&self) -> String {
        let name = self.name.as_deref().unwrap_or(&self.pane_id);
        format!("{}:{}", name, self.agent_status)
    }

    pub fn runs_in(&self, dir: &Path) -> bool {
        self.cwd
            .as_deref()
            .is_some_and(|cwd| Path::new(cwd).starts_with(dir))
    }
}

#[derive(Debug, Deserialize)]
pub struct Workspace {
    pub workspace_id: String,
    pub label: String,
    pub worktree: Option<WorkspaceWorktree>,
}

#[derive(Debug, Deserialize)]
pub struct WorkspaceWorktree {
    pub checkout_path: String,
}

pub fn list_agents() -> Result<Vec<Agent>> {
    result_field(run_herdr(&["agent", "list"])?, "/result/agents")
}

pub fn list_workspaces() -> Result<Vec<Workspace>> {
    result_field(run_herdr(&["workspace", "list"])?, "/result/workspaces")
}

pub fn close_workspace(workspace_id: &str) -> Result<()> {
    run_herdr(&["workspace", "close", workspace_id]).map(|_| ())
}

pub fn close_tab(tab_id: &str) -> Result<()> {
    run_herdr(&["tab", "close", tab_id]).map(|_| ())
}

/// Splits `pane` without moving focus and returns the new pane id.
pub fn split_pane(pane: &str, direction: &str, cwd: &Path) -> Result<String> {
    let cwd = cwd.to_string_lossy();
    let response = run_herdr(&[
        "pane",
        "split",
        "--pane",
        pane,
        "--direction",
        direction,
        "--cwd",
        &cwd,
        "--no-focus",
    ])?;
    result_field(response, "/result/pane/pane_id")
}

/// Opens a tab in `workspace` without moving focus and returns its root pane id.
pub fn create_tab(workspace: &str, cwd: &Path, label: &str) -> Result<String> {
    let cwd = cwd.to_string_lossy();
    let response = run_herdr(&[
        "tab",
        "create",
        "--workspace",
        workspace,
        "--cwd",
        &cwd,
        "--label",
        label,
        "--no-focus",
    ])?;
    result_field(response, "/result/root_pane/pane_id")
}

/// Starts claude in `pane`. An agent blocked by a startup dialog (folder trust)
/// still counts as started: its initial prompt runs once the dialog is answered.
pub fn start_claude(name: &str, pane: &str, claude_args: &[&str]) -> Result<()> {
    let mut args = vec!["agent", "start", name, "--kind", "claude", "--pane", pane];
    if !claude_args.is_empty() {
        args.push("--");
        args.extend_from_slice(claude_args);
    }
    match run_herdr(&args) {
        Err(HerdrError::NonZeroExit(message)) if message.contains("agent_not_ready") => Ok(()),
        other => other.map(|_| ()),
    }
}

/// Herdr agent names must match [a-z][a-z0-9_-]{0,31}.
pub fn agent_name(raw: &str) -> String {
    let mut name: String = raw
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
        name = format!("wt-{name}");
    }
    name.chars().take(32).collect()
}
