//! Joining the raw inputs into a table: which worktrees, MRs and tickets belong
//! to which task, what is left loose, and each task's derived state. No I/O, so
//! the rules are tested on hand-built inputs.

use super::{Agent, LooseTicket, Mr, Source, Table, TaskRow, Ticket, Tree, branch_finished};
use crate::client::gitlab::MrState;
use crate::client::herdr;
use crate::client::sessions::Session;
use crate::vault::task::{SessionRef, Task, TaskState};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Everything the table is built from, as fetched.
pub struct Inputs {
    pub tasks: Vec<Task>,
    pub tickets: HashMap<String, Ticket>,
    pub mrs: Vec<Mr>,
    /// Without agents, MR and `finished` yet: matching fills them in.
    pub trees: Vec<Tree>,
    pub agents: Vec<herdr::Agent>,
    pub workspaces: Vec<herdr::Workspace>,
    /// Claude sessions by id.
    pub sessions: HashMap<String, Session>,
    /// `<main checkout>/.claude/worktrees` of every repo.
    pub roots: Vec<PathBuf>,
    /// Jira and GitLab both answered in full: task states can be derived.
    pub complete: bool,
    pub jira: Source,
    pub gitlab: Source,
    pub warnings: Vec<String>,
    pub jira_host: String,
}

pub fn assemble(inputs: Inputs) -> Table {
    let Inputs {
        tasks,
        tickets,
        mrs,
        mut trees,
        agents,
        workspaces,
        sessions,
        roots,
        complete,
        jira,
        gitlab,
        warnings,
        jira_host,
    } = inputs;

    for tree in &mut trees {
        tree.agents = agents_in(&tree.wt.path, &agents, &sessions);
        let branch = tree.wt.branch.as_deref();
        // the open MR when there is one, otherwise the newest
        tree.mr = mrs
            .iter()
            .enumerate()
            .filter(|(_, m)| Some(m.mr.source_branch()) == branch)
            .max_by_key(|(_, m)| (m.opened(), m.mr.iid()))
            .map(|(i, _)| i);
        // a branch named without a ticket key still carries one in its MR title
        if tree.keys.is_empty()
            && let Some(i) = tree.mr
        {
            tree.keys = mrs[i].keys.clone();
        }
        tree.finished = match (tree.mr, tree.changes, tree.unpushed) {
            (Some(i), Some(0), Some(unpushed)) => {
                branch_finished(&mrs[i].mr, unpushed, tree.head.as_deref())
            }
            _ => false,
        };
    }

    let mut owners: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, task) in tasks.iter().enumerate() {
        for key in &task.tickets {
            owners.entry(key.as_str()).or_default().push(i);
        }
    }
    let mut conflicts: Vec<(String, Vec<String>)> = owners
        .iter()
        .filter(|(_, idx)| idx.len() > 1)
        .map(|(key, idx)| {
            (
                key.to_string(),
                idx.iter().map(|&i| tasks[i].name.clone()).collect(),
            )
        })
        .collect();
    conflicts.sort();
    let mut owned: HashSet<String> = owners.keys().map(|k| k.to_string()).collect();

    let mut rows: Vec<TaskRow> = tasks
        .into_iter()
        .map(|task| task_row(task, &trees, &mrs, &workspaces, &owned))
        .collect();
    // a key found on the work of several tasks is none of theirs to write down:
    // which one it is for is mine to say
    let mut finders: HashMap<String, usize> = HashMap::new();
    for key in rows.iter().flat_map(|r| &r.found_tickets) {
        *finders.entry(key.clone()).or_default() += 1;
    }
    for row in &mut rows {
        (row.shared_tickets, row.found_tickets) = std::mem::take(&mut row.found_tickets)
            .into_iter()
            .partition(|key| finders[key] > 1);
    }

    for row in rows.iter_mut().filter(|r| !r.task.done) {
        row.new_sessions = discover_sessions(row, &trees, &roots, &sessions);
        if complete {
            row.state = Some(derive_state(row, &tickets, &mrs, &trees));
        }
    }

    // worktrees already in a task through its herdr workspace are not loose
    let in_tasks: HashSet<usize> = rows.iter().flat_map(|r| r.trees.iter().copied()).collect();
    let free_trees: Vec<usize> = (0..trees.len()).filter(|i| !in_tasks.contains(i)).collect();

    // a ticket on a task's worktree or MR is that task's, listed in the note or not
    owned.extend(rows.iter().flat_map(|r| r.tickets().cloned()));
    let mut loose_keys: Vec<String> = Vec::new();
    let mut push_key = |key: &String| {
        if !owned.contains(key) && !loose_keys.contains(key) {
            loose_keys.push(key.clone());
        }
    };
    free_trees
        .iter()
        .flat_map(|&i| &trees[i].keys)
        .for_each(&mut push_key);
    mrs.iter()
        .filter(|m| m.opened())
        .flat_map(|m| &m.keys)
        .for_each(&mut push_key);
    let mut mine: Vec<&Ticket> = tickets.values().filter(|t| t.mine).collect();
    mine.sort_by(|a, b| a.key.cmp(&b.key));
    mine.into_iter().for_each(|t| push_key(&t.key));

    let mut loose: Vec<LooseTicket> = loose_keys
        .into_iter()
        .map(|key| LooseTicket {
            trees: free_trees
                .iter()
                .copied()
                .filter(|&i| trees[i].keys.contains(&key))
                .collect(),
            mrs: (0..mrs.len())
                .filter(|&i| mrs[i].keys.contains(&key))
                .collect(),
            key,
            state: None,
        })
        .collect();
    for l in &mut loose {
        l.state = loose_state(l, &trees, &mrs);
    }

    let other_trees = free_trees
        .iter()
        .copied()
        .filter(|&i| trees[i].keys.is_empty())
        .collect();
    let tree_branches: HashSet<&str> = trees
        .iter()
        .filter_map(|t| t.wt.branch.as_deref())
        .collect();
    let other_mrs = (0..mrs.len())
        .filter(|&i| {
            mrs[i].opened()
                && mrs[i].keys.is_empty()
                && !tree_branches.contains(mrs[i].mr.source_branch())
        })
        .collect();

    // active work first, newest first within a state
    rows.sort_by(|a, b| (task_rank(a), &b.task.created).cmp(&(task_rank(b), &a.task.created)));
    // worked on before in review before nothing done, newest first within each
    let loose_rank = |l: &LooseTicket| match l.state {
        Some(TaskState::Review) => 1,
        Some(_) => 0,
        None => 2,
    };
    let created = |l: &LooseTicket| {
        tickets
            .get(&l.key)
            .map(|t| t.created.clone())
            .unwrap_or_default()
    };
    loose.sort_by(|a, b| (loose_rank(a), created(b)).cmp(&(loose_rank(b), created(a))));

    Table {
        tasks: rows,
        loose,
        other_trees,
        other_mrs,
        conflicts,
        tickets,
        mrs,
        trees,
        sessions,
        jira,
        gitlab,
        warnings,
        jira_host,
    }
}

/// The task with its worktrees and MRs. A worktree belongs to the task by its
/// branch's ticket key, or because an agent in the task's herdr workspace works
/// in it; an MR by a ticket key, or by the branch of one of those worktrees.
fn task_row(
    task: Task,
    trees: &[Tree],
    mrs: &[Mr],
    workspaces: &[herdr::Workspace],
    owned: &HashSet<String>,
) -> TaskRow {
    let space = workspaces
        .iter()
        .find(|w| w.label.trim().eq_ignore_ascii_case(task.name.trim()))
        .map(|w| w.workspace_id.clone());
    let has_key = |keys: &[String]| keys.iter().any(|k| task.tickets.contains(k));
    let in_space = |tree: &Tree| {
        space
            .as_ref()
            .is_some_and(|s| tree.agents.iter().any(|a| &a.workspace == s))
    };
    let row_trees: Vec<usize> = (0..trees.len())
        .filter(|&i| has_key(&trees[i].keys) || in_space(&trees[i]))
        .collect();
    let branches: Vec<&str> = row_trees
        .iter()
        .filter_map(|&i| trees[i].wt.branch.as_deref())
        .collect();
    let row_mrs: Vec<usize> = (0..mrs.len())
        .filter(|&i| has_key(&mrs[i].keys) || branches.contains(&mrs[i].mr.source_branch()))
        .collect();
    // keys on its worktrees and MRs that no note lists are the task's too
    let mut found_tickets: Vec<String> = Vec::new();
    if !task.done {
        let keys = row_trees
            .iter()
            .flat_map(|&i| &trees[i].keys)
            .chain(row_mrs.iter().flat_map(|&i| &mrs[i].keys));
        for key in keys {
            let listed = task.tickets.contains(key) || owned.contains(key);
            if !listed && !found_tickets.contains(key) {
                found_tickets.push(key.clone());
            }
        }
    }
    TaskRow {
        task,
        trees: row_trees,
        mrs: row_mrs,
        found_tickets,
        shared_tickets: Vec::new(),
        new_sessions: Vec::new(),
        space,
        state: None,
    }
}

/// `active` or `review` for a ticket with a live worktree or an open MR, None
/// when nothing is done on it yet.
fn loose_state(loose: &LooseTicket, trees: &[Tree], mrs: &[Mr]) -> Option<TaskState> {
    let live: Vec<&Tree> = loose
        .trees
        .iter()
        .map(|&i| &trees[i])
        .filter(|t| !t.finished)
        .collect();
    let open = loose.mrs.iter().any(|&i| mrs[i].opened());
    match (live.is_empty(), open) {
        (true, false) => None,
        (_, true) if live.iter().all(|t| t.clean()) => Some(TaskState::Review),
        _ => Some(TaskState::Active),
    }
}

/// active, review, idea, finished, then done tasks.
fn task_rank(row: &TaskRow) -> u8 {
    if row.task.done {
        return 5;
    }
    match row.current_state() {
        Some(TaskState::Active) => 0,
        Some(TaskState::Review) => 1,
        Some(TaskState::Finished) => 3,
        Some(TaskState::Idea) | None => 2,
    }
}

/// | state      | condition                                                          |
/// |------------|--------------------------------------------------------------------|
/// | `finished` | every ticket closed, every MR merged or closed, no local changes   |
/// | `review`   | MRs opened or merged, at least one opened, worktrees clean         |
/// | `active`   | a worktree, an open MR or a ticket in progress                     |
/// | `idea`     | nothing of the above                                               |
fn derive_state(
    row: &TaskRow,
    tickets: &HashMap<String, Ticket>,
    mrs: &[Mr],
    trees: &[Tree],
) -> TaskState {
    let task_tickets: Vec<Option<&Ticket>> = row.tickets().map(|k| tickets.get(k)).collect();
    let row_mrs: Vec<&Mr> = row.mrs.iter().map(|&i| &mrs[i]).collect();
    let row_trees: Vec<&Tree> = row.trees.iter().map(|&i| &trees[i]).collect();

    let tickets_closed = task_tickets.iter().all(|t| t.is_some_and(Ticket::closed));
    let any_work = !task_tickets.is_empty() || !row_mrs.is_empty();
    let mrs_closed = row_mrs.iter().all(|m| !m.opened());
    let no_changes = row_trees.iter().all(|t| t.changes == Some(0));
    if any_work && tickets_closed && mrs_closed && no_changes {
        return TaskState::Finished;
    }

    let mrs_in_review = row_mrs.iter().any(|m| m.opened())
        && row_mrs
            .iter()
            .all(|m| matches!(m.mr.state(), MrState::Opened | MrState::Merged));
    if mrs_in_review && row_trees.iter().all(|t| t.clean()) {
        return TaskState::Review;
    }

    let in_progress = task_tickets
        .iter()
        .flatten()
        .any(|t| t.category == super::Category::InProgress);
    if !row_trees.is_empty() || row_mrs.iter().any(|m| m.opened()) || in_progress {
        return TaskState::Active;
    }
    TaskState::Idea
}

/// Sessions that worked in one of the task's worktrees — current ones, or ones
/// its note already lists sessions for — and are not in the note yet.
fn discover_sessions(
    row: &TaskRow,
    trees: &[Tree],
    roots: &[PathBuf],
    sessions: &HashMap<String, Session>,
) -> Vec<SessionRef> {
    let mut names: Vec<String> = row.trees.iter().map(|&i| trees[i].wt.name()).collect();
    names.extend(row.task.sessions.iter().map(|s| s.worktree.clone()));

    let mut found: Vec<SessionRef> = sessions
        .values()
        .filter(|s| !row.task.sessions.iter().any(|known| known.id == s.id))
        .filter_map(|s| {
            let worktree = roots
                .iter()
                .flat_map(|root| s.worktrees_under(root))
                .find(|name| names.contains(name))?;
            Some(SessionRef {
                id: s.id.clone(),
                worktree,
                date: s.date.clone(),
            })
        })
        .collect();
    found.sort_by(|a, b| a.date.cmp(&b.date).then_with(|| a.id.cmp(&b.id)));
    found
}

/// Agents working in `dir`: where their claude session last worked, else the
/// pane's cwd — agents started in the main checkout move into worktrees.
pub(super) fn agents_in(
    dir: &Path,
    agents: &[herdr::Agent],
    sessions: &HashMap<String, Session>,
) -> Vec<Agent> {
    agents
        .iter()
        .filter(|a| {
            let last_cwd = a
                .session_id()
                .and_then(|id| sessions.get(id))
                .and_then(|s| s.last_cwd.as_deref());
            match last_cwd {
                Some(cwd) => Path::new(cwd).starts_with(dir),
                None => a.runs_in(dir),
            }
        })
        .map(Agent::from_herdr)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::*;
    use super::*;
    use crate::table::{Category, write_back};
    use crate::vault::note::Note;

    #[test]
    fn worktree_joins_task_by_key_in_branch() {
        let table = table(
            vec![task("a2i", &["UCP-1"]), task("other", &["UCP-2"])],
            vec![],
            vec![],
            vec![
                tree("a2i-config", "i.udalov/UCP-1-config"),
                tree("misc", "i.udalov/misc"),
            ],
        );
        let row = table.task_row("a2i");
        assert_eq!(row.trees, vec![table.tree_index("a2i-config")]);
        assert!(table.task_row("other").trees.is_empty());
        assert_eq!(table.other_trees, vec![table.tree_index("misc")]);
    }

    #[test]
    fn worktree_joins_task_through_an_agent_of_its_workspace() {
        let mut inputs = inputs(
            vec![task("a2i", &["UCP-1"])],
            vec![],
            vec![],
            vec![
                tree("quick-fix", "i.udalov/quick-fix"),
                tree("elsewhere", "i.udalov/x"),
            ],
        );
        inputs.workspaces = vec![workspace("w1", "A2I "), workspace("w2", "other")];
        inputs.agents = vec![
            agent("fixer", "quick-fix", "w1"),
            agent("x", "elsewhere", "w2"),
        ];
        let table = assemble(inputs);
        let row = table.task_row("a2i");
        assert_eq!(row.space.as_deref(), Some("w1"));
        assert_eq!(row.trees, vec![table.tree_index("quick-fix")]);
        // a worktree in a task is not loose, key or not
        assert_eq!(table.other_trees, vec![table.tree_index("elsewhere")]);
        let fixer = &table.tree(table.tree_index("quick-fix")).agents[0];
        assert_eq!(
            (fixer.name.as_str(), fixer.status),
            ("fixer", crate::table::AgentStatus::Working)
        );
    }

    #[test]
    fn mr_joins_by_key_or_by_the_branch_of_a_task_worktree() {
        let table = table(
            vec![task("a2i", &["UCP-1"])],
            vec![],
            vec![
                mr(10, "i.udalov/UCP-1-a", "first", "opened"),
                mr(11, "i.udalov/b", "UCP-1: by title", "merged"),
                mr(12, "i.udalov/quick-fix", "no key", "opened"),
                mr(13, "i.udalov/unrelated", "no key either", "opened"),
            ],
            vec![
                tree("a", "i.udalov/UCP-1-a"),
                tree("quick-fix", "i.udalov/quick-fix"),
            ],
        );
        // the keyless worktree is not the task's: its MR isn't either
        let row = table.task_row("a2i");
        assert_eq!(row.mrs, vec![table.mr_index(10), table.mr_index(11)]);
        assert_eq!(
            table.tree(table.tree_index("a")).mr,
            Some(table.mr_index(10))
        );
        assert_eq!(table.other_mrs, vec![table.mr_index(13)]);

        let mut inputs = inputs(
            vec![task("a2i", &["UCP-1"])],
            vec![],
            vec![mr(12, "i.udalov/quick-fix", "no key", "opened")],
            vec![tree("quick-fix", "i.udalov/quick-fix")],
        );
        inputs.workspaces = vec![workspace("w1", "a2i")];
        inputs.agents = vec![agent("fixer", "quick-fix", "w1")];
        let table = assemble(inputs);
        assert_eq!(table.task_row("a2i").mrs, vec![0]);
        assert!(table.other_mrs.is_empty());
    }

    #[test]
    fn keys_on_a_task_s_work_are_found_tickets_unless_another_note_lists_them() {
        let mut table = table(
            vec![task("a2i", &["UCP-1"]), task("b", &["UCP-3"])],
            vec![],
            vec![mr(10, "i.udalov/UCP-1-UCP-2", "UCP-3 too", "opened")],
            vec![tree("a", "i.udalov/UCP-1-and-UCP-4")],
        );
        // UCP-3 is b's: the MR is b's too, but a2i does not take the key over
        assert_eq!(table.task_row("a2i").found_tickets, vec!["UCP-4"]);
        assert!(table.task_row("b").found_tickets.is_empty());
        // UCP-2 is on the work of both: shown in both, written into neither
        assert_eq!(table.task_row("a2i").shared_tickets, vec!["UCP-2"]);
        assert_eq!(table.task_row("b").shared_tickets, vec!["UCP-2"]);
        assert!(table.loose_keys().is_empty());

        write_back(&mut table);
        let tickets = |name: &str| {
            let path = &table.task_row(name).task.note.path;
            Task::from_note(Note::read(path).unwrap()).tickets
        };
        assert_eq!(tickets("a2i"), vec!["UCP-1", "UCP-4"]);
        assert_eq!(tickets("b"), vec!["UCP-3"]);
    }

    #[test]
    fn loose_tickets_are_worked_on_or_assigned() {
        let mut finished = tree("old", "i.udalov/UCP-9-old");
        finished.unpushed = Some(0);
        let table = table(
            vec![],
            vec![
                ticket("UCP-5", Category::InProgress, true),
                ticket("UCP-6", Category::New, true),
                ticket("UCP-7", Category::Done, false),
                ticket("UCP-9", Category::Done, false),
            ],
            vec![
                mr(20, "i.udalov/UCP-7", "x", "opened"),
                mr(21, "i.udalov/UCP-9-old", "y", "merged"),
            ],
            vec![tree("five", "i.udalov/UCP-5-five"), finished],
        );
        assert!(table.tree(table.tree_index("old")).finished);
        // worked on first (in review after active), then nothing done
        assert_eq!(table.loose_keys(), vec!["UCP-5", "UCP-7", "UCP-9", "UCP-6"]);
        let states: Vec<Option<TaskState>> = table.loose.iter().map(|l| l.state).collect();
        assert_eq!(
            states,
            vec![Some(TaskState::Active), Some(TaskState::Review), None, None]
        );
    }

    fn state_of(tickets: Vec<Ticket>, mrs: Vec<Mr>, trees: Vec<Tree>, keys: &[&str]) -> TaskState {
        let table = table(vec![task("t", keys)], tickets, mrs, trees);
        table.tasks[0].state.unwrap()
    }

    #[test]
    fn derives_task_state() {
        use Category::*;
        // every ticket closed, every MR merged, nothing local
        assert_eq!(
            state_of(
                vec![ticket("UCP-1", Done, false)],
                vec![mr(1, "UCP-1-a", "x", "merged")],
                vec![],
                &["UCP-1"]
            ),
            TaskState::Finished
        );
        // an open MR, the others merged, worktrees clean
        assert_eq!(
            state_of(
                vec![ticket("UCP-1", InProgress, true)],
                vec![
                    mr(1, "UCP-1-a", "x", "merged"),
                    mr(2, "UCP-1-b", "y", "opened")
                ],
                vec![tree("a", "UCP-1-b")],
                &["UCP-1"]
            ),
            TaskState::Review
        );
        // local changes keep it active
        let mut busy = tree("a", "UCP-1-b");
        busy.changes = Some(3);
        assert_eq!(
            state_of(
                vec![ticket("UCP-1", New, true)],
                vec![mr(2, "UCP-1-b", "y", "opened")],
                vec![busy],
                &["UCP-1"]
            ),
            TaskState::Active
        );
        // a ticket in progress with nothing else
        assert_eq!(
            state_of(
                vec![ticket("UCP-1", InProgress, true)],
                vec![],
                vec![],
                &["UCP-1"]
            ),
            TaskState::Active
        );
        assert_eq!(
            state_of(vec![ticket("UCP-1", New, true)], vec![], vec![], &["UCP-1"]),
            TaskState::Idea
        );
        assert_eq!(state_of(vec![], vec![], vec![], &[]), TaskState::Idea);
        // not recomputed when a service did not answer
        let mut partial = inputs(vec![task("t", &[])], vec![], vec![], vec![]);
        partial.complete = false;
        assert_eq!(assemble(partial).tasks[0].state, None);
    }

    #[test]
    fn tasks_go_by_state_then_newest_first() {
        let done = task_note("done", "done: true", "2026-10-09");
        let table = table(
            vec![
                task_note("idea-old", "state: idea", "2026-09-01"),
                done,
                task_note("active-old", "tickets: [UCP-1]", "2026-09-01"),
                task_note("idea-new", "", "2026-10-05"),
                task_note("active-new", "tickets: [UCP-2]", "2026-10-02"),
                task_note("review", "tickets: [UCP-3]", "2026-08-01"),
            ],
            vec![
                ticket("UCP-1", Category::InProgress, true),
                ticket("UCP-2", Category::InProgress, true),
            ],
            vec![mr(1, "UCP-3-a", "x", "opened")],
            vec![],
        );
        let names: Vec<&str> = table.tasks.iter().map(|r| r.task.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "active-new",
                "active-old",
                "review",
                "idea-new",
                "idea-old",
                "done"
            ]
        );
    }

    #[test]
    fn discovers_sessions_held_in_task_worktrees() {
        let mut inputs = inputs(
            vec![task_note(
                "a2i",
                "tickets: [UCP-1]\nsessions: [\"s0 gone 2026-10-01\"]",
                "2026-10-01",
            )],
            vec![],
            vec![],
            vec![tree("a", "UCP-1-a")],
        );
        let session = |id: &str, cwd: &str| Session {
            id: id.to_string(),
            date: "2026-10-08".to_string(),
            cwds: vec![(format!("{ROOT}/{cwd}"), 1)],
            first_cwd: None,
            last_cwd: None,
            modified: 0,
        };
        for s in [
            session("s0", "gone"),
            session("s1", "a/sub"),
            session("s2", "gone"),
            session("s3", "b"),
        ] {
            inputs.sessions.insert(s.id.clone(), s);
        }
        let table = assemble(inputs);
        let found: Vec<(&str, &str)> = table.tasks[0]
            .new_sessions
            .iter()
            .map(|s| (s.id.as_str(), s.worktree.as_str()))
            .collect();
        // in a current worktree, or in one the note already has sessions of
        assert_eq!(found, vec![("s1", "a"), ("s2", "gone")]);
    }

    #[test]
    fn write_back_updates_managed_keys_only() {
        let mut inputs = inputs(
            vec![task_note(
                "a2i",
                "tickets: [\"[[UCP-1]]\"]\ntags: [x]\nstate: idea\nsessions: [\"s0 a 2026-10-01\"]",
                "2026-10-01",
            )],
            vec![ticket("UCP-1", Category::InProgress, true)],
            vec![],
            vec![tree("a", "i.udalov/UCP-1-UCP-2")],
        );
        inputs.sessions.insert(
            "s1".to_string(),
            Session {
                id: "s1".to_string(),
                date: "2026-10-08".to_string(),
                cwds: vec![(format!("{ROOT}/a"), 1)],
                first_cwd: None,
                last_cwd: None,
                modified: 0,
            },
        );
        let mut table = assemble(inputs);
        let path = table.tasks[0].task.note.path.clone();
        assert_eq!(write_back(&mut table), vec!["a2i"]);

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("tags: [x]"), "{text}");
        let task = Task::from_note(Note::read(&path).unwrap());
        assert_eq!(task.state, Some(TaskState::Active));
        assert_eq!(task.note.strings("tickets"), vec!["[[UCP-1]]", "UCP-2"]);
        let sessions: Vec<String> = task.sessions.iter().map(|s| s.entry()).collect();
        assert_eq!(sessions, vec!["s0 a 2026-10-01", "s1 a 2026-10-08"]);
        // the table now matches the note: nothing more to write
        assert!(write_back(&mut table).is_empty());
    }
}
