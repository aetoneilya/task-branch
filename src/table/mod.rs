//! The table: task notes joined with their Jira tickets, GitLab MRs, worktrees,
//! herdr agents and claude sessions. `fetch` collects the raw inputs, `matching`
//! joins them, `view` decides what is shown and how it nests.

mod fetch;
pub mod look;
mod matching;
pub mod view;

#[cfg(test)]
pub(crate) mod fixtures;

use crate::client::gitlab::{MergeRequest, MrReview, MrState};
pub use crate::client::jira::Category;
use crate::client::jira::JiraIssue;
use crate::client::worktree::Worktree;
use crate::client::{herdr, sessions};
use crate::config::Config;
use crate::vault::task::{SessionRef, Task, TaskState, ticket_keys};
use regex::Regex;
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Ticket {
    pub key: String,
    pub summary: String,
    pub status: String,
    pub category: Category,
    /// Assigned to me and not done.
    pub mine: bool,
    /// RFC 3339.
    pub created: String,
}

impl Ticket {
    fn from_issue(issue: &JiraIssue, mine: bool) -> Ticket {
        Ticket {
            key: issue.key.clone(),
            summary: issue.fields.summary.clone(),
            status: issue.status().to_string(),
            category: issue.status_category(),
            mine,
            created: issue.fields.created.to_rfc3339(),
        }
    }

    pub fn closed(&self) -> bool {
        self.category == Category::Done
    }
}

static LEADING_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z][A-Z0-9]+-\d+:?\s*").unwrap());

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Mr {
    pub mr: MergeRequest,
    /// Ticket keys in the source branch and the title.
    pub keys: Vec<String>,
    /// Pipeline, approvals and unresolved threads; open MRs only.
    pub review: Option<MrReview>,
    /// The GitLab project it is in; empty in a cache from before it was kept.
    #[serde(default)]
    pub project: String,
}

impl Mr {
    fn new(mr: MergeRequest, project: &str) -> Mr {
        let mut keys = ticket_keys(mr.source_branch());
        for key in ticket_keys(mr.title()) {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        Mr {
            mr,
            keys,
            review: None,
            project: project.to_string(),
        }
    }

    pub fn opened(&self) -> bool {
        self.mr.state() == MrState::Opened
    }

    /// The title without the ticket key it starts with: rows show the key apart.
    fn short_title(&self) -> Cow<'_, str> {
        LEADING_KEY.replace(self.mr.title(), "")
    }
}

/// What a herdr agent is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Working,
    Blocked,
    Idle,
    Done,
    Unknown,
}

impl AgentStatus {
    fn parse(status: &str) -> AgentStatus {
        match status {
            "working" => AgentStatus::Working,
            "blocked" => AgentStatus::Blocked,
            "idle" => AgentStatus::Idle,
            "done" => AgentStatus::Done,
            _ => AgentStatus::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AgentStatus::Working => "working",
            AgentStatus::Blocked => "blocked",
            AgentStatus::Idle => "idle",
            AgentStatus::Done => "done",
            AgentStatus::Unknown => "unknown",
        }
    }

    /// Waits for me: asking (blocked) or finished unseen (done).
    pub fn waiting(self) -> bool {
        matches!(self, AgentStatus::Blocked | AgentStatus::Done)
    }
}

/// A herdr agent working in a worktree, or in its task's workspace outside any.
#[derive(Debug, Clone)]
pub struct Agent {
    /// Its herdr name, or the pane id when it has none.
    pub name: String,
    pub pane: String,
    pub tab: String,
    pub status: AgentStatus,
    /// The herdr workspace it runs in.
    pub workspace: String,
    /// Unix time its claude session last wrote to the transcript.
    pub active: Option<u64>,
}

impl Agent {
    /// It has a herdr name: herdr drops it when the agent restarts.
    pub fn named(&self) -> bool {
        self.name != self.pane
    }

    fn from_herdr(agent: &herdr::Agent, sessions: &HashMap<String, sessions::Session>) -> Agent {
        Agent {
            name: agent.name.clone().unwrap_or_else(|| agent.pane_id.clone()),
            pane: agent.pane_id.clone(),
            tab: agent.tab_id.clone(),
            status: AgentStatus::parse(&agent.agent_status),
            workspace: agent.workspace_id.clone(),
            active: agent
                .session_id()
                .and_then(|id| sessions.get(id))
                .map(|s| s.modified)
                .filter(|&m| m > 0),
        }
    }
}

/// A linked worktree of one of the configured repositories.
#[derive(Debug)]
pub struct Tree {
    pub repo: PathBuf,
    /// The repo's GitLab project, when the config names it: only MRs from there
    /// are this worktree's.
    pub project: Option<String>,
    pub wt: Worktree,
    pub keys: Vec<String>,
    pub changes: Option<usize>,
    pub unpushed: Option<usize>,
    /// The branch's head commit; read only when commits are unpushed.
    pub head: Option<String>,
    pub agents: Vec<Agent>,
    /// The MR opened from this worktree's branch, index into `Table::mrs`.
    pub mr: Option<usize>,
    /// Its MR is merged or closed and nothing local is left: safe to remove.
    pub finished: bool,
    /// Unix time of the branch's last commit.
    pub last_commit: Option<i64>,
}

impl Tree {
    pub fn clean(&self) -> bool {
        self.changes == Some(0) && self.unpushed == Some(0)
    }
}

#[derive(Debug)]
pub struct TaskRow {
    pub task: Task,
    pub trees: Vec<usize>,
    pub mrs: Vec<usize>,
    /// Ticket keys on the task's worktrees and MRs that no note lists yet:
    /// `write_back` adds them to the task's `tickets`.
    pub found_tickets: Vec<String>,
    /// Keys no note lists yet that are on other tasks' work too: shown, but
    /// written into no note.
    pub shared_tickets: Vec<String>,
    /// Sessions found in the task's worktrees that the note does not list yet.
    pub new_sessions: Vec<SessionRef>,
    /// The task's herdr workspace: the one labelled with the task name.
    pub space: Option<String>,
    /// Agents of that workspace that work in none of the worktrees, say in the
    /// main checkout.
    pub agents: Vec<Agent>,
    /// None when Jira or GitLab did not answer.
    pub state: Option<TaskState>,
    /// The task this one is part of, index into `Table::tasks`.
    pub parent: Option<usize>,
}

impl TaskRow {
    /// The derived state, else the one the note had last time.
    pub fn current_state(&self) -> Option<TaskState> {
        self.state.or(self.task.state)
    }

    /// What a row shows as the task's state: done for a closed task.
    pub fn row_state(&self) -> Option<RowState> {
        if self.task.done {
            return Some(RowState::Done);
        }
        self.current_state().map(RowState::Task)
    }

    /// The note's tickets, then the ones found on its worktrees and MRs.
    pub fn tickets(&self) -> impl Iterator<Item = &String> {
        self.task
            .tickets
            .iter()
            .chain(&self.found_tickets)
            .chain(&self.shared_tickets)
    }
}

/// A task's state as rows show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    Task(TaskState),
    /// A closed task.
    Done,
}

impl RowState {
    pub fn as_str(self) -> &'static str {
        match self {
            RowState::Task(state) => state.as_str(),
            RowState::Done => "done",
        }
    }
}

/// Work on a ticket that belongs to no task.
#[derive(Debug)]
pub struct LooseTicket {
    pub key: String,
    pub trees: Vec<usize>,
    pub mrs: Vec<usize>,
    /// `active` or `review` with a live worktree or an open MR; None when
    /// nothing is done on it yet.
    pub state: Option<TaskState>,
}

/// How current the table's data from a remote service is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fresh {
    /// It answered in this build.
    Live,
    /// Not asked, or it did not answer: what it said last time stands in.
    Cached,
    /// Not asked, or it did not answer, and nothing is cached either.
    Missing,
}

/// What a remote service gave this build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub fresh: Fresh,
    /// Why it did not answer, as shown in `Table::warnings`; None when it did,
    /// or was not asked.
    pub warning: Option<String>,
}

#[derive(Debug)]
pub struct Table {
    pub tasks: Vec<TaskRow>,
    pub loose: Vec<LooseTicket>,
    pub other_trees: Vec<usize>,
    pub other_mrs: Vec<usize>,
    /// Tickets listed in more than one task.
    pub conflicts: Vec<(String, Vec<String>)>,
    pub tickets: HashMap<String, Ticket>,
    pub mrs: Vec<Mr>,
    pub trees: Vec<Tree>,
    /// Claude sessions of the main checkouts, worktrees and open tasks, by id.
    pub sessions: HashMap<String, sessions::Session>,
    pub jira: Source,
    pub gitlab: Source,
    pub warnings: Vec<String>,
    pub jira_host: String,
}

/// `online: false` skips Jira and GitLab: worktrees, agents, workspaces and
/// sessions only, without task states — for commands that must answer fast.
pub fn build(config: &Config, tasks: Vec<Task>, online: bool) -> Table {
    let inputs = fetch::fetch(config, tasks, online, online);
    crate::trace::time("matching", || matching::assemble(inputs))
}

/// Offline, but with the tickets and MRs of the last online build: the TUI's
/// first frame shows them at once while the refresh runs.
pub fn build_cached(config: &Config, tasks: Vec<Task>) -> Table {
    matching::assemble(fetch::fetch(config, tasks, false, true))
}

/// MR merged or closed, and the branch has nothing beyond it: either every commit
/// is on origin, or the local tip is the MR head (squash merges leave the branch
/// commits off master, and the remote branch is usually deleted).
pub fn branch_finished(mr: &MergeRequest, unpushed: usize, head: Option<&str>) -> bool {
    if mr.state() == MrState::Opened {
        return false;
    }
    unpushed == 0 || (head.is_some() && head == mr.sha())
}

/// Writes changed `state`, found tickets and new `sessions` into the notes of
/// open tasks. Returns the names of updated notes; failures go to `table.warnings`.
pub fn write_back(table: &mut Table) -> Vec<String> {
    let _lock = match crate::vault::lock() {
        Ok(lock) => lock,
        Err(err) => {
            table
                .warnings
                .push(format!("notes not updated, can't lock: {err}"));
            return Vec::new();
        }
    };

    let mut updated = Vec::new();
    for row in table.tasks.iter_mut().filter(|r| !r.task.done) {
        match row
            .task
            .record(row.state, &mut row.found_tickets, &mut row.new_sessions)
        {
            Ok(true) => updated.push(row.task.name.clone()),
            Ok(false) => {}
            Err(err) => table.warnings.push(err.to_string()),
        }
    }
    updated
}

impl Table {
    /// Fresh agent states without rebuilding: herdr answers in milliseconds, so
    /// the TUI polls it far more often than Jira and GitLab. Worktrees keep their
    /// task; only who works in them and how changes.
    pub fn update_agents(&mut self, agents: &[herdr::Agent]) {
        for tree in &mut self.trees {
            tree.agents = matching::agents_in(&tree.wt.path, agents, &self.sessions);
        }
        for row in &mut self.tasks {
            row.agents =
                matching::agents_outside(row.space.as_deref(), agents, &self.trees, &self.sessions);
        }
    }

    /// Names for agents herdr knows by pane id only, as `(pane, name)`: after
    /// the label of their herdr tab, as herdr shows them; a tab herdr labelled
    /// by itself (a number, a repository's directory) gives way to the
    /// worktree's name, or the task's, else its first ticket.
    pub fn agent_names(&self, tabs: &HashMap<String, String>) -> Vec<(String, String)> {
        let repos: Vec<String> = self
            .trees
            .iter()
            .filter_map(|t| t.repo.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .collect();
        let label = |agent: &Agent| {
            tabs.get(&agent.tab)
                .filter(|l| !l.chars().all(|c| c.is_ascii_digit()) && !repos.contains(l))
        };
        let unnamed = self
            .trees
            .iter()
            .flat_map(|t| t.agents.iter().map(move |a| (a, t.wt.name())))
            .chain(self.tasks.iter().flat_map(|row| {
                let base = match herdr::agent_name(&row.task.name).as_str() {
                    "agent" => row.tickets().next().cloned().unwrap_or_default(),
                    _ => row.task.name.clone(),
                };
                row.agents.iter().map(move |a| (a, base.clone()))
            }));
        let mut taken: Vec<String> = self
            .trees
            .iter()
            .flat_map(|t| &t.agents)
            .chain(self.tasks.iter().flat_map(|r| &r.agents))
            .filter(|a| a.named())
            .map(|a| a.name.clone())
            .collect();
        let mut names = Vec::new();
        for (agent, base) in unnamed {
            if agent.named() || names.iter().any(|(p, _)| p == &agent.pane) {
                continue;
            }
            let name = herdr::free_agent_name(label(agent).unwrap_or(&base), &taken);
            taken.push(name.clone());
            names.push((agent.pane.clone(), name));
        }
        names
    }

    /// The tasks whose parent is task `i`, in table order.
    pub fn subtasks(&self, i: usize) -> Vec<usize> {
        (0..self.tasks.len())
            .filter(|&t| self.tasks[t].parent == Some(i))
            .collect()
    }

    /// The agent an agent row shows.
    pub fn agent(&self, at: view::AgentAt, agent: usize) -> &Agent {
        match at {
            view::AgentAt::Tree(t) => &self.trees[t].agents[agent],
            view::AgentAt::Task(i) => &self.tasks[i].agents[agent],
        }
    }

    pub fn tree(&self, i: usize) -> &Tree {
        &self.trees[i]
    }

    pub fn mr(&self, i: usize) -> &Mr {
        &self.mrs[i]
    }

    /// The Jira page of `key`, fetched or not.
    pub fn ticket_url(&self, key: &str) -> String {
        format!("{}/browse/{key}", self.jira_host.trim_end_matches('/'))
    }
}
