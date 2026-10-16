//! `wt done <task>` and `wt attach`: closing a task, attaching a ticket.

use super::common::{confirm, err, fuzzy, is_ticket_key, list, resolve_task, same_name};
use crate::cli::table::load;
use crate::client::worktree;
use crate::config::Config;
use crate::table::{self, TaskRow};
use crate::vault::note::Note;
use serde_yaml::Value;

/// Whether `name` is a task, or a ticket of one: then `wt done <name>` closes
/// the task rather than one worktree.
pub(in crate::cli) fn names_task(config: &Config, name: &str) -> bool {
    load(config).is_ok_and(|tasks| {
        tasks
            .iter()
            .any(|t| !t.done && (same_name(&t.name, name) || t.tickets.iter().any(|k| k == name)))
    })
}

/// `wt done <task>`: the task is over. Its worktrees go (each must be finished:
/// clean, MR merged or closed, nothing unpushed), my open tickets of it are
/// closed in Jira, and the note gets `done: true` with the final MRs and
/// branches — after that worktable leaves it alone.
pub(in crate::cli) fn cli_done_task(
    config: &Config,
    name: &str,
    force: bool,
    yes: bool,
) -> Result<(), String> {
    let table = table::build(config, load(config)?, true);
    if let Some(warning) = table.warnings.first() {
        return Err(format!("{warning} — not closing a task on partial data"));
    }
    let row = table
        .tasks
        .iter()
        .find(|r| {
            !r.task.done
                && (same_name(&r.task.name, name) || r.task.tickets.iter().any(|k| k == name))
        })
        .ok_or_else(|| format!("no open task \"{name}\""))?;

    // what still holds the task open
    let mut blockers = Vec::new();
    for &t in &row.trees {
        let tree = table.tree(t);
        let wt = tree.wt.name();
        if tree.changes.unwrap_or(0) > 0 {
            blockers.push(format!(
                "{wt}: {} uncommitted change(s)",
                tree.changes.unwrap_or(0)
            ));
        }
        match tree.mr.map(|m| table.mr(m)) {
            Some(mr) if mr.opened() => blockers.push(format!("{wt}: MR !{} is open", mr.mr.iid())),
            Some(mr) if !tree.finished && tree.changes == Some(0) => {
                blockers.push(format!("{wt}: commits after MR !{}", mr.mr.iid()))
            }
            None if tree.unpushed.unwrap_or(0) > 0 => {
                blockers.push(format!("{wt}: unpushed commits and no MR"))
            }
            _ => {}
        }
    }
    let held: Vec<usize> = row.trees.iter().filter_map(|&t| table.tree(t).mr).collect();
    for &m in row.mrs.iter().filter(|m| !held.contains(m)) {
        if table.mr(m).opened() {
            blockers.push(format!("MR !{} is open", table.mr(m).mr.iid()));
        }
    }
    if !blockers.is_empty() && !force {
        return Err(format!(
            "\"{}\" is not finished:\n  {}\nfinish these, or pass --force",
            row.task.name,
            blockers.join("\n  ")
        ));
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
        let removed = worktree::main_checkout(&tree.repo)
            .and_then(|main| worktree::remove_worktree(&main, &tree.wt.path, force).map(|_| main));
        match removed {
            Ok(main) => {
                if let Some(branch) = &tree.wt.branch {
                    // squash-merged branches look unmerged to `branch -d`
                    if let Err(e) = worktree::delete_branch(&main, branch, true) {
                        problems.push(format!("branch {branch} kept: {e}"));
                    }
                }
                println!("removed {}", tree.wt.name());
            }
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
        crate::cli::worktrees::close_herdr_views(&table.tree(t).wt.path);
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!("done, but:\n  {}", problems.join("\n  ")))
    }
}

/// Moves the ticket to a "closed"-like status of its workflow.
fn close_in_jira(jira: &crate::client::jira::JiraClient, key: &str) -> Result<String, String> {
    let transitions = jira.transitions(key).map_err(err)?;
    let chosen =
        closing_transition(&transitions).ok_or("no closing transition from its current status")?;
    jira.transition(key, &chosen.id).map_err(err)?;
    Ok(chosen.to.name.clone())
}

/// A done-category transition, preferring "closed"/"done"/"resolved", never one
/// that cancels or declines.
fn closing_transition(
    transitions: &[crate::client::jira::Transition],
) -> Option<&crate::client::jira::Transition> {
    let refused = ["cancel", "declin", "reject", "won't", "отмен", "отклон"];
    let preferred = ["close", "done", "resolve", "закры", "готов", "решен"];
    let name =
        |t: &crate::client::jira::Transition| format!("{} {}", t.name, t.to.name).to_lowercase();
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
    let table = table::build(config, load(config)?, false);
    if let Some(row) = table
        .tasks
        .iter()
        .find(|r| r.task.tickets.iter().any(|k| k == key))
    {
        return Err(format!("{key} is already in \"{}\"", row.task.name));
    }
    let mut task = match task {
        Some(name) => resolve_task(config, Some(&name))?,
        None => {
            let open: Vec<&TaskRow> = table.tasks.iter().filter(|r| !r.task.done).collect();
            let items: Vec<String> = open.iter().map(|r| r.task.name.clone()).collect();
            let chosen = fuzzy(&format!("Attach {key} to"), &items)?;
            resolve_task(config, Some(&items[chosen]))?
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
