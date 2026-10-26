//! Opening worktree tabs and claude agents in herdr, closing them; the hidden
//! `wt prepare`.

use super::brief::{agent_prompt, write_brief};
use super::common::{Ui, err, same_name};
use crate::cli::progress;
use crate::client::herdr::shell_quote;
use crate::client::{herdr, worktree};
use crate::vault::task::Task;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A fresh claude in a tab of the herdr workspace `space`, pointed at the task
/// note and the brief: it reads them and waits — the work starts with analysis,
/// on my command.
pub(super) fn launch(
    space: &str,
    cwd: &Path,
    label: &str,
    note: Option<&Path>,
    brief: &Path,
    focus: bool,
    ui: &mut dyn Ui,
) -> Result<(), String> {
    let prompt = agent_prompt(note, brief);
    let args: Vec<&str> = prompt.as_deref().into_iter().collect();
    open_agent(space, cwd, label, &args, focus, ui)
}

/// What `wt prepare` does in the new tab, handed over in a file.
#[derive(Serialize, Deserialize)]
struct PrepareSpec {
    main: PathBuf,
    name: String,
    branch: String,
    brief: String,
    note: Option<PathBuf>,
    pane: String,
    agent: String,
}

/// A tab for the worktree `name` in the task's herdr workspace. Fetch and
/// checkout of a big repo take half a minute, so they run in the tab itself:
/// `wt prepare` shows the progress there and writes the brief, then the shell
/// cds into the worktree and starts claude,
/// and `wt start` returns at once. Without herdr it all happens here.
pub(super) fn open_work(
    task: &Task,
    main: &Path,
    name: &str,
    branch: &str,
    brief: String,
    focus: bool,
    ui: &mut dyn Ui,
) -> Result<(), String> {
    let (space, note) = (task.name.as_str(), Some(task.note.path.as_path()));
    let dir = worktree::worktree_dir(main, name);
    if !herdr::available() {
        let dir = worktree::create_worktree(main, name, branch).map_err(err)?;
        ui.say(format!("worktree: {} ({branch})", dir.display()));
        let brief = write_brief(&dir, &brief)?;
        let prompt = agent_prompt(note, &brief).unwrap_or_default();
        ui.say(format!(
            "not in herdr — run it yourself: cd {} && claude '{prompt}'",
            dir.display()
        ));
        return Ok(());
    }

    let cwd = if dir.exists() { &dir } else { main };
    let (tab, pane) = open_tab(space, cwd, name)?;
    let agent = free_agent_name(name);
    let spec = PrepareSpec {
        main: main.to_path_buf(),
        name: name.to_string(),
        branch: branch.to_string(),
        brief,
        note: note.map(Path::to_path_buf),
        pane: pane.clone(),
        agent: agent.clone(),
    };
    let spec_path =
        crate::config::cache_dir().join(format!("prepare-{pane}.json").replace(':', "-"));
    if let Some(parent) = spec_path.parent() {
        std::fs::create_dir_all(parent).map_err(err)?;
    }
    std::fs::write(&spec_path, serde_json::to_vec(&spec).map_err(err)?).map_err(err)?;

    let exe = std::env::current_exe().map_err(err)?;
    let prompt = prompt_path(&spec_path);
    let command = format!(
        "{} prepare {spec} && cd {dir} && command claude \"$(cat {prompt}; rm -f {prompt})\"",
        shell_quote(&exe.to_string_lossy()),
        spec = shell_quote(&spec_path.to_string_lossy()),
        dir = shell_quote(&dir.to_string_lossy()),
        prompt = shell_quote(&prompt.to_string_lossy()),
    );
    herdr::run_in_pane(&pane, &command).map_err(err)?;
    if focus {
        let _ = herdr::focus_tab(&tab);
    }
    ui.say(format!(
        "tab {name} → workspace \"{space}\": the worktree ({branch}) is being prepared there, then claude {agent} starts"
    ));
    Ok(())
}

/// The task's herdr workspace with a shell in `cwd`, no worktree or agent.
pub(super) fn open_space(
    task: &Task,
    cwd: &Path,
    focus: bool,
    ui: &mut dyn Ui,
) -> Result<(), String> {
    if !herdr::available() {
        ui.say("not in herdr — no workspace".to_string());
        return Ok(());
    }
    let (tab, _) = herdr::create_workspace(&task.name, cwd).map_err(err)?;
    if focus {
        let _ = herdr::focus_tab(&tab);
    }
    ui.say(format!("workspace \"{}\"", task.name));
    Ok(())
}

/// `wt prepare <spec>`, run by `open_work` in the new tab: creates the worktree
/// with progress on screen, writes the brief and the first prompt; the tab's
/// shell then cds into the worktree and starts claude.
pub(in crate::cli) fn cli_prepare(spec_path: &Path) -> Result<(), String> {
    let spec: PrepareSpec =
        serde_json::from_slice(&std::fs::read(spec_path).map_err(err)?).map_err(err)?;
    let dir = worktree::worktree_dir(&spec.main, &spec.name);
    let started = std::time::Instant::now();

    if !dir.exists() {
        println!("{} ({})", dir.display(), spec.branch);
        let plan = progress::step("worktree plan", || {
            worktree::plan_worktree(&spec.main, &spec.name, &spec.branch)
        })?;
        if let Some(remote_branch) = &plan.fetch {
            progress::step(&format!("git fetch origin {remote_branch}"), || {
                worktree::fetch(&spec.main, remote_branch)
            })?;
        }
        progress::loud_step("git worktree add", || {
            worktree::add_worktree(&spec.main, &plan, true)
        })?;
        worktree::copy_worktree_includes(&spec.main, &dir);
    }
    let brief = write_brief(&dir, &spec.brief)?;
    println!(
        "ready in {:.0} s, starting claude",
        started.elapsed().as_secs_f64()
    );

    // the shell starts claude from the worktree itself: herdr shows the branch
    // of the pane's shell, which would otherwise stay in the main checkout
    let prompt = agent_prompt(spec.note.as_deref(), &brief).unwrap_or_default();
    std::fs::write(prompt_path(spec_path), prompt).map_err(err)?;
    let _ = std::fs::remove_file(spec_path);
    let _ = herdr::name_agent_when_up(&spec.pane, &spec.agent);
    Ok(())
}

/// Where `wt prepare` leaves the agent's first prompt for the shell to pass on.
fn prompt_path(spec_path: &Path) -> PathBuf {
    spec_path.with_extension("prompt")
}

/// Opens `cwd` as a tab in the herdr workspace `space` (created when missing),
/// starts a named claude there with `claude_args` and focuses it unless `focus`
/// is false.
pub(super) fn open_agent(
    space: &str,
    cwd: &Path,
    label: &str,
    claude_args: &[&str],
    focus: bool,
    ui: &mut dyn Ui,
) -> Result<(), String> {
    if !herdr::available() {
        ui.say(format!(
            "not in herdr — run it yourself: cd {} && claude {}",
            cwd.display(),
            claude_args.join(" ")
        ));
        return Ok(());
    }
    let (tab, pane) = open_tab(space, cwd, label)?;
    let agent = free_agent_name(label);
    herdr::start_claude(&agent, &pane, claude_args).map_err(err)?;
    if focus {
        let _ = herdr::focus_tab(&tab);
    }
    ui.say(format!(
        "agent {agent} → workspace \"{space}\", tab {label}"
    ));
    Ok(())
}

/// A new tab `label` in the herdr workspace `space`, created when missing;
/// returns the tab and its pane.
fn open_tab(space: &str, cwd: &Path, label: &str) -> Result<(String, String), String> {
    let existing = herdr::list_workspaces()
        .map_err(err)?
        .into_iter()
        .find(|w| same_name(&w.label, space));
    match existing {
        Some(ws) => herdr::create_tab(&ws.workspace_id, cwd, label).map_err(err),
        None => {
            let (tab, pane) = herdr::create_workspace(space, cwd).map_err(err)?;
            herdr::rename_tab(&tab, label).map_err(err)?;
            Ok((tab, pane))
        }
    }
}

fn free_agent_name(base: &str) -> String {
    let taken: Vec<String> = herdr::list_agents()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|a| a.name)
        .collect();
    herdr::free_agent_name(base, &taken)
}

/// herdr's agents; none outside herdr.
pub(super) fn live_agents() -> Vec<herdr::Agent> {
    if !herdr::available() {
        return Vec::new();
    }
    herdr::list_agents().unwrap_or_default()
}

/// Closes what herdr shows of a removed worktree: its own workspace, otherwise
/// the tabs whose agents work in it. Failures go to `say`.
pub(super) fn close_herdr_views(path: &Path, say: &mut dyn FnMut(String)) {
    close_herdr_views_outside(path, None, say);
}

/// `close_herdr_views`, leaving workspace `kept` alone: it is closed whole
/// later.
pub(super) fn close_herdr_views_outside(
    path: &Path,
    kept: Option<&str>,
    say: &mut dyn FnMut(String),
) {
    if !herdr::available() {
        return;
    }
    let path_str = path.to_string_lossy();
    let workspaces = herdr::list_workspaces().unwrap_or_default();
    let own: Vec<_> = workspaces
        .iter()
        .filter(|ws| Some(ws.workspace_id.as_str()) != kept)
        .filter(|ws| {
            ws.worktree
                .as_ref()
                .is_some_and(|wt| wt.checkout_path == path_str)
        })
        .collect();

    if !own.is_empty() {
        for ws in own {
            if let Err(err) = herdr::close_workspace(&ws.workspace_id) {
                say(format!("can't close workspace {}: {err}", ws.label));
            }
        }
        return;
    }

    // a tab in the task's workspace: close the tabs whose agents work in the worktree
    let mut tabs: Vec<String> = live_agents()
        .into_iter()
        .filter(|a| a.runs_in(path) && Some(a.workspace_id.as_str()) != kept)
        .map(|a| a.tab_id)
        .collect();
    tabs.dedup();
    for tab in tabs {
        if let Err(err) = herdr::close_tab(&tab) {
            say(format!("can't close tab {tab}: {err}"));
        }
    }
}
