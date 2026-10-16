//! Everything the table is built from, read from Jira, GitLab, git, herdr and
//! claude transcripts; as much of it at once as the dependencies allow.

use super::matching::Inputs;
use super::{Mr, Ticket, Tree, jira_client};
use crate::client::gitlab::{GitLab, MrReview};
use crate::client::jira::JiraClient;
use crate::client::worktree::{self, Worktree};
use crate::client::{herdr, sessions};
use crate::config::Config;
use crate::trace;
use crate::vault::task::{Task, ticket_keys};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::thread::ScopedJoinHandle;

/// Merged and closed MRs older than this are not fetched.
const MR_LOOKBACK_DAYS: i64 = 90;

/// What Jira and GitLab said last time, kept between runs.
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct RemoteCache {
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

    pub(super) fn load() -> Option<RemoteCache> {
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
        if let Ok(json) = serde_json::to_vec(&Ref { tickets, mrs })
            && std::fs::write(&tmp, json).is_ok()
        {
            let _ = std::fs::rename(tmp, path);
        }
    }
}

/// The value of a fetch thread; a panic in it becomes that source's error
/// instead of taking the whole table (and the TUI) down.
fn joined<T>(handle: ScopedJoinHandle<'_, T>, what: &str) -> Result<T, String> {
    handle
        .join()
        .map_err(|_| format!("{what}: the fetch panicked"))
}

/// `online: false` skips Jira and GitLab and git status; `cached` then stands
/// in for Jira and GitLab.
pub(super) fn fetch(
    config: &Config,
    tasks: Vec<Task>,
    online: bool,
    cached: Option<RemoteCache>,
) -> Inputs {
    let host = config.jira.host.as_str();
    let jira = online.then(|| jira_client(config));
    let gitlab = online.then(|| gitlab_client(config));
    // Jira answers in about a second, so every ticket query starts at once: mine,
    // the keys of open tasks and worktree branches, and those of open MRs as soon
    // as GitLab lists them
    let known: Vec<String> = match online {
        true => trace::time("known keys", || known_keys(config, &tasks)),
        false => Vec::new(),
    };
    let (mine, by_key, mrs, trees, herdr) = std::thread::scope(|scope| {
        let mine = scope.spawn(|| match &jira {
            Some(jira) => trace::time("jira: mine", || fetch_my_tickets(jira.as_ref()?, host)),
            None => Ok(HashMap::new()),
        });
        let by_key = scope.spawn(|| match &jira {
            Some(Ok(jira)) => trace::time(&format!("jira: {} known keys", known.len()), || {
                fetch_tickets_by_key(jira, host, &known)
            }),
            _ => HashMap::new(),
        });
        let mrs = scope.spawn(|| match &gitlab {
            Some(gitlab) => fetch_mrs(config, gitlab.as_ref()?, jira.as_ref(), &known),
            None => Ok((Vec::new(), HashMap::new())),
        });
        let herdr = scope.spawn(|| trace::time("herdr", live_herdr));
        let trees = trace::time("git: worktrees", || collect_trees(config, online));
        (
            joined(mine, "Jira").and_then(|r| r),
            joined(by_key, "Jira").unwrap_or_default(),
            joined(mrs, "GitLab").and_then(|r| r),
            trees,
            joined(herdr, "herdr").unwrap_or_default(),
        )
    });
    let (agents, workspaces) = herdr;
    let (mrs, of_mrs) = match mrs {
        Ok((mrs, tickets)) => (Ok(mrs), tickets),
        Err(err) => (Err(err), HashMap::new()),
    };

    // `mine` last: the same ticket fetched by key is not marked as mine
    let tickets = mine.map(|mine| {
        let mut tickets = by_key;
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
        warnings.push(format!(
            "Jira: {err}{}",
            if cached.is_some() {
                " — showing cached"
            } else {
                ""
            }
        ));
        cached.unwrap_or_default()
    });
    let mrs = mrs.unwrap_or_else(|err| {
        let cached = fallback.as_mut().map(|c| std::mem::take(&mut c.mrs));
        warnings.push(format!(
            "GitLab: {err}{}",
            if cached.is_some() {
                " — showing cached"
            } else {
                ""
            }
        ));
        cached.unwrap_or_default()
    });
    if complete {
        RemoteCache::save(&tickets, &mrs);
    }

    let roots = worktree_roots(config);
    let sessions = trace::time("sessions", || load_sessions(&roots, &trees, &tasks));
    Inputs {
        tasks,
        tickets,
        mrs,
        trees,
        agents,
        workspaces,
        sessions,
        roots,
        complete,
        warnings,
        jira_host: config.jira.host.clone(),
    }
}

fn gitlab_client(config: &Config) -> Result<GitLab, String> {
    let token = std::env::var("GITLAB_TOKEN").map_err(|_| "GITLAB_TOKEN is not set".to_string())?;
    GitLab::new(&config.gitlab.host, &token)
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
    let task_keys = tasks
        .iter()
        .filter(|t| !t.done)
        .flat_map(|t| t.tickets.clone());
    for key in task_keys.chain(branches.iter().flat_map(|b| ticket_keys(b))) {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// My tickets that are not done (resolution alone misses Declined/Cancelled).
fn fetch_my_tickets(jira: &JiraClient, host: &str) -> Result<HashMap<String, Ticket>, String> {
    let issues = jira
        .search_issues_brief("assignee = currentUser() AND statusCategory != Done", 200)
        .map_err(|err| err.to_string())?;
    Ok(issues
        .iter()
        .map(|issue| (issue.key.clone(), Ticket::from_issue(issue, host, true)))
        .collect())
}

/// Best effort: tickets that can't be fetched are shown by key only.
fn fetch_tickets_by_key(jira: &JiraClient, host: &str, keys: &[String]) -> HashMap<String, Ticket> {
    let mut tickets = HashMap::new();
    for chunk in keys.chunks(100) {
        match jira.search_issues_brief(&format!("key in ({})", chunk.join(",")), chunk.len()) {
            Ok(issues) => {
                for issue in &issues {
                    tickets.insert(issue.key.clone(), Ticket::from_issue(issue, host, false));
                }
            }
            // one unknown key fails the whole query: fall back to one request per
            // key, all at once — but not when Jira is down, that would wait out
            // every key
            Err(err) if err.bad_request() => {
                let found: Vec<Ticket> = std::thread::scope(|scope| {
                    let handles: Vec<_> = chunk
                        .iter()
                        .map(|key| scope.spawn(move || jira.get_issue_by_key(key).ok()))
                        .collect();
                    handles
                        .into_iter()
                        .filter_map(|h| h.join().ok().flatten())
                        .map(|issue| Ticket::from_issue(&issue, host, false))
                        .collect()
                });
                tickets.extend(found.into_iter().map(|t| (t.key.clone(), t)));
            }
            Err(_) => break,
        }
    }
    tickets
}

/// My MRs of every configured project. The open ones are listed first: their
/// review state and the tickets of their keys not in `known` load while GitLab
/// still lists the recently merged and closed ones, the large and slow list.
fn fetch_mrs(
    config: &Config,
    gitlab: &GitLab,
    jira: Option<&Result<JiraClient, String>>,
    known: &[String],
) -> Result<(Vec<Mr>, HashMap<String, Ticket>), String> {
    let mut projects: Vec<&str> = config
        .repos
        .iter()
        .map(|r| r.gitlab_project.as_str())
        .collect();
    if projects.is_empty() {
        projects.push(config.gitlab.project.as_str());
    }
    projects.dedup();

    std::thread::scope(|scope| {
        let recent = scope.spawn(|| {
            let _span = trace::span("gitlab: recent MRs");
            let mut found = Vec::new();
            for &project in &projects {
                let listed = gitlab.my_recent_merge_requests(project, MR_LOOKBACK_DAYS)?;
                found.extend(listed.into_iter().map(|mr| (project, mr)));
            }
            Ok::<_, String>(found)
        });

        let mut found: Vec<(&str, Mr)> = Vec::new();
        for &project in &projects {
            let listed = trace::time("gitlab: open MRs", || {
                gitlab.my_open_merge_requests(project)
            })?;
            found.extend(listed.into_iter().map(|mr| (project, Mr::new(mr))));
        }
        let mut keys: Vec<String> = Vec::new();
        for key in found.iter().flat_map(|(_, m)| &m.keys) {
            if !known.contains(key) && !keys.contains(key) {
                keys.push(key.clone());
            }
        }
        let tickets = scope.spawn(move || match jira {
            Some(Ok(jira)) => trace::time(&format!("jira: {} MR keys", keys.len()), || {
                fetch_tickets_by_key(jira, &config.jira.host, &keys)
            }),
            _ => HashMap::new(),
        });
        // review state of the open ones, all at once
        let reviews: Vec<Option<MrReview>> = std::thread::scope(|inner| {
            let _span = trace::span(format!("gitlab: review of {} open MRs", found.len()));
            let handles: Vec<_> = found
                .iter()
                .map(|(project, m)| {
                    let iid = m.mr.iid();
                    inner.spawn(move || gitlab.mr_review(project, iid))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().ok().and_then(Result::ok))
                .collect()
        });
        for ((_, mr), review) in found.iter_mut().zip(reviews) {
            mr.review = review;
        }

        for (project, mr) in joined(recent, "GitLab").and_then(|r| r)? {
            if !found
                .iter()
                .any(|(p, m)| *p == project && m.mr.iid() == mr.iid())
            {
                found.push((project, Mr::new(mr)));
            }
        }
        let tickets = joined(tickets, "Jira").unwrap_or_default();
        Ok((found.into_iter().map(|(_, m)| m).collect(), tickets))
    })
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
        // a worktree whose inspection panicked is left out
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
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
    // only `branch_finished` needs it, and only with unpushed commits
    let head = match (unpushed, wt.branch.as_deref()) {
        (Some(n), Some(branch)) if n > 0 => worktree::branch_head(&wt.path, branch).ok(),
        _ => None,
    };
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
        head,
        agents: Vec::new(),
        mr: None,
        finished: false,
        last_commit,
    }
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
