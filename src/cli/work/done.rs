//! `wt done` and `wt attach`: closing a task or worktrees, attaching a ticket.

use super::common::{
    Terminal, Ui, confirm, err, find_task, is_ticket_key, list, resolve_task, resolve_tree,
    tree_table,
};
use super::launch::close_herdr_views;
use crate::cli::DoneArgs;
use crate::cli::load;
use crate::client::gitlab::{MergeRequest, MrState};
use crate::client::herdr;
use crate::client::jira::{JiraClient, Transition};
use crate::client::worktree;
use crate::config::Config;
use crate::table::{self, Fresh, Table, Tree, branch_finished};
use crate::vault::note::Note;
use crate::vault::task::Task;
use serde_yaml::Value;

/// `wt done`: with the name of an open task, or a ticket of one, closes the task;
/// otherwise removes worktrees.
pub(in crate::cli) fn cli_done(config: &Config, args: DoneArgs) -> Result<(), String> {
    if let Some(name) = args.name.as_deref().filter(|_| !args.merged)
        && let Ok(tasks) = load(config)
        && let Some(i) = find_task(&tasks, name, false)?
        && !tasks[i].done
    {
        let plan = plan_task(config, tasks, name, args.force)?;
        if !plan.blockers.is_empty() {
            return Err(not_finished(plan.name(), &plan.blockers));
        }
        println!("Closing task \"{}\":", plan.name());
        for step in plan.steps() {
            println!("  {step}");
        }
        if !args.yes && !confirm(plan.question())? {
            return Ok(());
        }
        return finish_task(config, plan, args.force, &mut print);
    }
    done_trees(config, args)
}

fn print(line: String) {
    println!("{line}");
}

/// What `wt done <task>` would do, checked against fresh Jira and GitLab: the
/// task's worktrees go (each must be finished: MR merged or closed, nothing
/// unpushed; uncommitted changes are lost once confirmed), my open tickets of
/// it are closed in Jira, and the note gets `done: true` with the final MRs and
/// branches — after that worktable leaves it alone.
pub(crate) struct TaskPlan {
    table: Table,
    row: usize,
    /// What still holds the task open; none with force.
    pub(crate) blockers: Vec<String>,
    /// Worktrees with uncommitted changes, and how many: they go with them.
    dirty: Vec<(usize, usize)>,
    to_close: Vec<String>,
    left_open: Vec<String>,
}

impl TaskPlan {
    pub(crate) fn name(&self) -> &str {
        &self.table.tasks[self.row].task.name
    }

    /// One line per thing that will happen.
    pub(crate) fn steps(&self) -> Vec<String> {
        let row = &self.table.tasks[self.row];
        let mut steps: Vec<String> = row
            .trees
            .iter()
            .map(|&t| {
                let name = self.table.tree(t).wt.name();
                match self.changes(t) {
                    Some(n) => {
                        format!("! remove worktree {name}, losing {n} uncommitted change(s)")
                    }
                    None => format!("remove worktree {name}"),
                }
            })
            .collect();
        steps.extend(
            row.agents
                .iter()
                .map(|a| format!("close the pane of agent {}", a.name)),
        );
        steps.extend(self.to_close.iter().map(|k| format!("close {k} in Jira")));
        steps.extend(
            self.left_open
                .iter()
                .map(|k| format!("{k} is not assigned to me — left open")),
        );
        steps.push("mark the note done".to_string());
        steps
    }

    /// Asked before the plan runs.
    pub(crate) fn question(&self) -> &'static str {
        if self.dirty.is_empty() {
            "Close it?"
        } else {
            "Close it, losing the uncommitted changes?"
        }
    }

    /// Uncommitted changes of worktree `tree`, if it has any.
    fn changes(&self, tree: usize) -> Option<usize> {
        self.dirty.iter().find(|(t, _)| *t == tree).map(|&(_, n)| n)
    }
}

/// `tasks` from the vault, `name` an open one of them by name or ticket.
pub(crate) fn plan_task(
    config: &Config,
    tasks: Vec<Task>,
    name: &str,
    force: bool,
) -> Result<TaskPlan, String> {
    let table = table::build(config, tasks, true);
    if let Some(warning) = [&table.jira, &table.gitlab]
        .into_iter()
        .find_map(|source| source.warning.as_ref())
    {
        return Err(format!("{warning} — not closing a task on partial data"));
    }
    let row = find_task(table.tasks.iter().map(|r| &r.task), name, false)?
        .filter(|&i| !table.tasks[i].task.done)
        .ok_or_else(|| format!("no open task \"{name}\""))?;
    let task = &table.tasks[row];

    let mut blockers = Vec::new();
    let mut dirty = Vec::new();
    if !force {
        for &t in &task.trees {
            let tree = table.tree(t);
            blockers.extend(held(&table, tree));
            match uncommitted(tree)? {
                0 => {}
                n => dirty.push((t, n)),
            }
        }
        let held: Vec<usize> = task
            .trees
            .iter()
            .filter_map(|&t| table.tree(t).mr)
            .collect();
        for &m in task.mrs.iter().filter(|m| !held.contains(m)) {
            if table.mr(m).opened() {
                blockers.push(format!("MR !{} is open", table.mr(m).mr.iid()));
            }
        }
    }

    let open_tickets = |mine: bool| -> Vec<String> {
        task.task
            .tickets
            .iter()
            .chain(&task.found_tickets)
            .filter(|k| {
                table
                    .tickets
                    .get(*k)
                    .is_some_and(|t| t.mine == mine && !t.closed())
            })
            .cloned()
            .collect()
    };
    let (to_close, left_open) = (open_tickets(true), open_tickets(false));
    Ok(TaskPlan {
        table,
        row,
        blockers,
        dirty,
        to_close,
        left_open,
    })
}

/// Does what the plan says, reporting each step to `say`.
pub(crate) fn finish_task(
    config: &Config,
    plan: TaskPlan,
    force: bool,
    say: &mut dyn FnMut(String),
) -> Result<(), String> {
    let TaskPlan {
        table,
        row,
        dirty,
        to_close,
        ..
    } = plan;
    let row = &table.tasks[row];
    // before anything goes: without Jira the task would end half-closed
    let jira = if to_close.is_empty() {
        None
    } else {
        Some(JiraClient::from_env(&config.jira.host)?)
    };

    let mut problems = Vec::new();
    for &t in &row.trees {
        let tree = table.tree(t);
        let discard = dirty.iter().any(|&(d, _)| d == t);
        match remove_tree(tree, mr_of(&table, tree), force, discard, say) {
            Ok(kept) => problems.extend(kept),
            Err(e) => problems.push(format!("{}: {e}", tree.wt.name())),
        }
    }

    if let Some(jira) = &jira {
        for key in &to_close {
            match close_in_jira(jira, key) {
                Ok(status) => say(format!("{key} → {status}")),
                Err(e) => problems.push(format!("{key}: {e}")),
            }
        }
    }

    let mrs: Vec<Value> = row
        .mrs
        .iter()
        .map(|&m| Value::String(table.mr(m).mr.web_url().to_string()))
        .collect();
    let mut branches: Vec<String> = row
        .mrs
        .iter()
        .map(|&m| table.mr(m).mr.source_branch().to_string())
        .collect();
    for &t in &row.trees {
        if let Some(b) = &table.tree(t).wt.branch
            && !branches.contains(b)
        {
            branches.push(b.clone());
        }
    }
    {
        let _lock = crate::vault::lock().map_err(err)?;
        let mut note = Note::read(&row.task.note.path).map_err(err)?;
        note.set("done", Value::Bool(true));
        note.set(
            "closed",
            Value::String(chrono::Local::now().format("%Y-%m-%d").to_string()),
        );
        if !mrs.is_empty() {
            note.set("mrs", Value::Sequence(mrs));
        }
        if !branches.is_empty() {
            note.set("branches", list(&branches));
        }
        note.save().map_err(err)?;
    }
    say(format!("\"{}\" is done", row.task.name));

    // last: this may close the tab wt itself runs in
    for &t in &row.trees {
        close_herdr_views(&table.tree(t).wt.path, &mut |e| problems.push(e));
    }
    close_agent_panes(row, &mut |e| problems.push(e));
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!("done, but:\n  {}", problems.join("\n  ")))
    }
}

/// Closes the panes of the task's agents outside its worktrees; the pane wt
/// itself runs in goes last.
fn close_agent_panes(row: &table::TaskRow, say: &mut dyn FnMut(String)) {
    if row.agents.is_empty() || !herdr::available() {
        return;
    }
    let own = std::env::var("HERDR_PANE_ID").ok();
    let mut panes: Vec<&str> = row.agents.iter().map(|a| a.pane.as_str()).collect();
    panes.sort_by_key(|p| own.as_deref() == Some(*p));
    for pane in panes {
        if let Err(e) = herdr::close_pane(pane) {
            say(format!("can't close pane {pane}: {e}"));
        }
    }
}

/// `wt done [<worktree>]` and `wt done --merged`: worktrees go one by one, each
/// once it is finished.
fn done_trees(config: &Config, args: DoneArgs) -> Result<(), String> {
    let (table, warning) = trees_for_done(config, args.force)?;
    if let Some(warning) = warning {
        eprintln!("{warning}");
    }

    if !args.merged {
        let tree = resolve_tree(&table, args.name, &mut Terminal)?;
        let plan = TreePlan::new(table, tree, args.force)?;
        if !plan.blockers.is_empty() {
            return Err(not_finished(&plan.name(), &plan.blockers));
        }
        if !args.yes && !confirm(&format!("{}?", plan.step()))? {
            return Ok(());
        }
        return finish_tree(plan, args.force, &mut print);
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
    if !args.yes && !confirm("Remove all of them?")? {
        return Ok(());
    }
    let mut removed = Vec::new();
    for tree in finished {
        let name = tree.wt.name();
        let result = tree_blockers(&table, tree).and_then(|blockers| {
            if blockers.is_empty() {
                remove_tree(tree, mr_of(&table, tree), false, false, &mut print)
            } else {
                Err(not_finished(&name, &blockers))
            }
        });
        match result {
            Ok(kept) => {
                kept.into_iter().for_each(|k| eprintln!("{k}"));
                removed.push(tree);
            }
            Err(e) => eprintln!("{name}: {e}"),
        }
    }
    // last: this may close the pane wt itself runs in
    for tree in removed {
        close_herdr_views(&tree.wt.path, &mut |e| eprintln!("{e}"));
    }
    Ok(())
}

/// Worktrees with their MRs, and what to warn about. GitLab did not answer: the
/// last known MR states will do, but with none at all an open MR looks like no
/// MR, so that takes force.
pub(crate) fn trees_for_done(
    config: &Config,
    force: bool,
) -> Result<(Table, Option<String>), String> {
    let table = tree_table(config, true)?;
    let warning = table.gitlab.warning.clone();
    if let Some(warning) = &warning
        && !force
        && table.gitlab.fresh == Fresh::Missing
    {
        return Err(format!(
            "{warning} — not removing worktrees on partial data, or pass --force"
        ));
    }
    Ok((table, warning))
}

/// What `wt done <worktree>` would do: the worktree goes, with its branch and
/// herdr tabs, once nothing holds it.
pub(crate) struct TreePlan {
    table: Table,
    tree: usize,
    /// What still holds the worktree; none with force.
    pub(crate) blockers: Vec<String>,
}

impl TreePlan {
    /// `tree` indexes `table.trees`.
    pub(crate) fn new(table: Table, tree: usize, force: bool) -> Result<TreePlan, String> {
        let blockers = if force {
            Vec::new()
        } else {
            tree_blockers(&table, table.tree(tree))?
        };
        Ok(TreePlan {
            table,
            tree,
            blockers,
        })
    }

    pub(crate) fn name(&self) -> String {
        self.table.tree(self.tree).wt.name()
    }

    pub(crate) fn step(&self) -> String {
        let wt = &self.table.tree(self.tree).wt;
        let branch = wt.branch.as_deref().unwrap_or("(detached)");
        format!("Remove {} (branch {branch}) and its herdr tabs", wt.name())
    }
}

pub(crate) fn finish_tree(
    plan: TreePlan,
    force: bool,
    say: &mut dyn FnMut(String),
) -> Result<(), String> {
    let tree = plan.table.tree(plan.tree);
    if let Some(kept) = remove_tree(tree, mr_of(&plan.table, tree), force, false, say)? {
        say(kept);
    }
    // last: this may close the pane wt itself runs in
    close_herdr_views(&tree.wt.path, say);
    Ok(())
}

/// What keeps a worktree from being removed: uncommitted changes, and what
/// `held` names.
fn tree_blockers(table: &Table, tree: &Tree) -> Result<Vec<String>, String> {
    let mut blockers = Vec::new();
    match uncommitted(tree)? {
        0 => {}
        n => blockers.push(format!("{}: {n} uncommitted change(s)", tree.wt.name())),
    }
    blockers.extend(held(table, tree));
    Ok(blockers)
}

fn uncommitted(tree: &Tree) -> Result<usize, String> {
    match tree.changes {
        Some(changes) => Ok(changes),
        None => worktree::changed_files(&tree.wt.path).map_err(err),
    }
}

/// What holds a worktree beyond its changes: an open MR, commits that are
/// nowhere but here or may be.
fn held(table: &Table, tree: &Tree) -> Vec<String> {
    let wt = tree.wt.name();
    let mut blockers = Vec::new();
    match (mr_of(table, tree), tree.unpushed) {
        (Some(mr), _) if mr.state() == MrState::Opened => {
            blockers.push(format!("{wt}: MR !{} is open", mr.iid()))
        }
        // git could not tell: they may be the only copy, MR or not
        (_, None) => blockers.push(format!("{wt}: unpushed commits unknown")),
        (Some(mr), Some(n)) if !branch_finished(mr, n, tree.head.as_deref()) => {
            blockers.push(format!("{wt}: commits after MR !{}", mr.iid()))
        }
        (None, Some(n)) if n > 0 => blockers.push(format!("{wt}: unpushed commits and no MR")),
        _ => {}
    }
    blockers
}

fn not_finished(name: &str, blockers: &[String]) -> String {
    format!(
        "\"{name}\" is not finished:\n  {}\nfinish these, or pass --force",
        blockers.join("\n  ")
    )
}

fn mr_of<'a>(table: &'a Table, tree: &Tree) -> Option<&'a MergeRequest> {
    tree.mr.map(|i| &table.mr(i).mr)
}

/// Removes the worktree and deletes its branch; returns why the branch was kept,
/// when it was. `discard`: its uncommitted changes go with it, the branch
/// still only when nothing is lost.
fn remove_tree(
    tree: &Tree,
    mr: Option<&MergeRequest>,
    force: bool,
    discard: bool,
    say: &mut dyn FnMut(String),
) -> Result<Option<String>, String> {
    let name = tree.wt.name();
    let main = worktree::main_checkout(&tree.repo).map_err(err)?;
    worktree::remove_worktree(&main, &tree.wt.path, force || discard).map_err(err)?;
    say(format!("removed {name}"));
    let Some(branch) = &tree.wt.branch else {
        return Ok(None);
    };
    Ok(
        worktree::delete_branch(&main, branch, force || nothing_to_lose(tree, mr))
            .err()
            .map(|e| format!("{name}: branch {branch} kept: {e}")),
    )
}

/// Whether the branch may go though `git branch -d` refuses it: -d compares with
/// the local master, which is usually stale, and refuses squash-merged branches.
/// Nothing is lost when the MR is merged or every commit is on origin — not
/// when git could not count them.
fn nothing_to_lose(tree: &Tree, mr: Option<&MergeRequest>) -> bool {
    mr.is_some_and(|mr| mr.state() == MrState::Merged) || tree.unpushed == Some(0)
}

/// Moves the ticket to a "closed"-like status of its workflow.
fn close_in_jira(jira: &JiraClient, key: &str) -> Result<String, String> {
    let transitions = jira.transitions(key).map_err(err)?;
    let chosen =
        closing_transition(&transitions).ok_or("no closing transition from its current status")?;
    jira.transition(key, &chosen.id).map_err(err)?;
    Ok(chosen.to.name.clone())
}

/// A done-category transition, preferring "closed"/"done"/"resolved", never one
/// that cancels or declines.
fn closing_transition(transitions: &[Transition]) -> Option<&Transition> {
    let refused = ["cancel", "declin", "reject", "won't", "отмен", "отклон"];
    let preferred = ["close", "done", "resolve", "закры", "готов", "решен"];
    let name = |t: &Transition| format!("{} {}", t.name, t.to.name).to_lowercase();
    let usable: Vec<_> = transitions
        .iter()
        .filter(|t| t.to.status_category.key == table::Category::Done)
        .filter(|t| !refused.iter().any(|r| name(t).contains(r)))
        .collect();
    usable
        .iter()
        .find(|t| preferred.iter().any(|p| name(t).contains(p)))
        .or(usable.first())
        .copied()
}

pub(in crate::cli) fn cli_attach(
    config: &Config,
    key: &str,
    task: Option<String>,
) -> Result<(), String> {
    attach(config, key, task, &mut Terminal)
}

/// `wt attach <KEY> [<task>]`: the ticket joins a task — just its `tickets:`,
/// no worktree or agent. Without a task, a picker over the open ones.
pub(crate) fn attach(
    config: &Config,
    key: &str,
    task: Option<String>,
    ui: &mut dyn Ui,
) -> Result<(), String> {
    if !is_ticket_key(key) {
        return Err(format!("\"{key}\" is not a ticket key"));
    }
    let tasks = load(config)?;
    if let Some(i) = find_task(&tasks, key, false)? {
        return Err(format!("{key} is already in \"{}\"", tasks[i].name));
    }
    let mut task = match task {
        Some(name) => resolve_task(config, tasks, Some(&name), ui)?,
        None => {
            let mut table = table::build(config, tasks, false);
            let open: Vec<usize> = (0..table.tasks.len())
                .filter(|&i| !table.tasks[i].task.done)
                .collect();
            let items: Vec<String> = open
                .iter()
                .map(|&i| table.tasks[i].task.name.clone())
                .collect();
            let chosen = open[ui.pick(&format!("Attach {key} to"), &items)?];
            table.tasks.swap_remove(chosen).task
        }
    };
    let _lock = crate::vault::lock().map_err(err)?;
    task.note.reload().map_err(err)?;
    let mut entries = task.note.strings("tickets");
    entries.push(key.to_string());
    task.note.set("tickets", list(&entries));
    task.note.save().map_err(err)?;
    ui.say(format!("{key} attached to \"{}\"", task.name));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{TaskPlan, closing_transition, nothing_to_lose, tree_blockers};
    use crate::client::jira::Transition;
    use crate::table::fixtures::{mr, table, task, tree};
    use crate::table::{Agent, AgentStatus};

    #[test]
    fn closing_a_task_closes_the_panes_of_its_agents() {
        let mut table = table(vec![task("video", &[])], vec![], vec![], vec![]);
        table.tasks[0].agents.push(Agent {
            name: "research".into(),
            pane: "w1:p2".into(),
            tab: "w1:t1".into(),
            status: AgentStatus::Idle,
            workspace: "w1".into(),
            active: None,
        });
        let plan = TaskPlan {
            table,
            row: 0,
            blockers: vec![],
            dirty: vec![],
            to_close: vec![],
            left_open: vec![],
        };
        assert!(
            plan.steps()
                .contains(&"close the pane of agent research".to_string())
        );
    }

    #[test]
    fn uncommitted_changes_are_lost_once_confirmed() {
        let mut busy = tree("busy", "i.udalov/UCP-1-a");
        busy.changes = Some(3);
        let mut table = table(vec![task("video", &[])], vec![], vec![], vec![busy]);
        table.tasks[0].trees = vec![0];
        let plan = TaskPlan {
            table,
            row: 0,
            blockers: vec![],
            dirty: vec![(0, 3)],
            to_close: vec![],
            left_open: vec![],
        };
        assert_eq!(
            plan.steps()[0],
            "! remove worktree busy, losing 3 uncommitted change(s)"
        );
        assert_eq!(plan.question(), "Close it, losing the uncommitted changes?");
    }

    #[test]
    fn unknown_unpushed_commits_hold_a_worktree() {
        let mut unknown = tree("unknown", "i.udalov/UCP-1-a");
        unknown.unpushed = None;
        let mut ahead = tree("ahead", "i.udalov/UCP-2-b");
        ahead.unpushed = Some(2);
        let table = table(
            vec![],
            vec![],
            vec![mr(1, "i.udalov/UCP-1-a", "x", "closed")],
            vec![unknown, ahead, tree("pushed", "i.udalov/UCP-3-c")],
        );
        let blockers =
            |name: &str| tree_blockers(&table, table.tree(table.tree_index(name))).unwrap();
        // with an MR or without one
        assert_eq!(
            blockers("unknown"),
            vec!["unknown: unpushed commits unknown"]
        );
        assert_eq!(blockers("ahead"), vec!["ahead: unpushed commits and no MR"]);
        assert!(blockers("pushed").is_empty());

        let unknown = table.tree(table.tree_index("unknown"));
        assert!(!nothing_to_lose(unknown, None));
        let closed = &table.mr(table.mr_index(1)).mr;
        assert!(!nothing_to_lose(unknown, Some(closed)));
        assert!(nothing_to_lose(
            table.tree(table.tree_index("pushed")),
            None
        ));
    }

    #[test]
    fn closes_rather_than_cancels() {
        let transitions: Vec<Transition> = serde_json::from_str(
            r#"[
              {"id":"601","name":"Cancelled","to":{"name":"Cancelled","statusCategory":{"key":"done"}}},
              {"id":"521","name":"In Progress","to":{"name":"В работе","statusCategory":{"key":"indeterminate"}}},
              {"id":"381","name":"Closed","to":{"name":"Закрыт","statusCategory":{"key":"done"}}}
            ]"#,
        )
        .unwrap();
        assert_eq!(closing_transition(&transitions).unwrap().id, "381");
        assert!(closing_transition(&transitions[..2]).is_none());
    }
}
