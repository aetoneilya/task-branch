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
    pub workspace_id: String,
    pub agent_session: Option<AgentSession>,
}

/// The agent's own session handle; for claude, the session id.
#[derive(Debug, Deserialize)]
pub struct AgentSession {
    pub value: String,
}

impl Agent {
    pub fn session_id(&self) -> Option<&str> {
        self.agent_session.as_ref().map(|s| s.value.as_str())
    }

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

/// Opens a tab in `workspace` without moving focus; returns its id and root pane id.
pub fn create_tab(workspace: &str, cwd: &Path, label: &str) -> Result<(String, String)> {
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
    Ok((
        result_field(response.clone(), "/result/tab/tab_id")?,
        result_field(response, "/result/root_pane/pane_id")?,
    ))
}

/// A new workspace without moving focus; returns its id, root tab id and root pane id.
pub fn create_workspace(label: &str, cwd: &Path) -> Result<(String, String, String)> {
    let cwd = cwd.to_string_lossy();
    let response = run_herdr(&[
        "workspace",
        "create",
        "--cwd",
        &cwd,
        "--label",
        label,
        "--no-focus",
    ])?;
    Ok((
        result_field(response.clone(), "/result/workspace/workspace_id")?,
        result_field(response.clone(), "/result/tab/tab_id")?,
        result_field(response, "/result/root_pane/pane_id")?,
    ))
}

/// Types `command` and Enter into the shell of `pane`.
pub fn run_in_pane(pane: &str, command: &str) -> Result<()> {
    let output = Command::new("herdr")
        .args(["pane", "run", pane, command])
        .output()?;
    if !output.status.success() {
        return Err(HerdrError::NonZeroExit(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(())
}

/// Names the agent `pane` will host once herdr detects it, in the background:
/// claude started by a plain command in the pane is detected a moment later.
pub fn name_agent_when_up(pane: &str, name: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;
    Command::new("sh")
        .args([
            "-c",
            r#"i=0; while [ $i -lt 120 ]; do herdr agent rename "$0" "$1" >/dev/null 2>&1 && exit 0; i=$((i+1)); sleep 0.5; done"#,
            pane,
            name,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()?;
    Ok(())
}

pub fn rename_tab(tab: &str, label: &str) -> Result<()> {
    run_herdr(&["tab", "rename", tab, label]).map(|_| ())
}

pub fn focus_tab(tab: &str) -> Result<()> {
    run_herdr(&["tab", "focus", tab]).map(|_| ())
}

/// Switches to the agent: its workspace, its tab, then its pane — `agent focus`
/// alone only makes the pane active inside a tab that may not be on screen.
/// Accepts an agent name or the pane id hosting it.
pub fn focus_agent(target: &str) -> Result<()> {
    let info = run_herdr(&["agent", "get", target])?;
    let field = |name: &str| {
        info.pointer(&format!("/result/agent/{name}"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    if let Some(workspace) = field("workspace_id") {
        run_herdr(&["workspace", "focus", &workspace])?;
    }
    if let Some(tab) = field("tab_id") {
        run_herdr(&["tab", "focus", &tab])?;
    }
    run_herdr(&["agent", "focus", target]).map(|_| ())
}

/// Starts claude in the shell of `pane` and names the agent once herdr sees it.
/// Typed as `command claude` rather than through `herdr agent start`: ai-launcher
/// keeps putting a `claude` shell function into ~/.zshrc that refuses to run it.
pub fn start_claude(name: &str, pane: &str, claude_args: &[&str]) -> Result<()> {
    let mut command = String::from("command claude");
    for arg in claude_args {
        command.push(' ');
        command.push_str(&shell_quote(arg));
    }
    run_in_pane(pane, &command)?;
    name_agent_when_up(pane, name)
}

fn shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', "'\\''"))
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
