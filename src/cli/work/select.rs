//! `wt select`: back to existing work, focusing its agent or resuming its session.

use super::brief::{brief_path, brief_text};
use super::common::{
    Terminal, Ui, branch_name, choose_repo, err, fetch_issue, find_task, is_ticket_key,
    name_from_issue, name_from_text, repo_with_worktree,
};
use super::launch::{Open, launch, open_agent, open_work};
use crate::cli::SelectArgs;
use crate::cli::load;
use crate::client::agent::AgentKind;
use crate::client::sessions::Session;
use crate::client::{herdr, worktree};
use crate::config::Config;
use crate::table::{self, Table, TaskRow};
use crate::vault::task::SessionRef;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

enum Work {
    Task(usize),
    Ticket(usize),
}

/// A task with nothing to get back to (an idea, or a start whose worktree
/// failed) gets its first worktree and agent: named `tree_name`, else after
/// its first ticket, else after its title.
fn first_worktree(
    config: &Config,
    row: &TaskRow,
    tree_name: Option<String>,
    bg: bool,
    ui: &mut dyn Ui,
) -> Result<(), String> {
    let task = &row.task;
    let key = task.tickets.first();
    let issue = key.map(|k| fetch_issue(config, k)).transpose()?;
    let name = match (tree_name, &issue) {
        (Some(name), _) => name,
        (None, Some(issue)) => name_from_issue(issue),
        (None, None) => name_from_text(&task.name).ok_or_else(|| {
            format!(
                "no latin worktree name comes out of \"{}\" — give one with --tree",
                task.name
            )
        })?,
    };
    let main = choose_repo(config, ui)?;
    let branch = branch_name(config, key, &name);
    let tree = worktree::worktree_dir(&main, &name);
    let brief = brief_text(
        &tree,
        &branch,
        Some(task),
        issue.as_ref(),
        &task.tickets,
        None,
    );
    let open = Open {
        kind: config.workflow.agent,
        agent: true,
        focus: !bg,
    };
    open_work(task, &main, &name, &branch, brief, open, ui)
}

/// `wt select [<task | KEY>]`: back to existing work — focus its agent, or start
/// the owning CLI in its worktree resuming the last session, restoring the worktree if
/// it was removed.
pub(in crate::cli) fn cli_select(config: &Config, args: SelectArgs) -> Result<(), String> {
    select(config, args, &mut Terminal)
}

pub(crate) fn select(config: &Config, args: SelectArgs, ui: &mut dyn Ui) -> Result<(), String> {
    let SelectArgs {
        target,
        tree: tree_name,
        session: pick_session,
        new_session,
        bg,
    } = args;
    let table = table::build(config, load(config)?, false);
    let key = target.clone().filter(|t| is_ticket_key(t));
    let work = match target {
        Some(target) => {
            find_work(&table, &target)?.ok_or_else(|| format!("no task or ticket \"{target}\""))?
        }
        None => pick_work(&table, ui)?,
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
    if !new_session && let Some(kind) = config.workflow.agent_override {
        sessions.retain(|s| AgentKind::from_session_key(&s.id) == kind);
    }
    newest_first(&mut sessions, &table.sessions);

    if pick_session {
        if sessions.is_empty() {
            return Err("the task has no saved sessions".to_string());
        }
        let items: Vec<String> = sessions
            .iter()
            .map(|s| {
                format!(
                    "{}  {}  {}  {}",
                    s.date,
                    s.worktree,
                    AgentKind::from_session_key(&s.id),
                    &AgentKind::native_id(&s.id)[..8.min(AgentKind::native_id(&s.id).len())]
                )
            })
            .collect();
        let chosen = &sessions[ui.pick("Session", &items)?];
        let tree = restore_worktree(config, row, &chosen.worktree, ui)?;
        return resume(&table, &space, chosen, &tree, note, bg, ui);
    }

    if trees.is_empty() {
        let Some(last) = sessions.first() else {
            return match row {
                Some(row) => first_worktree(config, row, tree_name, bg, ui),
                None => Err(format!("\"{space}\" has neither worktrees nor sessions")),
            };
        };
        let tree = restore_worktree(config, row, &last.worktree, ui)?;
        if new_session {
            let brief = brief_path(&tree)?;
            return launch(
                config.workflow.agent,
                &space,
                &tree,
                &last.worktree,
                (note, &brief),
                !bg,
                ui,
            );
        }
        return resume(&table, &space, last, &tree, note, bg, ui);
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
        table.tree(trees[ui.pick("Worktree", &items)?])
    };

    if !new_session {
        if let Some(agent) = tree
            .agents
            .iter()
            .find(|a| {
                config
                    .workflow
                    .agent_override
                    .is_none_or(|kind| a.kind == Some(kind))
            })
            .map(|a| a.name.as_str())
        {
            if !bg {
                herdr::focus_agent(agent).map_err(err)?;
            }
            ui.say(format!("→ {agent}"));
            return Ok(());
        }
        if let Some(last) = sessions.iter().find(|s| s.worktree == tree.wt.name()) {
            return resume(&table, &space, last, &tree.wt.path, note, bg, ui);
        }
    }
    let brief = brief_path(&tree.wt.path)?;
    launch(
        config.workflow.agent,
        &space,
        &tree.wt.path,
        &tree.wt.name(),
        (note, &brief),
        !bg,
        ui,
    )
}

/// Most recently active transcript first; saved date breaks ties or missing files.
fn newest_first(sessions: &mut [SessionRef], transcripts: &HashMap<String, Session>) {
    let modified = |s: &SessionRef| transcripts.get(&s.id).map(|t| t.modified);
    sessions.sort_by(|a, b| {
        modified(b)
            .cmp(&modified(a))
            .then_with(|| b.date.cmp(&a.date))
    });
}

/// Resume with the owning CLI in its original launch directory when available.
/// Claude's deleted transcript falls back to a fresh conversation.
fn resume(
    table: &Table,
    space: &str,
    session: &SessionRef,
    tree: &Path,
    note: Option<&Path>,
    bg: bool,
    ui: &mut dyn Ui,
) -> Result<(), String> {
    // A session already open in an agent must be focused instead of opened twice:
    // two conversations in one transcript
    if let Some(agent) = herdr::list_agents()
        .unwrap_or_default()
        .into_iter()
        .find(|a| a.matches_session(&session.id, &table.sessions))
    {
        let name = agent.name.clone().unwrap_or(agent.pane_id.clone());
        if !bg {
            herdr::focus_agent(&agent.pane_id).map_err(err)?;
        }
        ui.say(format!("the session is open in agent {name} → {name}"));
        return Ok(());
    }
    let kind = AgentKind::from_session_key(&session.id);
    let transcript = table.sessions.get(&session.id);
    if transcript.is_none() && kind == AgentKind::Claude {
        ui.say(format!(
            "{kind} conversation {} is unavailable — starting a new session",
            session.id
        ));
        let brief = brief_path(tree)?;
        return launch(
            kind,
            space,
            tree,
            &session.worktree,
            (note, &brief),
            !bg,
            ui,
        );
    }
    // Codex can resume from its own history DB or compressed rollout even when
    // no plain JSONL transcript was discoverable here.
    let launch_dir = transcript
        .and_then(|s| s.first_cwd.as_deref())
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .unwrap_or_else(|| tree.to_path_buf());
    let args = kind.resume_args(&session.id);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    open_agent(kind, space, &launch_dir, &session.worktree, &args, !bg, ui)
}

/// The worktree directory, recreated at the same path from its branch if it was
/// removed: sessions are keyed by directory.
fn restore_worktree(
    config: &Config,
    row: Option<&TaskRow>,
    name: &str,
    ui: &mut dyn Ui,
) -> Result<PathBuf, String> {
    if let Some(main) = repo_with_worktree(config, name) {
        return Ok(worktree::worktree_dir(&main, name));
    }
    let main = choose_repo(config, ui)?;
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
        many => many[ui.pick("Worktree branch", many)?].clone(),
    };
    let dir = worktree::create_worktree(&main, name, &branch).map_err(err)?;
    ui.say(format!("restored worktree {} ({branch})", dir.display()));
    Ok(dir)
}

/// A task by `find_task`; a ticket key of no task, its worktrees.
fn find_work(table: &Table, target: &str) -> Result<Option<Work>, String> {
    let key = is_ticket_key(target);
    if let Some(i) = find_task(table.tasks.iter().map(|r| &r.task), target, !key)? {
        return Ok(Some(Work::Task(i)));
    }
    Ok(key
        .then(|| table.loose.iter().position(|l| l.key == target))
        .flatten()
        .map(Work::Ticket))
}

/// Open tasks, then tickets without a task that have worktrees; agents first.
fn pick_work(table: &Table, ui: &mut dyn Ui) -> Result<Work, String> {
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
    let chosen = ui.pick("Back to", &labels)?;
    Ok(items.swap_remove(chosen).1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resumed_older_conversation_is_newer_than_an_idle_new_one() {
        let entry = |id: &str, date: &str| SessionRef {
            id: id.into(),
            date: date.into(),
            worktree: "task".into(),
        };
        let mut saved = vec![
            entry("new", "2026-10-29"),
            entry("codex:resumed", "2026-10-01"),
        ];
        let transcripts = saved
            .iter()
            .enumerate()
            .map(|(i, s)| {
                (
                    s.id.clone(),
                    Session {
                        id: s.id.clone(),
                        date: s.date.clone(),
                        cwds: vec![],
                        first_cwd: None,
                        last_cwd: None,
                        modified: 100 + i as u64,
                    },
                )
            })
            .collect();
        newest_first(&mut saved, &transcripts);
        assert_eq!(saved[0].id, "codex:resumed");
    }

    #[test]
    fn resumes_the_latest_session_of_the_day() {
        let session = |id: &str, date: &str| SessionRef {
            id: id.to_string(),
            worktree: "a".to_string(),
            date: date.to_string(),
        };
        let mut sessions = vec![
            session("old", "2026-10-01"),
            session("morning", "2026-10-08"),
            session("gone", "2026-10-08"),
            session("evening", "2026-10-08"),
        ];
        let transcript = |id: &str, modified: u64| Session {
            id: id.to_string(),
            date: "2026-10-08".to_string(),
            cwds: Vec::new(),
            first_cwd: None,
            last_cwd: None,
            modified,
        };
        let transcripts: HashMap<String, Session> =
            [transcript("morning", 100), transcript("evening", 200)]
                .into_iter()
                .map(|t| (t.id.clone(), t))
                .collect();
        newest_first(&mut sessions, &transcripts);
        let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["evening", "morning", "gone", "old"]);
    }
}
