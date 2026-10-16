//! Everything the table is built from, read from Jira, GitLab, git, herdr and
//! claude transcripts; as much of it at once as the dependencies allow.

use super::matching::Inputs;
use super::{Fresh, Mr, Source, Ticket, Tree};
use crate::client::gitlab::{GitLab, MrReview};
use crate::client::jira::{JiraClient, JiraIssue};
use crate::client::worktree::{self, Worktree};
use crate::client::{herdr, joined, sessions};
use crate::config::Config;
use crate::trace;
use crate::vault::task::{Task, ticket_keys};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Merged and closed MRs older than this are not fetched.
const MR_LOOKBACK_DAYS: i64 = 90;

/// Tickets by key, or why Jira did not give them.
type Tickets = Result<HashMap<String, Ticket>, String>;

/// What Jira and GitLab said last time, kept between runs.
#[derive(serde::Serialize, serde::Deserialize)]
struct RemoteCache {
    tickets: HashMap<String, Ticket>,
    mrs: Vec<Mr>,
}

impl RemoteCache {
    fn path() -> PathBuf {
        crate::config::cache_dir().join("remote.json")
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
        if let Ok(json) = serde_json::to_vec(&Ref { tickets, mrs })
            && std::fs::write(&tmp, json).is_ok()
        {
            let _ = std::fs::rename(tmp, path);
        }
    }
}

/// `online: false` skips Jira and GitLab and git status. `cached`: what Jira
/// and GitLab said last time stands in for what they do not say now, offline or
/// failing.
pub(super) fn fetch(config: &Config, tasks: Vec<Task>, online: bool, cached: bool) -> Inputs {
    let jira = online.then(|| JiraClient::from_env(&config.jira.host));
    let gitlab = online.then(|| GitLab::from_env(&config.gitlab.host));
    // Jira answers in about a second, so every ticket query starts at once: mine,
    // the keys of open tasks and worktree branches, and those of open MRs as soon
    // as GitLab lists them
    let known: Vec<String> = if online {
        trace::time("known keys", || known_keys(config, &tasks))
    } else {
        Vec::new()
    };
    let (mine, by_key, mrs, trees, herdr) = std::thread::scope(|scope| {
        let mine = scope.spawn(|| match &jira {
            Some(jira) => trace::time("jira: mine", || fetch_my_tickets(jira.as_ref()?)),
            None => Ok(HashMap::new()),
        });
        let by_key = scope.spawn(|| match &jira {
            Some(Ok(jira)) => trace::time(&format!("jira: {} known keys", known.len()), || {
                fetch_tickets_by_key(jira, &known)
            }),
            _ => Ok(HashMap::new()),
        });
        let mrs = scope.spawn(|| match &gitlab {
            Some(gitlab) => fetch_mrs(config, gitlab.as_ref()?, jira.as_ref(), &known),
            None => Ok((Vec::new(), Ok(HashMap::new()))),
        });
        let herdr = scope.spawn(|| trace::time("herdr", live_herdr));
        let trees = trace::time("git: worktrees", || collect_trees(config, online));
        (
            joined(mine, "the fetch"),
            joined(by_key, "the fetch"),
            joined(mrs, "the fetch"),
            trees,
            herdr.join().unwrap_or_default(),
        )
    });
    let (agents, workspaces) = herdr;
    let (mrs, of_mrs) = match mrs {
        Ok((mrs, tickets)) => (Ok(mrs), tickets),
        Err(err) => (Err(err), Ok(HashMap::new())),
    };
    let tickets = all_tickets(mine, by_key, of_mrs);

    let mut warnings = Vec::new();
    let complete = online && tickets.is_ok() && mrs.is_ok();
    let mut cache = (cached && !complete).then(RemoteCache::load).flatten();
    let (tickets, jira) = or_cached(
        "Jira",
        online.then_some(tickets),
        cache.as_mut().map(|c| std::mem::take(&mut c.tickets)),
        &mut warnings,
    );
    let (mrs, gitlab) = or_cached(
        "GitLab",
        online.then_some(mrs),
        cache.map(|c| c.mrs),
        &mut warnings,
    );
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
        jira,
        gitlab,
        warnings,
        jira_host: config.jira.host.clone(),
    }
}

/// Every ticket Jira was asked for, or why not all of them: a ticket missing for
/// a failed query would read as unknown, and so would the state of its task.
/// `mine` goes last: the same ticket fetched by key is not marked as mine.
fn all_tickets(mine: Tickets, by_key: Tickets, of_mrs: Tickets) -> Tickets {
    let mine = mine?;
    let mut tickets = by_key?;
    tickets.extend(of_mrs?);
    tickets.extend(mine);
    Ok(tickets)
}

/// A service that did not answer is shown as it was last time, not as empty,
/// with a warning either way; one not asked (`fetched: None`) as it was last
/// time, without one.
fn or_cached<T: Default>(
    service: &str,
    fetched: Option<Result<T, String>>,
    cached: Option<T>,
    warnings: &mut Vec<String>,
) -> (T, Source) {
    let fresh = if cached.is_some() {
        Fresh::Cached
    } else {
        Fresh::Missing
    };
    let warning = match fetched {
        Some(Ok(value)) => {
            let source = Source {
                fresh: Fresh::Live,
                warning: None,
            };
            return (value, source);
        }
        Some(Err(err)) => {
            let shown = if cached.is_some() {
                " — showing cached"
            } else {
                ""
            };
            let warning = format!("{service}: {err}{shown}");
            warnings.push(warning.clone());
            Some(warning)
        }
        None => None,
    };
    (cached.unwrap_or_default(), Source { fresh, warning })
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
fn fetch_my_tickets(jira: &JiraClient) -> Tickets {
    let issues = jira
        .search_issues_brief("assignee = currentUser() AND statusCategory != Done", 200)
        .map_err(|err| err.to_string())?;
    Ok(issues
        .iter()
        .map(|issue| (issue.key.clone(), Ticket::from_issue(issue, true)))
        .collect())
}

/// The tickets of `keys` that exist; keys Jira does not know are left out, but a
/// query that fails otherwise fails them all.
fn fetch_tickets_by_key(jira: &JiraClient, keys: &[String]) -> Tickets {
    let mut tickets = HashMap::new();
    for chunk in keys.chunks(100) {
        let jql = format!("key in ({})", chunk.join(","));
        let issues = match jira.search_issues_brief(&jql, chunk.len()) {
            Ok(issues) => issues,
            // one unknown key fails the whole query: fall back to one request per
            // key, all at once — but not when Jira is down, that would wait out
            // every key
            Err(err) if err.bad_request() => std::thread::scope(|scope| {
                let handles: Vec<_> = chunk
                    .iter()
                    .map(|key| scope.spawn(move || existing(jira.get_issue_by_key(key))))
                    .collect();
                handles
                    .into_iter()
                    .map(|h| joined(h, "a Jira request"))
                    .collect::<Result<Vec<_>, _>>()
            })?
            .into_iter()
            .flatten()
            .collect(),
            Err(err) => return Err(err.to_string()),
        };
        for issue in &issues {
            tickets.insert(issue.key.clone(), Ticket::from_issue(issue, false));
        }
    }
    Ok(tickets)
}

/// The issue Jira answered with, None for a key it does not know (404, or 400).
fn existing(answer: crate::client::jira::Result<JiraIssue>) -> Result<Option<JiraIssue>, String> {
    match answer {
        Ok(issue) => Ok(Some(issue)),
        Err(err) if err.bad_request() || err.not_found() => Ok(None),
        Err(err) => Err(err.to_string()),
    }
}

/// My MRs of every configured project. The open ones are listed first: their
/// review state and the tickets of their keys not in `known` load while GitLab
/// still lists the recently merged and closed ones, the large and slow list.
fn fetch_mrs(
    config: &Config,
    gitlab: &GitLab,
    jira: Option<&Result<JiraClient, String>>,
    known: &[String],
) -> Result<(Vec<Mr>, Tickets), String> {
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
                fetch_tickets_by_key(jira, &keys)
            }),
            _ => Ok(HashMap::new()),
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

        for (project, mr) in joined(recent, "the fetch")? {
            if !found
                .iter()
                .any(|(p, m)| *p == project && m.mr.iid() == mr.iid())
            {
                found.push((project, Mr::new(mr)));
            }
        }
        let tickets = joined(tickets, "the fetch");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::jira::JiraError;
    use crate::table::Category;
    use crate::table::fixtures::ticket;
    use reqwest::StatusCode;

    fn tickets(keys: &[&str]) -> Tickets {
        Ok(keys
            .iter()
            .map(|k| (k.to_string(), ticket(k, Category::New, false)))
            .collect())
    }

    #[test]
    fn jira_answered_only_when_every_ticket_query_did() {
        let all = all_tickets(
            tickets(&["UCP-1"]),
            tickets(&["UCP-2"]),
            tickets(&["UCP-3"]),
        );
        let mut keys: Vec<String> = all.unwrap().into_keys().collect();
        keys.sort();
        assert_eq!(keys, vec!["UCP-1", "UCP-2", "UCP-3"]);
        let down = || Err("timed out".to_string());
        assert!(all_tickets(tickets(&["UCP-1"]), down(), tickets(&["UCP-3"])).is_err());
        assert!(all_tickets(tickets(&["UCP-1"]), tickets(&["UCP-2"]), down()).is_err());
    }

    #[test]
    fn a_failed_request_for_one_key_fails_the_query() {
        let refused = |status| JiraError::RequestResultError(status, String::new());
        assert!(matches!(
            existing(Err(refused(StatusCode::NOT_FOUND))),
            Ok(None)
        ));
        assert!(matches!(
            existing(Err(refused(StatusCode::BAD_REQUEST))),
            Ok(None)
        ));
        assert!(existing(Err(refused(StatusCode::BAD_GATEWAY))).is_err());
    }

    #[test]
    fn a_service_that_did_not_answer_shows_the_cache() {
        let mut warnings = Vec::new();
        let (value, source) = or_cached("GitLab", Some(Ok(1)), Some(2), &mut warnings);
        assert_eq!((value, source.fresh), (1, Fresh::Live));
        let (value, source) = or_cached("GitLab", Some(Err("down".into())), Some(2), &mut warnings);
        assert_eq!((value, source.fresh), (2, Fresh::Cached));
        assert_eq!(
            source.warning.as_deref(),
            Some("GitLab: down — showing cached")
        );
        let (value, source) =
            or_cached::<i32>("Jira", Some(Err("down".into())), None, &mut warnings);
        assert_eq!((value, source.fresh), (0, Fresh::Missing));
        assert_eq!(
            warnings,
            vec!["GitLab: down — showing cached", "Jira: down"]
        );
        // offline: the cache, silently
        let (value, source) = or_cached("Jira", None, Some(3), &mut warnings);
        assert_eq!(
            (value, source),
            (
                3,
                Source {
                    fresh: Fresh::Cached,
                    warning: None
                }
            )
        );
        assert_eq!(warnings.len(), 2);
    }
}
