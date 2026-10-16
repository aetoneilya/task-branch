use crate::client::gitlab::{MergeRequest, MrState};
use crate::client::herdr;
use crate::client::worktree;
use crate::config::Config;
use crate::table::{self, Table, Tree, branch_finished};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, FuzzySelect};
use std::path::Path;

const REVIEW_PROMPT: &str = "\
You are a reviewer; another agent wrote the code. Review this branch's changes: \
`git diff origin/HEAD...HEAD` and uncommitted edits (`git diff HEAD`). \
Look for correctness bugs, broken contracts, branches without tests. Don't edit or commit anything. \
List findings, most severe first: file:line, the problem, a scenario where it breaks. \
If there are none, say so.";

pub(super) fn cli_review(config: &Config, name: Option<String>) -> Result<(), String> {
    if !herdr::available() {
        return Err("wt review needs herdr: run it inside a herdr pane".to_string());
    }
    // worktrees and agents only: nothing here needs Jira or GitLab
    let table = worktrees(config, false)?;
    let wt = &table.tree(resolve_tree(&table, name)?).wt;

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
    if let Some(model) = config.workflow.review_model.as_deref() {
        args.extend(["--model", model]);
    }
    args.push(REVIEW_PROMPT);
    herdr::start_claude(&reviewer, &pane, &args).map_err(|err| err.to_string())?;

    println!("Started {reviewer} in {}", wt.path.display());
    Ok(())
}

pub(super) fn cli_done(
    config: &Config,
    name: Option<String>,
    merged: bool,
    force: bool,
    yes: bool,
) -> Result<(), String> {
    let table = worktrees(config, true)?;
    // GitLab did not answer: the last known MR states will do, but with none at
    // all an open MR looks like no MR
    if let Some(warning) = table.warnings.iter().find(|w| w.starts_with("GitLab")) {
        if !force && !warning.ends_with("showing cached") {
            return Err(format!(
                "{warning} — not removing worktrees on partial data, or pass --force"
            ));
        }
        eprintln!("{warning}");
    }

    if !merged {
        let tree = table.tree(resolve_tree(&table, name)?);
        return finish_worktree(tree, mr_of(&table, tree), force, yes);
    }

    // sweep: clean worktrees whose merge request is merged or closed
    let finished: Vec<&Tree> = table.trees.iter().filter(|t| t.finished).collect();
    if finished.is_empty() {
        println!("No finished worktrees");
        return Ok(());
    }
    println!("Finished worktrees:");
    for tree in &finished {
        println!("  {}", tree.wt.name());
    }
    if !yes && !confirm("Remove all of them?")? {
        return Ok(());
    }
    for tree in finished {
        if let Err(err) = finish_worktree(tree, mr_of(&table, tree), false, true) {
            eprintln!("{}: {err}", tree.wt.name());
        }
    }
    Ok(())
}

/// Every worktree of the configured repos; `online` adds MRs, changes and
/// unpushed counts.
fn worktrees(config: &Config, online: bool) -> Result<Table, String> {
    if config.repos.is_empty() {
        return Err("no [[repo]] in ~/.config/worktable/config.toml".to_string());
    }
    // tasks don't decide which worktree is which, and are not needed here
    Ok(table::build(config, Vec::new(), online))
}

fn mr_of<'a>(table: &'a Table, tree: &Tree) -> Option<&'a MergeRequest> {
    tree.mr.map(|i| &table.mr(i).mr)
}

fn finish_worktree(
    tree: &Tree,
    mr: Option<&MergeRequest>,
    force: bool,
    yes: bool,
) -> Result<(), String> {
    let (repo, wt) = (&tree.repo, &tree.wt);
    let name = wt.name();
    let changes = match tree.changes {
        Some(changes) => changes,
        None => worktree::changed_files(&wt.path).map_err(|err| err.to_string())?,
    };
    let mr_merged = mr.is_some_and(|mr| mr.state() == MrState::Merged);
    let unpushed = tree.unpushed.unwrap_or(0);

    if !force {
        if changes > 0 {
            return Err(format!(
                "{name}: {changes} uncommitted changes — commit them or pass --force"
            ));
        }
        match mr {
            Some(mr) if mr.state() == MrState::Opened => {
                return Err(format!(
                    "{name}: MR !{} is still open — merge or close it, or pass --force",
                    mr.iid()
                ));
            }
            Some(mr) if !branch_finished(mr, unpushed, tree.head.as_deref()) => {
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

    // last: this may close the pane wt itself runs in
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

/// A worktree by name or branch, otherwise the one containing the cwd, otherwise
/// a picker. Index into `table.trees`.
fn resolve_tree(table: &Table, name: Option<String>) -> Result<usize, String> {
    let trees = &table.trees;

    if let Some(name) = name {
        return trees
            .iter()
            .position(|t| t.wt.name() == name || t.wt.branch.as_deref() == Some(name.as_str()))
            .ok_or_else(|| format!("No worktree named {name}"));
    }

    let root = std::env::current_dir()
        .ok()
        .and_then(|d| worktree::worktree_root(&d).ok());
    if let Some(i) = root.and_then(|root| trees.iter().position(|t| t.wt.path == root)) {
        return Ok(i);
    }

    if trees.is_empty() {
        return Err("No linked worktrees".to_string());
    }
    let names: Vec<String> = trees.iter().map(|t| t.wt.name()).collect();
    FuzzySelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Worktree:")
        .items(&names)
        .interact_opt()
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "Worktree wasn't selected".to_string())
}

fn live_agents() -> Vec<herdr::Agent> {
    if !herdr::available() {
        return Vec::new();
    }
    herdr::list_agents().unwrap_or_default()
}

fn confirm(prompt: &str) -> Result<bool, String> {
    Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .default(false)
        .interact()
        .map_err(|err| err.to_string())
}
