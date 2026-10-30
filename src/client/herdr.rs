use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
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

/// herdr's stdout, or its stderr as the error.
fn herdr_output(args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new(herdr_bin()).args(args).output()?;
    if !output.status.success() {
        return Err(HerdrError::NonZeroExit(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(output.stdout)
}

fn herdr_bin() -> OsString {
    std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into())
}

/// Plugin actions and panes start in the plugin checkout, not the caller's repo.
pub fn caller_dir() -> Option<PathBuf> {
    let context = std::env::var("HERDR_PLUGIN_CONTEXT_JSON").ok()?;
    let context: serde_json::Value = serde_json::from_str(&context).ok()?;
    [
        "/focused_pane_cwd",
        "/worktree/checkout_path",
        "/workspace_cwd",
    ]
    .iter()
    .filter_map(|key| context.pointer(key).and_then(|v| v.as_str()))
    .find(|path| !path.is_empty())
    .map(PathBuf::from)
}

pub fn open_board() -> Result<()> {
    run_herdr(&[
        "plugin",
        "pane",
        "open",
        "--plugin",
        "worktable",
        "--entrypoint",
        "board",
    ])
    .map(|_| ())
}

fn run_herdr(args: &[&str]) -> Result<serde_json::Value> {
    Ok(serde_json::from_slice(&herdr_output(args)?)?)
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

#[derive(Debug, Deserialize)]
pub struct Tab {
    pub tab_id: String,
    pub label: String,
}

pub fn list_tabs() -> Result<Vec<Tab>> {
    result_field(run_herdr(&["tab", "list"])?, "/result/tabs")
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

/// A new workspace without moving focus; returns its root tab id and root pane id.
pub fn create_workspace(label: &str, cwd: &Path) -> Result<(String, String)> {
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
        result_field(response.clone(), "/result/tab/tab_id")?,
        result_field(response, "/result/root_pane/pane_id")?,
    ))
}

/// Types `command` and Enter into the shell of `pane`.
pub fn run_in_pane(pane: &str, command: &str) -> Result<()> {
    let command = if std::env::var_os("HERDR_PLUGIN_ROOT").is_some() {
        // Run after the pane's login shell has initialized, so its startup files
        // cannot hide this installation of wt. Agents inherit it for `wt note`.
        let exe = std::env::current_exe()?;
        let bin = exe.parent().unwrap_or(Path::new("."));
        format!(
            "export PATH={}:\"$PATH\"; export WORKTABLE_CONFIG={}; export HERDR_BIN_PATH={}; {command}",
            shell_quote(&bin.to_string_lossy()),
            shell_quote(&crate::config::config_path().to_string_lossy()),
            shell_quote(&herdr_bin().to_string_lossy()),
        )
    } else {
        command.to_string()
    };
    herdr_output(&["pane", "run", pane, &command]).map(|_| ())
}

/// Names the agent `pane` will host once herdr detects it, in the background:
/// claude started by a plain command in the pane is detected a moment later.
pub fn name_agent_when_up(pane: &str, name: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;
    Command::new("sh")
        .args([
            "-c",
            r#"i=0; while [ $i -lt 120 ]; do "$2" agent rename "$0" "$1" >/dev/null 2>&1 && exit 0; i=$((i+1)); sleep 0.5; done"#,
            pane,
            name,
            &herdr_bin().to_string_lossy(),
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

/// The last `lines` lines of what the agent (a name or its pane id) printed,
/// unwrapped.
pub fn read_agent(target: &str, lines: usize) -> Result<String> {
    let lines = lines.to_string();
    let output = herdr_output(&[
        "agent",
        "read",
        target,
        "--source",
        "recent-unwrapped",
        "--lines",
        &lines,
    ])?;
    Ok(String::from_utf8_lossy(&output).trim_end().to_string())
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

/// `arg` as one word for sh, in single quotes.
pub fn shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', "'\\''"))
}

/// Herdr agent names must match [a-z][a-z0-9_-]{0,31}: other characters
/// become one `-` per run, none at the ends; `agent` when nothing is left.
pub fn agent_name(raw: &str) -> String {
    let mut name = String::new();
    for c in raw.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            name.push(c);
        } else if !name.is_empty() && !name.ends_with('-') {
            name.push('-');
        }
    }
    let name = name.trim_end_matches('-');
    let name = if name.is_empty() {
        "agent".to_string()
    } else if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
        format!("wt-{name}")
    } else {
        name.to_string()
    };
    name.chars()
        .take(32)
        .collect::<String>()
        .trim_end_matches('-')
        .to_string()
}

/// `agent_name(base)`, with `-2`, `-3`, … when one of `taken` has it.
pub fn free_agent_name(base: &str, taken: &[String]) -> String {
    let base = agent_name(base);
    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .map(|n| {
            let suffix = format!("-{n}");
            let head: String = base.chars().take(32 - suffix.len()).collect();
            format!("{head}{suffix}")
        })
        .find(|candidate| !taken.contains(candidate))
        .unwrap_or(base)
}

pub fn rename_agent(target: &str, name: &str) -> Result<()> {
    run_herdr(&["agent", "rename", target, name]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_names_keep_what_herdr_takes() {
        assert_eq!(agent_name("a2i-config-folder"), "a2i-config-folder");
        assert_eq!(agent_name("javadoc-и ко всем точкам"), "javadoc");
        assert_eq!(agent_name("Схлопнуть d2d и hnsw"), "d2d-hnsw");
        assert_eq!(agent_name("единый образ"), "agent");
        assert_eq!(agent_name("2fa"), "wt-2fa");
        assert_eq!(agent_name(&"x-".repeat(40)).len(), 31);
        let taken = vec!["video".to_string(), "video-2".to_string()];
        assert_eq!(free_agent_name("video", &taken), "video-3");
    }
}
