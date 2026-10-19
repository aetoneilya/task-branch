//! Small hand-built inputs for the matching and view tests: tasks are real
//! notes in a temp directory, everything else is built in memory.

use super::matching::{Inputs, assemble};
use super::{Category, Fresh, Mr, Source, Table, Ticket, Tree};
use crate::client::gitlab::MergeRequest;
use crate::client::herdr;
use crate::client::worktree::Worktree;
use crate::vault::note::Note;
use crate::vault::task::{Task, ticket_keys};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

pub const ROOT: &str = "/r/.claude/worktrees";

/// A fresh empty directory for one test's notes.
pub fn temp_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "worktable-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A task note with `frontmatter` (YAML lines), created at `created`.
pub fn task_note(name: &str, frontmatter: &str, created: &str) -> Task {
    let path = temp_dir().join(format!("{name}.md"));
    std::fs::write(&path, format!("---\n{frontmatter}\n---\nbody\n")).unwrap();
    let mut task = Task::from_note(Note::read(&path).unwrap());
    task.created = Some(created.to_string());
    task
}

pub fn task(name: &str, tickets: &[&str]) -> Task {
    task_note(
        name,
        &format!("tickets: [{}]", tickets.join(", ")),
        "2026-10-01",
    )
}

pub fn ticket(key: &str, category: Category, mine: bool) -> Ticket {
    Ticket {
        key: key.to_string(),
        summary: format!("summary of {key}"),
        status: format!("{category:?}"),
        category,
        mine,
        created: "2026-10-01T00:00:00+00:00".to_string(),
    }
}

/// `state`: opened, merged or closed.
pub fn mr(iid: u64, branch: &str, title: &str, state: &str) -> Mr {
    let mr: MergeRequest = serde_json::from_value(serde_json::json!({
        "iid": iid,
        "title": title,
        "state": state,
        "web_url": format!("https://gitlab/mr/{iid}"),
        "source_branch": branch,
        "sha": null,
    }))
    .unwrap();
    Mr::new(mr, "ai/ucp")
}

/// A clean worktree `ROOT/<name>` on `branch`.
pub fn tree(name: &str, branch: &str) -> Tree {
    Tree {
        repo: PathBuf::from("/r"),
        project: None,
        wt: Worktree {
            path: PathBuf::from(ROOT).join(name),
            branch: Some(branch.to_string()),
        },
        keys: ticket_keys(branch),
        changes: Some(0),
        unpushed: Some(0),
        head: None,
        agents: Vec::new(),
        mr: None,
        finished: false,
        last_commit: None,
    }
}

/// An agent whose pane is in worktree `tree`, in herdr workspace `space`.
pub fn agent(name: &str, tree: &str, space: &str) -> herdr::Agent {
    herdr::Agent {
        name: Some(name.to_string()),
        agent_status: "working".to_string(),
        cwd: Some(format!("{ROOT}/{tree}")),
        pane_id: format!("{space}:{name}"),
        tab_id: format!("{space}:t"),
        workspace_id: space.to_string(),
        agent_session: None,
    }
}

pub fn workspace(id: &str, label: &str) -> herdr::Workspace {
    herdr::Workspace {
        workspace_id: id.to_string(),
        label: label.to_string(),
        worktree: None,
    }
}

/// Inputs as from a build where Jira and GitLab both answered.
pub fn inputs(tasks: Vec<Task>, tickets: Vec<Ticket>, mrs: Vec<Mr>, trees: Vec<Tree>) -> Inputs {
    Inputs {
        tasks,
        tickets: tickets.into_iter().map(|t| (t.key.clone(), t)).collect(),
        mrs,
        trees,
        agents: Vec::new(),
        workspaces: Vec::new(),
        sessions: HashMap::new(),
        roots: vec![PathBuf::from(ROOT)],
        complete: true,
        jira: live(),
        gitlab: live(),
        warnings: Vec::new(),
        jira_host: "https://jira".to_string(),
    }
}

fn live() -> Source {
    Source {
        fresh: Fresh::Live,
        warning: None,
    }
}

pub fn table(tasks: Vec<Task>, tickets: Vec<Ticket>, mrs: Vec<Mr>, trees: Vec<Tree>) -> Table {
    assemble(inputs(tasks, tickets, mrs, trees))
}

impl Table {
    pub fn task_row(&self, name: &str) -> &super::TaskRow {
        self.tasks.iter().find(|r| r.task.name == name).unwrap()
    }

    pub fn tree_index(&self, name: &str) -> usize {
        self.trees.iter().position(|t| t.wt.name() == name).unwrap()
    }

    pub fn mr_index(&self, iid: u64) -> usize {
        self.mrs.iter().position(|m| m.mr.iid() == iid).unwrap()
    }

    pub fn loose_keys(&self) -> Vec<&str> {
        self.loose.iter().map(|l| l.key.as_str()).collect()
    }
}
