use crate::client::gitlab::MergeRequest;
use crate::client::herdr;
use crate::client::worktree::{self, Worktree};
use crate::config::WorkflowConfig;
use crate::core::JiraGitlabState;
use crate::table::branch_finished;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, FuzzySelect};
use std::path::Path;

const REVIEW_PROMPT: &str = "\
You are a reviewer; another agent wrote the code. Review this branch's changes: \
`git diff origin/HEAD...HEAD` and uncommitted edits (`git diff HEAD`). \
Look for correctness bugs, broken contracts, branches without tests. Don't edit or commit anything. \
List findings, most severe first: file:line, the problem, a scenario where it breaks. \
If there are none, say so.";

pub(super) fn cli_review(
    state: &JiraGitlabState,
    workflow: &WorkflowConfig,
    name: Option<String>,
) -> Result<(), String> {
    if !herdr::available() {
        return Err("jg review needs herdr: run it inside a herdr pane".to_string());
    }
    let repo = worktree::main_checkout(state.repo_path()).map_err(|err| err.to_string())?;
    let wt = resolve_worktree(state, &repo, name)?;

    let task_agent = live_agents().into_iter().find(|a| a.runs_in(&wt.path));
    let pane = match &task_agent {
        Some(agent) => herdr::split_pane(&agent.pane_id, "down", &wt.path),
        None => {
            let workspace = std::env::var("HERDR_WORKSPACE_ID").unwrap_or_default();
            herdr::create_tab(&workspace, &wt.path, &format!("{}-review", wt.name()))
                .map(|(_, pane)| pane)
        }
    }
    .map_err(|err| err.to_string())?;

    let reviewer = herdr::agent_name(&format!("{}-review", wt.name()));
    let mut args = Vec::new();
    if let Some(model) = workflow.review_model.as_deref() {
        args.extend(["--model", model]);
    }
    args.push(REVIEW_PROMPT);
    herdr::start_claude(&reviewer, &pane, &args).map_err(|err| err.to_string())?;

    println!("Started {reviewer} in {}", wt.path.display());
    Ok(())
}

pub(super) fn cli_done(
    state: &JiraGitlabState,
    name: Option<String>,
    merged: bool,
    force: bool,
    yes: bool,
) -> Result<(), String> {
    let repo = worktree::main_checkout(state.repo_path()).map_err(|err| err.to_string())?;

    if !merged {
        let wt = resolve_worktree(state, &repo, name)?;
        return finish_worktree(state, &repo, &wt, force, yes);
    }

    // sweep: clean worktrees whose merge request is merged or closed
    let finished: Vec<Worktree> = worktree::linked_worktrees(&repo)
        .map_err(|err| err.to_string())?
        .into_iter()
        .filter(|wt| worktree::changed_files(&wt.path).is_ok_and(|c| c == 0))
        .filter(|wt| {
            let unpushed = wt
                .branch
                .as_deref()
                .and_then(|b| worktree::unpushed_commits(&repo, b).ok())
                .unwrap_or(0);
            latest_mr(state, wt).is_some_and(|mr| branch_finished(wt, &mr, unpushed))
        })
        .collect();

    if finished.is_empty() {
        println!("No finished worktrees");
        return Ok(());
    }
    println!("Finished worktrees:");
    for wt in &finished {
        println!("  {}", wt.name());
    }
    if !yes && !confirm("Remove all of them?")? {
        return Ok(());
    }
    for wt in &finished {
        if let Err(err) = finish_worktree(state, &repo, wt, false, true) {
            eprintln!("{}: {err}", wt.name());
        }
    }
    Ok(())
}

fn finish_worktree(
    state: &JiraGitlabState,
    repo: &Path,
    wt: &Worktree,
    force: bool,
    yes: bool,
) -> Result<(), String> {
    let name = wt.name();
    let changes = worktree::changed_files(&wt.path).map_err(|err| err.to_string())?;
    let mr = latest_mr(state, wt);
    let mr_merged = mr.as_ref().is_some_and(|mr| mr.state() == "merged");
    let unpushed = wt
        .branch
        .as_deref()
        .and_then(|b| worktree::unpushed_commits(repo, b).ok())
        .unwrap_or(0);

    if !force {
        if changes > 0 {
            return Err(format!(
                "{name}: {changes} uncommitted changes — commit them or pass --force"
            ));
        }
        match &mr {
            Some(mr) if mr.state() == "opened" => {
                return Err(format!(
                    "{name}: MR !{} is still open — merge or close it, or pass --force",
                    mr.iid()
                ));
            }
            Some(mr) if !branch_finished(wt, mr, unpushed) => {
                return Err(format!(
                    "{name}: the branch has commits after MR !{} — push them or pass --force",
                    mr.iid()
                ));
            }
            None => {
                if unpushed > 0 {
                    return Err(format!(
                        "{name}: {unpushed} commits are not pushed and there is no MR — push them or pass --force"
                    ));
                }
            }
            _ => {}
        }
    }

    let branch = wt.branch.as_deref().unwrap_or("(detached)");
    if !yes
        && !confirm(&format!(
            "Remove {name} (branch {branch}) and its herdr tabs?"
        ))?
    {
        return Ok(());
    }

    worktree::remove_worktree(repo, &wt.path, force).map_err(|err| err.to_string())?;
    if let Some(branch) = &wt.branch {
        // -d compares with the local master, which is usually stale, and refuses
        // squash-merged branches; nothing is lost when every commit is on origin
        let nothing_to_lose = mr_merged || unpushed == 0;
        if let Err(err) = worktree::delete_branch(repo, branch, force || nothing_to_lose) {
            eprintln!("{name}: branch {branch} kept: {err}");
        }
    }
    println!("Removed {name}");

    // last: this may close the pane jg itself runs in
    close_herdr_views(&wt.path);
    Ok(())
}

pub(super) fn close_herdr_views(path: &Path) {
    if !herdr::available() {
        return;
    }
    let path_str = path.to_string_lossy();
    let workspaces = herdr::list_workspaces().unwrap_or_default();
    let own: Vec<_> = workspaces
        .iter()
        .filter(|ws| {
            ws.worktree
                .as_ref()
                .is_some_and(|wt| wt.checkout_path == path_str)
        })
        .collect();

    if !own.is_empty() {
        for ws in own {
            if let Err(err) = herdr::close_workspace(&ws.workspace_id) {
                eprintln!("can't close workspace {}: {err}", ws.label);
            }
        }
        return;
    }

    // opened with --tab: close the tabs whose agents work in the worktree
    let mut tabs: Vec<String> = live_agents()
        .into_iter()
        .filter(|a| a.runs_in(path))
        .map(|a| a.tab_id)
        .collect();
    tabs.dedup();
    for tab in tabs {
        if let Err(err) = herdr::close_tab(&tab) {
            eprintln!("can't close tab {tab}: {err}");
        }
    }
}

fn resolve_worktree(
    state: &JiraGitlabState,
    repo: &Path,
    name: Option<String>,
) -> Result<Worktree, String> {
    let mut worktrees = worktree::linked_worktrees(repo).map_err(|err| err.to_string())?;

    if let Some(name) = name {
        return worktrees
            .into_iter()
            .find(|wt| wt.name() == name || wt.branch.as_deref() == Some(name.as_str()))
            .ok_or_else(|| format!("No worktree named {name}"));
    }

    if let Ok(root) = worktree::worktree_root(state.repo_path()) {
        if let Some(pos) = worktrees.iter().position(|wt| wt.path == root) {
            return Ok(worktrees.swap_remove(pos));
        }
    }

    if worktrees.is_empty() {
        return Err("No linked worktrees".to_string());
    }
    let names: Vec<String> = worktrees.iter().map(Worktree::name).collect();
    let selection = FuzzySelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Worktree:")
        .items(&names)
        .interact_opt()
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "Worktree wasn't selected".to_string())?;
    Ok(worktrees.swap_remove(selection))
}

fn live_agents() -> Vec<herdr::Agent> {
    if !herdr::available() {
        return Vec::new();
    }
    herdr::list_agents().unwrap_or_default()
}

fn latest_mr(state: &JiraGitlabState, wt: &Worktree) -> Option<MergeRequest> {
    let branch = wt.branch.as_deref()?;
    state
        .find_merge_requests_by_branch(branch)
        .ok()?
        .into_iter()
        .next()
}

fn confirm(prompt: &str) -> Result<bool, String> {
    Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .default(false)
        .interact()
        .map_err(|err| err.to_string())
}
