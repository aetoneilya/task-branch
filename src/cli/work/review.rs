//! `wt review`: a reviewer agent next to the task's agent.

use super::common::{err, resolve_tree, tree_table};
use super::launch::live_agents;
use crate::client::herdr;
use crate::config::Config;

const REVIEW_PROMPT: &str = "\
You are a reviewer; another agent wrote the code. Review this branch's changes: \
`git diff origin/HEAD...HEAD` and uncommitted edits (`git diff HEAD`). \
Look for correctness bugs, broken contracts, branches without tests. Don't edit or commit anything. \
List findings, most severe first: file:line, the problem, a scenario where it breaks. \
If there are none, say so.";

pub(in crate::cli) fn cli_review(config: &Config, name: Option<String>) -> Result<(), String> {
    if !herdr::available() {
        return Err("wt review needs herdr: run it inside a herdr pane".to_string());
    }
    // worktrees and agents only: nothing here needs Jira or GitLab
    let table = tree_table(config, false)?;
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
    .map_err(err)?;

    let reviewer = herdr::agent_name(&format!("{}-review", wt.name()));
    let mut args = Vec::new();
    if let Some(model) = config.workflow.review_model.as_deref() {
        args.extend(["--model", model]);
    }
    args.push(REVIEW_PROMPT);
    herdr::start_claude(&reviewer, &pane, &args).map_err(err)?;

    println!("Started {reviewer} in {}", wt.path.display());
    Ok(())
}
