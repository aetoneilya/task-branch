//! `wt done` and `wt attach`: closing a task or worktrees, attaching a ticket.

use super::common::{
    confirm, err, find_task, fuzzy, is_ticket_key, list, resolve_task, resolve_tree, tree_table,
};
use super::launch::close_herdr_views;
use crate::cli::DoneArgs;
use crate::cli::table::load;
use crate::client::gitlab::{MergeRequest, MrState};
use crate::client::jira::{JiraClient, Transition};
use crate::client::worktree;
use crate::config::Config;
use crate::table::{self, Table, Tree, branch_finished};
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
        return done_task(config, tasks, name, args.force, args.yes);
    }
    done_trees(config, args)
}

/// `wt done <task>`: the task is over. Its worktrees go (each must be finished:
/// clean, MR merged or closed, nothing unpushed), my open tickets of it are
/// closed in Jira, and the note gets `done: true` with the final MRs and
/// branches — after that worktable leaves it alone.
fn done_task(
    config: &Config,
    tasks: Vec<Task>,
    name: &str,
    force: bool,
    yes: bool,
) -> Result<(), String> {
    let table = table::build(config, tasks, true);
    if let Some(warning) = table.warnings.first() {
        return Err(format!("{warning} — not closing a task on partial data"));
    }
    let row = find_task(table.tasks.iter().map(|r| &r.task), name, false)?
        .map(|i| &table.tasks[i])
        .filter(|r| !r.task.done)
        .ok_or_else(|| format!("no open task \"{name}\""))?;

    if !force {
        // what still holds the task open
        let mut blockers = Vec::new();
        for &t in &row.trees {
            blockers.extend(tree_blockers(&table, table.tree(t))?);
        }
        let held: Vec<usize> = row.trees.iter().filter_map(|&t| table.tree(t).mr).collect();
        for &m in row.mrs.iter().filter(|m| !held.contains(m)) {
            if table.mr(m).opened() {
                blockers.push(format!("MR !{} is open", table.mr(m).mr.iid()));
            }
        }
        if !blockers.is_empty() {
            return Err(not_finished(&row.task.name, &blockers));
        }
    }

    let keys: Vec<&String> = row.task.tickets.iter().chain(&row.found_tickets).collect();
    let to_close: Vec<&String> = keys
        .iter()
        .copied()
        .filter(|k| table.tickets.get(*k).is_some_and(|t| t.mine && !t.closed()))
        .collect();
    let left_open: Vec<&String> = keys
        .iter()
        .copied()
        .filter(|k| {
            table
                .tickets
                .get(*k)
                .is_some_and(|t| !t.mine && !t.closed())
        })
        .collect();

    println!("Closing task \"{}\":", row.task.name);
    for &t in &row.trees {
        println!("  remove worktree {}", table.tree(t).wt.name());
    }
    for key in &to_close {
        println!("  close {key} in Jira");
    }
    for key in &left_open {
        println!("  {key} is not assigned to me — left open");
    }
    println!("  mark the note done");
    if !yes && !confirm("Go?")? {
        return Ok(());
    }

    let mut problems = Vec::new();
    for &t in &row.trees {
        let tree = table.tree(t);
        match remove_tree(tree, mr_of(&table, tree), force) {
            Ok(kept) => problems.extend(kept),
            Err(e) => problems.push(format!("{}: {e}", tree.wt.name())),
        }
    }

    let jira = table::jira_client(config)?;
    for key in &to_close {
        match close_in_jira(&jira, key) {
            Ok(status) => println!("{key} → {status}"),
            Err(e) => problems.push(format!("{key}: {e}")),
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
        let _lock = table::lock().map_err(err)?;
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
    println!("\"{}\" is done", row.task.name);

    // last: this may close the tab wt itself runs in
    for &t in &row.trees {
        close_herdr_views(&table.tree(t).wt.path);
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!("done, but:\n  {}", problems.join("\n  ")))
    }
}

/// `wt done [<worktree>]` and `wt done --merged`: worktrees go one by one, each
/// once it is finished.
fn done_trees(config: &Config, args: DoneArgs) -> Result<(), String> {
    let table = tree_table(config, true)?;
    // GitLab did not answer: the last known MR states will do, but with none at
    // all an open MR looks like no MR
    if let Some(warning) = table.warnings.iter().find(|w| w.starts_with("GitLab")) {
        if !args.force && !warning.ends_with("showing cached") {
            return Err(format!(
                "{warning} — not removing worktrees on partial data, or pass --force"
            ));
        }
        eprintln!("{warning}");
    }

    if !args.merged {
        let tree = table.tree(resolve_tree(&table, args.name)?);
        return done_tree(&table, tree, args.force, args.yes);
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
    for tree in finished {
        if let Err(err) = done_tree(&table, tree, false, true) {
            eprintln!("{}: {err}", tree.wt.name());
        }
    }
    Ok(())
}

/// One worktree, with its branch and herdr tabs, once nothing holds it.
fn done_tree(table: &Table, tree: &Tree, force: bool, yes: bool) -> Result<(), String> {
    let name = tree.wt.name();
    if !force {
        let blockers = tree_blockers(table, tree)?;
        if !blockers.is_empty() {
            return Err(not_finished(&name, &blockers));
        }
    }

    let branch = tree.wt.branch.as_deref().unwrap_or("(detached)");
    if !yes
        && !confirm(&format!(
            "Remove {name} (branch {branch}) and its herdr tabs?"
        ))?
    {
        return Ok(());
    }
    if let Some(kept) = remove_tree(tree, mr_of(table, tree), force)? {
        eprintln!("{kept}");
    }

    // last: this may close the pane wt itself runs in
    close_herdr_views(&tree.wt.path);
    Ok(())
}

/// What keeps a worktree from being removed: uncommitted changes, an open MR,
/// commits that are nowhere but here.
fn tree_blockers(table: &Table, tree: &Tree) -> Result<Vec<String>, String> {
    let wt = tree.wt.name();
    let changes = match tree.changes {
        Some(changes) => changes,
        None => worktree::changed_files(&tree.wt.path).map_err(err)?,
    };
    let mut blockers = Vec::new();
    if changes > 0 {
        blockers.push(format!("{wt}: {changes} uncommitted change(s)"));
    }
    match mr_of(table, tree) {
        Some(mr) if mr.state() == MrState::Opened => {
            blockers.push(format!("{wt}: MR !{} is open", mr.iid()))
        }
        // unknown unpushed commits count as some
        Some(mr)
            if tree
                .unpushed
                .is_none_or(|n| !branch_finished(mr, n, tree.head.as_deref())) =>
        {
            blockers.push(format!("{wt}: commits after MR !{}", mr.iid()))
        }
        None if tree.unpushed.unwrap_or(0) > 0 => {
            blockers.push(format!("{wt}: unpushed commits and no MR"))
        }
        _ => {}
    }
    Ok(blockers)
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
/// when it was.
fn remove_tree(
    tree: &Tree,
    mr: Option<&MergeRequest>,
    force: bool,
) -> Result<Option<String>, String> {
    let name = tree.wt.name();
    let main = worktree::main_checkout(&tree.repo).map_err(err)?;
    worktree::remove_worktree(&main, &tree.wt.path, force).map_err(err)?;
    println!("removed {name}");
    let Some(branch) = &tree.wt.branch else {
        return Ok(None);
    };
    // -d compares with the local master, which is usually stale, and refuses
    // squash-merged branches; nothing is lost when every commit is on origin
    let merged = mr.is_some_and(|mr| mr.state() == MrState::Merged);
    let nothing_to_lose = merged || tree.unpushed.unwrap_or(0) == 0;
    Ok(
        worktree::delete_branch(&main, branch, force || nothing_to_lose)
            .err()
            .map(|e| format!("{name}: branch {branch} kept: {e}")),
    )
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
        .filter(|t| table::Category::parse(&t.to.status_category.key) == table::Category::Done)
        .filter(|t| !refused.iter().any(|r| name(t).contains(r)))
        .collect();
    usable
        .iter()
        .find(|t| preferred.iter().any(|p| name(t).contains(p)))
        .or(usable.first())
        .copied()
}

/// `wt attach <KEY> [<task>]`: the ticket joins a task — just its `tickets:`,
/// no worktree or agent. Without a task, a picker over the open ones.
pub(in crate::cli) fn cli_attach(
    config: &Config,
    key: &str,
    task: Option<String>,
) -> Result<(), String> {
    if !is_ticket_key(key) {
        return Err(format!("\"{key}\" is not a ticket key"));
    }
    let tasks = load(config)?;
    if let Some(i) = find_task(&tasks, key, false)? {
        return Err(format!("{key} is already in \"{}\"", tasks[i].name));
    }
    let mut task = match task {
        Some(name) => resolve_task(config, tasks, Some(&name))?,
        None => {
            let mut table = table::build(config, tasks, false);
            let open: Vec<usize> = (0..table.tasks.len())
                .filter(|&i| !table.tasks[i].task.done)
                .collect();
            let items: Vec<String> = open
                .iter()
                .map(|&i| table.tasks[i].task.name.clone())
                .collect();
            let chosen = open[fuzzy(&format!("Attach {key} to"), &items)?];
            table.tasks.swap_remove(chosen).task
        }
    };
    let _lock = table::lock().map_err(err)?;
    task.note.reload().map_err(err)?;
    let mut entries = task.note.strings("tickets");
    entries.push(key.to_string());
    task.note.set("tickets", list(&entries));
    task.note.save().map_err(err)?;
    println!("{key} attached to \"{}\"", task.name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::closing_transition;
    use crate::client::jira::Transition;

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
