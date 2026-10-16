//! `wt select`: back to existing work, focusing its agent or resuming its session.

use super::common::{current_repo, err, fuzzy, is_ticket_key, same_name};
use super::launch::{launch, open_agent};
use crate::cli::table::load;
use crate::client::{herdr, worktree};
use crate::config::Config;
use crate::table::{self, Table, TaskRow};
use crate::vault::task::SessionRef;
use std::path::{Path, PathBuf};

enum Work {
    Task(usize),
    Ticket(usize),
}

/// `wt select [<task | KEY>]`: back to existing work — focus its agent, or start
/// claude in its worktree resuming the last session, restoring the worktree if
/// it was removed.
pub(in crate::cli) fn cli_select(
    config: &Config,
    target: Option<String>,
    tree_name: Option<String>,
    pick_session: bool,
    bg: bool,
) -> Result<(), String> {
    let table = table::build(config, load(config)?, false);
    let key = target.clone().filter(|t| is_ticket_key(t));
    let work = match target {
        Some(target) => {
            find_work(&table, &target).ok_or_else(|| format!("no task or ticket \"{target}\""))?
        }
        None => pick_work(&table)?,
    };
    let (space, trees, row) = match work {
        Work::Task(i) => {
            let row = &table.tasks[i];
            (row.task.name.clone(), row.trees.clone(), Some(row))
        }
        Work::Ticket(i) => {
            let loose = &table.loose[i];
            (loose.key.clone(), loose.trees.clone(), None)
        }
    };

    let note = row.map(|r| r.task.note.path.as_path());
    let mut sessions: Vec<SessionRef> = row
        .map(|r| {
            r.task
                .sessions
                .iter()
                .chain(&r.new_sessions)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    sessions.sort_by(|a, b| b.date.cmp(&a.date));

    if pick_session {
        if sessions.is_empty() {
            return Err("the task has no saved sessions".to_string());
        }
        let items: Vec<String> = sessions
            .iter()
            .map(|s| format!("{}  {}  {}", s.date, s.worktree, &s.id[..8.min(s.id.len())]))
            .collect();
        let chosen = &sessions[fuzzy("Session", &items)?];
        let tree = restore_worktree(config, row, &chosen.worktree)?;
        return resume(&table, &space, chosen, &tree, note, bg);
    }

    if trees.is_empty() {
        let Some(last) = sessions.first() else {
            return Err(format!(
                "\"{space}\" has neither worktrees nor sessions: wt add \"{space}\" <KEY>"
            ));
        };
        let tree = restore_worktree(config, row, &last.worktree)?;
        return resume(&table, &space, last, &tree, note, bg);
    }

    let mut trees = trees;
    if let Some(name) = &tree_name {
        trees.retain(|&i| &table.tree(i).wt.name() == name);
        if trees.is_empty() {
            return Err(format!("\"{space}\" has no worktree {name}"));
        }
    }
    // `wt select UCP-123` means that ticket's worktree
    if let Some(key) = &key
        && trees.iter().any(|&i| table.tree(i).keys.contains(key))
    {
        trees.retain(|&i| table.tree(i).keys.contains(key));
    }
    trees.sort_by_key(|&i| std::cmp::Reverse(!table.tree(i).agents.is_empty()));
    let tree = if trees.len() == 1 {
        table.tree(trees[0])
    } else {
        let items: Vec<String> = trees
            .iter()
            .map(|&i| {
                let t = table.tree(i);
                let agents: Vec<String> = t
                    .agents
                    .iter()
                    .map(|a| format!("{}:{}", a.name, a.status.as_str()))
                    .collect();
                format!("{}  {}", t.wt.name(), agents.join(", "))
            })
            .collect();
        table.tree(trees[fuzzy("Worktree", &items)?])
    };

    if let Some(agent) = tree.agents.first().map(|a| a.name.as_str()) {
        if !bg {
            herdr::focus_agent(agent).map_err(err)?;
        }
        println!("→ {agent}");
        return Ok(());
    }
    if let Some(last) = sessions.iter().find(|s| s.worktree == tree.wt.name()) {
        return resume(&table, &space, last, &tree.wt.path, note, bg);
    }
    let brief = worktree::git_dir(&tree.wt.path)
        .map_err(err)?
        .join("task.md");
    launch(&space, &tree.wt.path, &tree.wt.name(), note, &brief, !bg)
}

/// `claude --resume` must run where the session was launched — often the main
/// checkout, not the worktree. A transcript already deleted by Claude Code's
/// cleanup leaves only a fresh start.
fn resume(
    table: &Table,
    space: &str,
    session: &SessionRef,
    tree: &Path,
    note: Option<&Path>,
    bg: bool,
) -> Result<(), String> {
    // a session still open in an agent: a second claude on it would interleave
    // two conversations in one transcript
    if let Some(agent) = herdr::list_agents()
        .unwrap_or_default()
        .into_iter()
        .find(|a| a.session_id() == Some(session.id.as_str()))
    {
        let name = agent.name.clone().unwrap_or(agent.pane_id.clone());
        if !bg {
            herdr::focus_agent(&agent.pane_id).map_err(err)?;
        }
        println!("the session is open in agent {name} → {name}");
        return Ok(());
    }
    let Some(transcript) = table.sessions.get(&session.id) else {
        println!(
            "Claude Code has already deleted conversation {} — starting a new session",
            session.id
        );
        let brief = worktree::git_dir(tree).map_err(err)?.join("task.md");
        return launch(space, tree, &session.worktree, note, &brief, !bg);
    };
    let launch_dir = transcript
        .first_cwd
        .as_deref()
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .unwrap_or_else(|| tree.to_path_buf());
    open_agent(
        space,
        &launch_dir,
        &session.worktree,
        &["--resume", &session.id],
        !bg,
    )
}

/// The worktree directory, recreated at the same path from its branch if it was
/// removed: sessions are keyed by directory.
fn restore_worktree(config: &Config, row: Option<&TaskRow>, name: &str) -> Result<PathBuf, String> {
    let main = current_repo(config)?;
    let dir = main.join(".claude/worktrees").join(name);
    if dir.exists() {
        return Ok(dir);
    }
    let keys: Vec<String> = row.map(|r| r.task.tickets.clone()).unwrap_or_default();
    let candidates: Vec<String> = worktree::all_branches(&main)
        .map_err(err)?
        .into_iter()
        .filter(|b| b.ends_with(name))
        .filter(|b| keys.is_empty() || keys.iter().any(|k| b.contains(k.as_str())))
        .collect();
    let branch = match candidates.as_slice() {
        [] => {
            return Err(format!(
                "worktree {name} is removed and its branch is gone — the note and session ids remain"
            ));
        }
        [only] => only.clone(),
        many => many[fuzzy("Worktree branch", many)?].clone(),
    };
    let dir = worktree::create_worktree(&main, name, &branch).map_err(err)?;
    println!("restored worktree {} ({branch})", dir.display());
    Ok(dir)
}

fn find_work(table: &Table, target: &str) -> Option<Work> {
    let open = |r: &&TaskRow| !r.task.done;
    if is_ticket_key(target) {
        if let Some(i) = table
            .tasks
            .iter()
            .position(|r| r.task.tickets.iter().any(|k| k == target))
        {
            return Some(Work::Task(i));
        }
        return table
            .loose
            .iter()
            .position(|l| l.key == target)
            .map(Work::Ticket);
    }
    if let Some(i) = table
        .tasks
        .iter()
        .position(|r| same_name(&r.task.name, target))
    {
        return Some(Work::Task(i));
    }
    let needle = target.to_lowercase();
    let partial: Vec<usize> = table
        .tasks
        .iter()
        .enumerate()
        .filter(|(_, r)| open(r) && r.task.name.to_lowercase().contains(&needle))
        .map(|(i, _)| i)
        .collect();
    match partial.as_slice() {
        [i] => Some(Work::Task(*i)),
        _ => None,
    }
}

/// Open tasks, then tickets without a task that have worktrees; agents first.
fn pick_work(table: &Table) -> Result<Work, String> {
    let mut items: Vec<(String, Work, bool)> = Vec::new();
    for (i, row) in table.tasks.iter().enumerate().filter(|(_, r)| !r.task.done) {
        let agents: Vec<&table::Agent> = row
            .trees
            .iter()
            .flat_map(|&t| &table.tree(t).agents)
            .collect();
        let label = format!(
            "{}  · {} worktree(s){}",
            row.task.name,
            row.trees.len(),
            if agents.is_empty() {
                String::new()
            } else {
                format!(" · {}", agents.len()) + " agent(s)"
            }
        );
        items.push((label, Work::Task(i), !agents.is_empty()));
    }
    for (i, loose) in table
        .loose
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.trees.is_empty())
    {
        let names: Vec<String> = loose
            .trees
            .iter()
            .map(|&t| table.tree(t).wt.name())
            .collect();
        let live = loose
            .trees
            .iter()
            .any(|&t| !table.tree(t).agents.is_empty());
        items.push((
            format!("{}  · {}", loose.key, names.join(", ")),
            Work::Ticket(i),
            live,
        ));
    }
    items.sort_by_key(|(_, _, live)| std::cmp::Reverse(*live));
    if items.is_empty() {
        return Err("no open tasks: wt start <name>".to_string());
    }
    let labels: Vec<String> = items.iter().map(|(l, _, _)| l.clone()).collect();
    let chosen = fuzzy("Back to", &labels)?;
    Ok(items.swap_remove(chosen).1)
}
