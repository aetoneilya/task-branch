use super::{hyperlink, select_issue, slugify};
use crate::client::gitlab::MergeRequest;
use crate::client::herdr;
use crate::client::jira::JiraIssue;
use crate::client::worktree::{self, Worktree};
use crate::config::WorkflowConfig;
use crate::core::JiraGitlabState;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, FuzzySelect, Input};
use std::io::IsTerminal;
use std::path::Path;
use std::process::Command;

const REVIEW_PROMPT: &str = "\
Ты ревьюер, код писал другой агент. Проверь изменения этой ветки: \
`git diff origin/HEAD...HEAD` и незакоммиченные правки (`git diff HEAD`). \
Ищи баги корректности, сломанные контракты, ветки без тестов. Ничего не правь и не коммить. \
Выдай находки по убыванию важности: файл:строка, проблема, сценарий, при котором ломается. \
Если находок нет — так и скажи.";

pub(super) fn cli_start(
    state: &JiraGitlabState,
    workflow: &WorkflowConfig,
    key: Option<String>,
    name: Option<String>,
    branch: Option<String>,
    message: Option<String>,
    tab: bool,
    bg: bool,
) -> Result<(), String> {
    check_agents_limit(workflow)?;

    let issue = select_issue(state, key)?;
    // with --name given (e.g. by an agent) the branch is not asked either
    let interactive = name.is_none();
    let name = match name {
        Some(name) => name,
        None => Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Worktree name")
            .default(default_worktree_name(&issue))
            .interact_text()
            .map_err(|err| err.to_string())?,
    };

    let default_branch = format!("{}{}-{}", workflow.branch_prefix, issue.key, name);
    let branch = match branch {
        Some(branch) => branch,
        None if !interactive => default_branch,
        None => Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Branch name")
            .default(default_branch)
            .interact_text()
            .map_err(|err| err.to_string())?,
    };

    let message = match message {
        Some(message) => message,
        None if !interactive => String::new(),
        None => Input::with_theme(&ColorfulTheme::default())
            .with_prompt("What should the agent do (empty: implement the issue)")
            .allow_empty(true)
            .interact_text()
            .map_err(|err| err.to_string())?,
    };

    let repo = worktree::main_checkout(state.repo_path()).map_err(|err| err.to_string())?;
    let mut wt = Command::new("wt");
    wt.current_dir(&repo).arg(&name).arg(&branch);
    if tab {
        wt.arg("--tab");
    }
    if bg {
        wt.arg("--bg");
    }
    wt.arg("--").arg(task_prompt(&issue, &message));

    let status = wt
        .status()
        .map_err(|err| format!("failed to run wt: {err}"))?;
    if !status.success() {
        return Err(format!("wt exited with {status}"));
    }
    Ok(())
}

pub(super) fn cli_status(state: &JiraGitlabState) -> Result<(), String> {
    let repo = worktree::main_checkout(state.repo_path()).map_err(|err| err.to_string())?;
    let worktrees = worktree::linked_worktrees(&repo).map_err(|err| err.to_string())?;
    let agents = live_agents();

    // git status and the GitLab lookup take ~0.5s per worktree in a big repo
    let rows: Vec<[String; 5]> = std::thread::scope(|scope| {
        let handles: Vec<_> = worktrees
            .iter()
            .map(|wt| scope.spawn(|| status_row(state, wt, &agents)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let header = ["WORKTREE", "BRANCH", "LOCAL", "AGENT", "MR"].map(String::from);
    let mut widths = [0usize; 4];
    for row in std::iter::once(&header).chain(&rows) {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    for row in std::iter::once(&header).chain(&rows) {
        println!(
            "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {}",
            row[0],
            row[1],
            row[2],
            row[3],
            row[4],
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
            w3 = widths[3],
        );
    }
    Ok(())
}

fn status_row(state: &JiraGitlabState, wt: &Worktree, agents: &[herdr::Agent]) -> [String; 5] {
    let changes = worktree::changed_files(&wt.path).unwrap_or(0);
    let unpushed = wt
        .branch
        .as_deref()
        .and_then(|b| worktree::unpushed_commits(&wt.path, b).ok())
        .unwrap_or(0);
    let local = match (changes, unpushed) {
        (0, 0) => "clean".to_string(),
        (c, 0) => format!("{c} changed"),
        (0, u) => format!("{u} unpushed"),
        (c, u) => format!("{c} changed, {u} unpushed"),
    };
    let agent = agents
        .iter()
        .filter(|a| a.runs_in(&wt.path))
        .map(herdr::Agent::label)
        .collect::<Vec<_>>()
        .join(",");
    let mr = latest_mr(state, wt);
    let mr_text = match &mr {
        Some(mr) if changes == 0 && is_finished(wt, mr, unpushed) => {
            format!("{}  → jg done", display_mr_short(mr))
        }
        Some(mr) => display_mr_short(mr),
        None => String::new(),
    };

    [
        wt.name(),
        wt.branch.clone().unwrap_or_else(|| "(detached)".to_string()),
        local,
        if agent.is_empty() { "-".to_string() } else { agent },
        mr_text,
    ]
}

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
            latest_mr(state, wt).is_some_and(|mr| is_finished(wt, &mr, unpushed))
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
            Some(mr) if !is_finished(wt, mr, unpushed) => {
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
    if !yes && !confirm(&format!("Remove {name} (branch {branch}) and its herdr tabs?"))? {
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

fn close_herdr_views(path: &Path) {
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

fn check_agents_limit(workflow: &WorkflowConfig) -> Result<(), String> {
    let live = live_agents().len();
    if live < workflow.max_agents {
        return Ok(());
    }
    let prompt = format!(
        "{live} agents are already live (limit {}). Start another anyway?",
        workflow.max_agents
    );
    if !std::io::stdin().is_terminal() {
        eprintln!("warning: {live} agents are already live (limit {})", workflow.max_agents);
        return Ok(());
    }
    if confirm(&prompt)? {
        Ok(())
    } else {
        Err("Finish or close a task first (jg status, jg done)".to_string())
    }
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

/// MR merged or closed, and the branch has nothing beyond it: either every commit
/// is on origin, or the local tip is the MR head (squash merges leave the branch
/// commits off master, and the remote branch is usually deleted).
fn is_finished(wt: &Worktree, mr: &MergeRequest, unpushed: usize) -> bool {
    if mr.state() == "opened" {
        return false;
    }
    if unpushed == 0 {
        return true;
    }
    let head = wt
        .branch
        .as_deref()
        .and_then(|b| worktree::branch_head(&wt.path, b).ok());
    head.is_some() && head.as_deref() == mr.sha()
}

fn display_mr_short(mr: &MergeRequest) -> String {
    hyperlink(mr.web_url(), &format!("!{} {}", mr.iid(), mr.state()))
}

fn default_worktree_name(issue: &JiraIssue) -> String {
    let slug = slugify(&issue.fields.summary);
    let short: Vec<&str> = slug.split('-').filter(|w| !w.is_empty()).take(4).collect();
    if short.is_empty() {
        issue.key.to_lowercase()
    } else {
        short.join("-")
    }
}

fn task_prompt(issue: &JiraIssue, message: &str) -> String {
    let mut prompt = format!("{}: {}\n", issue.key, issue.fields.summary);
    if let Some(description) = issue.fields.description.as_deref() {
        prompt.push_str(&format!("\n{}\n", description.trim()));
    }
    let message = message.trim();
    prompt.push_str(&format!(
        "\n{}",
        if message.is_empty() {
            "Реализуй задачу."
        } else {
            message
        }
    ));
    prompt
}

fn confirm(prompt: &str) -> Result<bool, String> {
    Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .default(false)
        .interact()
        .map_err(|err| err.to_string())
}
