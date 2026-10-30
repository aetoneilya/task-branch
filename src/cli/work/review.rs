//! `wt review`: a reviewer agent next to the task's agent.

use super::common::{Terminal, Ui, err, resolve_tree, tree_table};
use super::launch::live_agents;
use crate::client::agent::AgentKind;
use crate::client::herdr;
use crate::config::Config;

const REVIEW_PROMPT: &str = "\
You are a reviewer; another agent wrote the code. Review this branch's changes: \
`git diff origin/HEAD...HEAD` and uncommitted edits (`git diff HEAD`). \
Look for correctness bugs, broken contracts, branches without tests. Don't edit or commit anything. \
List findings, most severe first: file:line, the problem, a scenario where it breaks. \
If there are none, say so.";

pub(in crate::cli) fn cli_review(config: &Config, name: Option<String>) -> Result<(), String> {
    review(config, name, &mut Terminal)
}

/// A reviewer agent on the worktree, under the task's agent or in a tab of its own.
pub(crate) fn review(config: &Config, name: Option<String>, ui: &mut dyn Ui) -> Result<(), String> {
    if !herdr::available() {
        return Err("wt review needs herdr: run it inside a herdr pane".to_string());
    }
    // worktrees and agents only: nothing here needs Jira or GitLab
    let table = tree_table(config, false)?;
    let wt = &table.tree(resolve_tree(&table, name, ui)?).wt;

    let task_agent = live_agents().into_iter().find(|a| a.runs_in(&wt.path));
    let pane = match &task_agent {
        Some(agent) => herdr::split_pane(&agent.pane_id, "down", &wt.path),
        None => {
            let workspace = std::env::var("HERDR_WORKSPACE_ID").unwrap_or_default();
            herdr::create_tab(&workspace, &wt.path, &format!("{}-review", wt.name()))
                .map(|(_, pane)| pane)
        }
    }
    .map_err(err)?;

    let reviewer = herdr::agent_name(&format!("{}-review", wt.name()));
    let kind = config
        .workflow
        .agent_override
        .or(config.workflow.review_agent)
        .unwrap_or(config.workflow.agent);
    let model = match kind {
        AgentKind::Claude => &config.workflow.review_model,
        AgentKind::Codex => &config.workflow.codex_review_model,
    };
    let mut args = Vec::new();
    if let Some(model) = model.as_deref() {
        args.extend(["--model", model]);
    }
    args.push(REVIEW_PROMPT);
    herdr::start_agent(kind, &reviewer, &pane, &args).map_err(err)?;

    ui.say(format!("Started {reviewer} in {}", wt.path.display()));
    Ok(())
}
