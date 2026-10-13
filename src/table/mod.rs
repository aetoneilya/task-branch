use crate::client::gitlab::{MergeRequest, MrReview, mr_review, my_merge_requests};
use crate::client::jira::{JiraClient, JiraIssue};
use crate::client::worktree::{self, Worktree};
use crate::client::{herdr, sessions};
use crate::config::Config;
use crate::vault::task::{SessionRef, Task, ticket_keys};
use serde_yaml::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

/// Merged and closed MRs older than this are not fetched.
const MR_LOOKBACK_DAYS: i64 = 90;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Ticket {
    pub key: String,
    pub summary: String,
    pub status: String,
    /// Jira status category: "new", "indeterminate" or "done".
    pub category: String,
    pub url: String,
    /// Assigned to me and not done.
    pub mine: bool,
    /// RFC 3339.
    pub created: String,
}

impl Ticket {
    fn from_issue(issue: &JiraIssue, host: &str, mine: bool) -> Ticket {
        Ticket {
            key: issue.key.clone(),
            summary: issue.fields.summary.clone(),
            status: issue.status().to_string(),
            category: issue.status_category().to_string(),
            url: format!("{}/browse/{}", host.trim_end_matches('/'), issue.key),
            mine,
            created: issue.fields.created.to_rfc3339(),
        }
    }

    pub fn closed(&self) -> bool {
        self.category == "done"
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Mr {
    pub mr: MergeRequest,
    /// Ticket keys in the source branch and the title.
    pub keys: Vec<String>,
    /// Pipeline, approvals and unresolved threads; open MRs only.
    pub review: Option<MrReview>,
}

impl Mr {
    pub fn opened(&self) -> bool {
        self.mr.state() == "opened"
    }
}

/// A linked worktree of one of the configured repositories.
#[derive(Debug)]
pub struct Tree {
    pub repo: PathBuf,
    pub wt: Worktree,
    pub keys: Vec<String>,
    pub changes: Option<usize>,
    pub unpushed: Option<usize>,
    pub agents: Vec<String>,
    /// herdr workspaces its agents run in.
    pub spaces: Vec<String>,
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
    /// Sessions found in the task's worktrees that the note does not list yet.
    pub new_sessions: Vec<SessionRef>,
    /// The task's herdr workspace: the one labelled with the task name.
    pub space: Option<String>,
    /// None when Jira or GitLab did not answer.
    pub state: Option<&'static str>,
}

/// Work on a ticket that belongs to no task.
#[derive(Debug)]
pub struct LooseTicket {
    pub key: String,
    pub trees: Vec<usize>,
    pub mrs: Vec<usize>,
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
    pub warnings: Vec<String>,
    pub jira_host: String,
}

/// `online: false` skips Jira and GitLab: worktrees, agents, workspaces and
/// sessions only, without task states — for commands that must answer fast.
pub fn build(config: &Config, tasks: Vec<Task>, online: bool) -> Table {
    build_with(config, tasks, online, None)
}

/// Offline, but with the tickets and MRs of the last online build: the TUI's
/// first frame shows them at once while the refresh runs.
pub fn build_cached(config: &Config, tasks: Vec<Task>) -> Table {
    build_with(config, tasks, false, RemoteCache::load())
}

/// What Jira and GitLab said last time, kept between runs.
#[derive(serde::Serialize, serde::Deserialize)]
struct RemoteCache {
    tickets: HashMap<String, Ticket>,
    mrs: Vec<Mr>,
}

impl RemoteCache {
    fn path() -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("worktable")
            .join("remote.json")
    }

    fn load() -> Option<RemoteCache> {
        serde_json::from_slice(&std::fs::read(RemoteCache::path()).ok()?).ok()
    }

    fn save(tickets: &HashMap<String, Ticket>, mrs: &[Mr]) {
        #[derive(serde::Serialize)]
        struct Ref<'a> {
            tickets: &'a HashMap<String, Ticket>,
            mrs: &'a [Mr],
        }
        let path = RemoteCache::path();
        let tmp = path.with_extension("json.tmp");
        if let Ok(json) = serde_json::to_vec(&Ref { tickets, mrs }) {
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }
}

fn build_with(config: &Config, tasks: Vec<Task>, online: bool, cached: Option<RemoteCache>) -> Table {
    // Jira answers in about a second, so every ticket query starts at once: mine,
    // the keys of open tasks and worktree branches, and those of open MRs as soon
    // as GitLab lists them
    let known: Vec<String> = if online { known_keys(config, &tasks) } else { Vec::new() };
    let (mine, known, (mrs, of_mrs), mut trees, (agents, spaces)) = std::thread::scope(|scope| {
        let mine = scope.spawn(|| match online {
            true => fetch_my_tickets(config),
            false => Ok(HashMap::new()),
        });
        let by_key = scope.spawn(|| {
            let keys: Vec<&String> = known.iter().collect();
            let mut tickets = HashMap::new();
            fetch_tickets_by_key(config, &keys, &mut tickets);
            tickets
        });
        let mrs = scope.spawn(|| {
            if !online {
                return (Ok(Vec::new()), HashMap::new());
            }
            let mrs = fetch_mrs(config);
            let mut keys: Vec<&String> = Vec::new();
            for key in mrs.iter().flatten().filter(|m| m.opened()).flat_map(|m| &m.keys) {
                if !known.contains(key) && !keys.contains(&key) {
                    keys.push(key);
                }
            }
            let mut tickets = HashMap::new();
            fetch_tickets_by_key(config, &keys, &mut tickets);
            (mrs, tickets)
        });
        let agents = scope.spawn(live_herdr);
        let trees = collect_trees(config, online);
        (
            mine.join().unwrap(),
            by_key.join().unwrap(),
            mrs.join().unwrap(),
            trees,
            agents.join().unwrap(),
        )
    });

    // `mine` last: the same ticket fetched by key is not marked as mine
    let tickets = mine.map(|mine| {
        let mut tickets = known;
        tickets.extend(of_mrs);
        tickets.extend(mine);
        tickets
    });

    let (tickets, mrs) = match cached {
        Some(cache) if !online => (Ok(cache.tickets), Ok(cache.mrs)),
        _ => (tickets, mrs),
    };
    let mut warnings = Vec::new();
    let complete = online && tickets.is_ok() && mrs.is_ok();
    // a service that did not answer is shown as it was last time, not as empty
    let mut fallback = (!complete && online).then(RemoteCache::load).flatten();
    let tickets = tickets.unwrap_or_else(|err| {
        let cached = fallback.as_mut().map(|c| std::mem::take(&mut c.tickets));
        warnings.push(format!("Jira: {err}{}", if cached.is_some() { " — showing cached" } else { "" }));
        cached.unwrap_or_default()
    });
    let mrs = mrs.unwrap_or_else(|err| {
        let cached = fallback.as_mut().map(|c| std::mem::take(&mut c.mrs));
        warnings.push(format!("GitLab: {err}{}", if cached.is_some() { " — showing cached" } else { "" }));
        cached.unwrap_or_default()
    });
    if complete {
        RemoteCache::save(&tickets, &mrs);
    }

    let roots = worktree_roots(config);
    let sessions = load_sessions(&roots, &trees, &tasks);

    for tree in &mut trees {
        let here = agents_in(&tree.wt.path, &agents, &sessions);
        tree.agents = here.iter().map(|a| a.label()).collect();
        tree.spaces = here.iter().map(|a| a.workspace_id.clone()).collect();
        let branch = tree.wt.branch.as_deref();
        // the open MR when there is one, otherwise the newest
        tree.mr = mrs
            .iter()
            .enumerate()
            .filter(|(_, m)| Some(m.mr.source_branch()) == branch)
            .max_by_key(|(_, m)| (m.opened(), m.mr.iid()))
            .map(|(i, _)| i);
        // a branch named without a ticket key still carries one in its MR title
        if tree.keys.is_empty() {
            if let Some(i) = tree.mr {
                tree.keys = mrs[i].keys.clone();
            }
        }
        tree.finished = match (tree.mr, tree.changes, tree.unpushed) {
            (Some(i), Some(0), Some(unpushed)) => branch_finished(&tree.wt, &mrs[i].mr, unpushed),
            _ => false,
        };
    }

    let mut owners: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, task) in tasks.iter().enumerate() {
        for key in &task.tickets {
            owners.entry(key.as_str()).or_default().push(i);
        }
    }
    let conflicts: Vec<(String, Vec<String>)> = owners
        .iter()
        .filter(|(_, idx)| idx.len() > 1)
        .map(|(key, idx)| {
            (
                key.to_string(),
                idx.iter().map(|&i| tasks[i].name.clone()).collect(),
            )
        })
        .collect();
    let mut owned: HashSet<String> = owners.keys().map(|k| k.to_string()).collect();

    let mut rows: Vec<TaskRow> = tasks
        .into_iter()
        .map(|task| {
            let space = spaces
                .iter()
                .find(|w| w.label.trim().eq_ignore_ascii_case(task.name.trim()))
                .map(|w| w.workspace_id.clone());
            let has_key = |keys: &[String]| keys.iter().any(|k| task.tickets.contains(k));
            // a worktree belongs to the task by its branch's ticket key, or because
            // an agent in the task's herdr workspace works in it
            let in_space = |tree: &Tree| space.as_ref().is_some_and(|s| tree.spaces.contains(s));
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
                new_sessions: Vec::new(),
                space,
                state: None,
            }
        })
        .collect();

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
    owned.extend(rows.iter().flat_map(|r| r.found_tickets.iter().cloned()));
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
        })
        .collect();

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
    let mut rows = rows;
    rows.sort_by(|a, b| {
        (task_rank(a), &b.task.created).cmp(&(task_rank(b), &a.task.created))
    });
    let loose_rank = |l: &LooseTicket| {
        let live: Vec<&Tree> = l.trees.iter().map(|&i| &trees[i]).filter(|t| !t.finished).collect();
        let open = l.mrs.iter().any(|&i| mrs[i].opened());
        match (live.is_empty(), open) {
            (true, false) => 2,
            (_, true) if live.iter().all(|t| t.clean()) => 1,
            _ => 0,
        }
    };
    let created = |l: &LooseTicket| tickets.get(&l.key).map(|t| t.created.clone()).unwrap_or_default();
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
        warnings,
        jira_host: config.jira.host.clone(),
    }
}

/// | state      | condition                                                          |
/// |------------|--------------------------------------------------------------------|
/// | `finished` | every ticket closed, every MR merged or closed, no local changes   |
/// | `review`   | MRs opened or merged, at least one opened, worktrees clean         |
/// | `active`   | a worktree, an open MR or a ticket in progress                     |
/// | `idea`     | nothing of the above                                               |
/// active, review, idea, finished, then done tasks.
fn task_rank(row: &TaskRow) -> u8 {
    if row.task.done {
        return 5;
    }
    let state = row.state.map(str::to_string).or_else(|| row.task.state.clone());
    match state.as_deref() {
        Some("active") => 0,
        Some("review") => 1,
        Some("finished") => 3,
        _ => 2,
    }
}

fn derive_state(
    row: &TaskRow,
    tickets: &HashMap<String, Ticket>,
    mrs: &[Mr],
    trees: &[Tree],
) -> &'static str {
    let task_tickets: Vec<Option<&Ticket>> = row
        .task
        .tickets
        .iter()
        .chain(&row.found_tickets)
        .map(|k| tickets.get(k))
        .collect();
    let row_mrs: Vec<&Mr> = row.mrs.iter().map(|&i| &mrs[i]).collect();
    let row_trees: Vec<&Tree> = row.trees.iter().map(|&i| &trees[i]).collect();

    let tickets_closed = task_tickets.iter().all(|t| t.is_some_and(Ticket::closed));
    let any_work = !task_tickets.is_empty() || !row_mrs.is_empty();
    let mrs_closed = row_mrs.iter().all(|m| !m.opened());
    let no_changes = row_trees.iter().all(|t| t.changes == Some(0));
    if any_work && tickets_closed && mrs_closed && no_changes {
        return "finished";
    }

    let mrs_in_review = row_mrs.iter().any(|m| m.opened())
        && row_mrs
            .iter()
            .all(|m| m.opened() || m.mr.state() == "merged");
    if mrs_in_review && row_trees.iter().all(|t| t.clean()) {
        return "review";
    }

    let in_progress = task_tickets
        .iter()
        .flatten()
        .any(|t| t.category == "indeterminate");
    if !row_trees.is_empty() || row_mrs.iter().any(|m| m.opened()) || in_progress {
        return "active";
    }
    "idea"
}

/// MR merged or closed, and the branch has nothing beyond it: either every commit
/// is on origin, or the local tip is the MR head (squash merges leave the branch
/// commits off master, and the remote branch is usually deleted).
pub fn branch_finished(wt: &Worktree, mr: &MergeRequest, unpushed: usize) -> bool {
    if mr.state() == "opened" {
        return false;
    }
    if unpushed == 0 {
        return true;
    }
    let head = wt
        .branch
        .as_deref()
        .and_then(|b| worktree::branch_head(&wt.path, b).ok());
    head.is_some() && head.as_deref() == mr.sha()
}

/// `<main checkout>/.claude/worktrees` of every configured repo.
fn worktree_roots(config: &Config) -> Vec<PathBuf> {
    config
        .repos
        .iter()
        .filter_map(|r| worktree::main_checkout(&r.path).ok())
        .map(|main| main.join(".claude/worktrees"))
        .collect()
}

/// Claude sessions launched in the main checkouts, in current worktrees and in
/// worktrees open tasks already list sessions for, by id.
fn load_sessions(
    roots: &[PathBuf],
    trees: &[Tree],
    tasks: &[Task],
) -> HashMap<String, sessions::Session> {
    let mut dirs: Vec<PathBuf> = roots
        .iter()
        .filter_map(|r| r.parent()?.parent().map(Path::to_path_buf))
        .collect();
    dirs.extend(trees.iter().map(|t| t.wt.path.clone()));
    for task in tasks.iter().filter(|t| !t.done) {
        for known in &task.sessions {
            dirs.extend(roots.iter().map(|r| r.join(&known.worktree)));
        }
    }
    dirs.sort();
    dirs.dedup();

    let mut cache = sessions::Cache::load();
    let mut found = HashMap::new();
    for dir in dirs {
        for session in sessions::sessions_in(&dir, &mut cache) {
            found.insert(session.id.clone(), session);
        }
    }
    cache.save();
    found
}

/// Sessions that worked in one of the task's worktrees — current ones, or ones
/// its note already lists sessions for — and are not in the note yet.
fn discover_sessions(
    row: &TaskRow,
    trees: &[Tree],
    roots: &[PathBuf],
    sessions: &HashMap<String, sessions::Session>,
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

/// Writes changed `state` and new `sessions` into the notes of open tasks.
/// Returns the names of updated notes; failures go to `table.warnings`.
pub fn write_back(table: &mut Table) -> Vec<String> {
    let _lock = match lock() {
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
        let mut changed = false;
        if let Some(state) = row.state {
            if row.task.state.as_deref() != Some(state) {
                row.task.note.set("state", Value::String(state.to_string()));
                changed = true;
            }
        }
        if !row.found_tickets.is_empty() {
            // the note's own entries stay as written; found keys go after them
            let entries = row
                .task
                .note
                .strings("tickets")
                .into_iter()
                .chain(row.found_tickets.iter().cloned())
                .map(Value::String)
                .collect();
            row.task.note.set("tickets", Value::Sequence(entries));
            changed = true;
        }
        if !row.new_sessions.is_empty() {
            let entries = row
                .task
                .sessions
                .iter()
                .chain(&row.new_sessions)
                .map(|s| Value::String(s.entry()))
                .collect();
            row.task.note.set("sessions", Value::Sequence(entries));
            changed = true;
        }
        if !changed {
            continue;
        }
        match row.task.note.save() {
            Ok(true) => {
                updated.push(row.task.name.clone());
                if let Some(state) = row.state {
                    row.task.state = Some(state.to_string());
                }
                row.task.sessions.append(&mut row.new_sessions);
                row.task.tickets.append(&mut row.found_tickets);
            }
            Ok(false) => {}
            Err(err) => table.warnings.push(err.to_string()),
        }
    }
    updated
}

/// Serialises note writes between concurrent `wt` processes.
pub fn lock() -> std::io::Result<File> {
    let dir = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("worktable");
    std::fs::create_dir_all(&dir)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("lock"))?;
    file.lock()?;
    Ok(file)
}

pub fn jira_client(config: &Config) -> Result<JiraClient, String> {
    let token = std::env::var("JIRA_TOKEN").map_err(|_| "JIRA_TOKEN is not set".to_string())?;
    Ok(JiraClient::new(config.jira.host.clone(), token))
}

/// Ticket keys of open tasks and of worktree branches: `git worktree list` is
/// instant, unlike the worktrees' status.
fn known_keys(config: &Config, tasks: &[Task]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let branches: Vec<String> = config
        .repos
        .iter()
        .filter_map(|r| worktree::main_checkout(&r.path).ok())
        .flat_map(|main| worktree::linked_worktrees(&main).unwrap_or_default())
        .filter_map(|wt| wt.branch)
        .collect();
    let task_keys = tasks.iter().filter(|t| !t.done).flat_map(|t| t.tickets.clone());
    for key in task_keys.chain(branches.iter().flat_map(|b| ticket_keys(b))) {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// My tickets that are not done (resolution alone misses Declined/Cancelled).
fn fetch_my_tickets(config: &Config) -> Result<HashMap<String, Ticket>, String> {
    let client = jira_client(config)?;
    let issues = client
        .search_issues_brief("assignee = currentUser() AND statusCategory != Done", 200)
        .map_err(|err| err.to_string())?;
    Ok(issues
        .iter()
        .map(|issue| {
            (
                issue.key.clone(),
                Ticket::from_issue(issue, &config.jira.host, true),
            )
        })
        .collect())
}

/// Best effort: tickets that can't be fetched are shown by key only.
fn fetch_tickets_by_key(config: &Config, keys: &[&String], tickets: &mut HashMap<String, Ticket>) {
    let Ok(client) = jira_client(config) else {
        return;
    };
    let host = &config.jira.host;
    for chunk in keys.chunks(100) {
        let list = chunk
            .iter()
            .map(|k| k.as_str())
            .collect::<Vec<_>>()
            .join(",");
        match client.search_issues_brief(&format!("key in ({list})"), chunk.len()) {
            Ok(issues) => {
                for issue in &issues {
                    tickets.insert(issue.key.clone(), Ticket::from_issue(issue, host, false));
                }
            }
            // one unknown key fails the whole query: fall back to one request per
            // key — but not when Jira is down, that would wait out every key
            Err(err) if err.bad_request() => {
                for key in chunk {
                    if let Ok(issue) = client.get_issue_by_key(key) {
                        tickets.insert(issue.key.clone(), Ticket::from_issue(&issue, host, false));
                    }
                }
            }
            Err(_) => return,
        }
    }
}

fn fetch_mrs(config: &Config) -> Result<Vec<Mr>, String> {
    let token = std::env::var("GITLAB_TOKEN").map_err(|_| "GITLAB_TOKEN is not set".to_string())?;
    let mut projects: Vec<&str> = config
        .repos
        .iter()
        .map(|r| r.gitlab_project.as_str())
        .collect();
    if projects.is_empty() {
        projects.push(config.gitlab.project.as_str());
    }
    projects.dedup();

    let mut mrs = Vec::new();
    for project in projects {
        let mut found: Vec<Mr> = Vec::new();
        for mr in my_merge_requests(&config.gitlab.host, &token, project, MR_LOOKBACK_DAYS)? {
            let mut keys = ticket_keys(mr.source_branch());
            for key in ticket_keys(mr.title()) {
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
            found.push(Mr {
                mr,
                keys,
                review: None,
            });
        }
        // review state of the open ones, all at once
        std::thread::scope(|scope| {
            let handles: Vec<_> = found
                .iter()
                .enumerate()
                .filter(|(_, m)| m.opened())
                .map(|(i, m)| {
                    let (host, token, iid) = (&config.gitlab.host, &token, m.mr.iid());
                    (i, scope.spawn(move || mr_review(host, token, project, iid)))
                })
                .collect();
            for (i, handle) in handles {
                found[i].review = handle.join().ok().and_then(Result::ok);
            }
        });
        mrs.extend(found);
    }
    Ok(mrs)
}

/// Linked worktrees of every configured repo; git status and the unpushed count
/// take ~0.5s per worktree in a big repo, so they run in parallel.
fn collect_trees(config: &Config, with_status: bool) -> Vec<Tree> {
    let mut found: Vec<(PathBuf, Worktree)> = Vec::new();
    for repo in &config.repos {
        let Ok(main) = worktree::main_checkout(&repo.path) else {
            continue;
        };
        for wt in worktree::linked_worktrees(&main).unwrap_or_default() {
            found.push((main.clone(), wt));
        }
    }

    std::thread::scope(|scope| {
        let handles: Vec<_> = found
            .into_iter()
            .map(|(repo, wt)| scope.spawn(move || inspect_tree(repo, wt, with_status)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

/// `git status` is the slow part; offline callers skip it.
fn inspect_tree(repo: PathBuf, wt: Worktree, with_status: bool) -> Tree {
    let changes = with_status
        .then(|| worktree::changed_files(&wt.path).ok())
        .flatten();
    let unpushed = with_status
        .then(|| {
            wt.branch
                .as_deref()
                .and_then(|b| worktree::unpushed_commits(&wt.path, b).ok())
        })
        .flatten();
    let keys = wt.branch.as_deref().map(ticket_keys).unwrap_or_default();
    let last_commit = with_status
        .then(|| worktree::last_commit_time(&wt.path).ok())
        .flatten();
    Tree {
        repo,
        wt,
        keys,
        changes,
        unpushed,
        agents: Vec::new(),
        spaces: Vec::new(),
        mr: None,
        finished: false,
        last_commit,
    }
}

/// Agents working in `dir`: where their claude session last worked, else the
/// pane's cwd — agents started in the main checkout move into worktrees.
fn agents_in<'a>(
    dir: &Path,
    agents: &'a [herdr::Agent],
    sessions: &HashMap<String, sessions::Session>,
) -> Vec<&'a herdr::Agent> {
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
        .collect()
}

fn live_herdr() -> (Vec<herdr::Agent>, Vec<herdr::Workspace>) {
    if !herdr::available() {
        return (Vec::new(), Vec::new());
    }
    (
        herdr::list_agents().unwrap_or_default(),
        herdr::list_workspaces().unwrap_or_default(),
    )
}

impl Table {
    /// Fresh agent states without rebuilding: herdr answers in milliseconds, so
    /// the TUI polls it far more often than Jira and GitLab. Worktrees keep their
    /// task; only who works in them and how changes.
    pub fn update_agents(&mut self, agents: &[herdr::Agent]) {
        for tree in &mut self.trees {
            tree.agents = agents_in(&tree.wt.path, agents, &self.sessions)
                .iter()
                .map(|a| a.label())
                .collect();
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
        match self.tickets.get(key) {
            Some(ticket) => ticket.url.clone(),
            None => format!("{}/browse/{key}", self.jira_host.trim_end_matches('/')),
        }
    }
}
