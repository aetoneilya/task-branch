use super::progress;
use super::slugify;
use super::table::{load, vault};
use crate::client::jira::JiraIssue;
use crate::client::{herdr, worktree};
use crate::config::Config;
use crate::table::{self, Table, TaskRow};
use crate::vault::note::{Note, NoteError};
use crate::vault::task::{SessionRef, Task, ticket_keys};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, FuzzySelect, Input};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

const DONE_DEFINITION: &str = "\
Если меняешь код — готово, когда:
- изменения компилируются, тесты затронутых модулей запущены и проходят — приложи вывод;
- ничего не закоммичено и не запушено: дифф сначала смотрит Илья;
- в конце короткое резюме: что изменено, чем проверено, открытые вопросы.
Если задача неоднозначна или затрагивает больше пары файлов — начни с плана и согласуй его.";

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn is_ticket_key(text: &str) -> bool {
    ticket_keys(text).first().is_some_and(|k| k == text)
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// `wt start <task name | KEY>`: a new task (note, herdr workspace, worktree tab
/// with claude), or work on a ticket without a note.
#[allow(clippy::too_many_arguments)]
pub(super) fn cli_start(
    config: &Config,
    target: Option<String>,
    tickets: Vec<String>,
    tags: Vec<String>,
    message: Option<String>,
    name: Option<String>,
    branch: Option<String>,
    bg: bool,
) -> Result<(), String> {
    let tasks = load(config)?;
    let target = match target {
        Some(target) => target,
        None => pick_start_target(config, &tasks)?,
    };

    if is_ticket_key(&target) {
        if let Some(task) = tasks.iter().find(|t| t.tickets.contains(&target)) {
            println!("{target} is already in task \"{}\" — adding it there", task.name);
            let args = vec![task.name.clone(), target];
            return cli_add(config, args, None, name, message, bg);
        }
        // work always gets a task: a note for shared context and agents' findings,
        // named after the ticket
        let issue = fetch_issue(config, &target)?;
        let title = note_title(&issue.fields.summary);
        if title.is_empty() {
            return Err(format!("{target} has no summary to name the task after"));
        }
        let mut keys = vec![target.clone()];
        keys.extend(tickets.into_iter().filter(|k| k != &target));
        return cli_start(config, Some(title), keys, tags, message, name, branch, bg);
    }

    if let Some(task) = tasks.iter().find(|t| same_name(&t.name, &target)) {
        return Err(format!(
            "task \"{0}\" already exists: wt select \"{0}\" to get back to it, wt add \"{0}\" <KEY> to add a ticket",
            task.name
        ));
    }
    let similar = similar_tasks(&tasks, &target, &tags);
    if !similar.is_empty() && std::io::stdin().is_terminal() {
        println!("similar tasks: {}", similar.join(", "));
        if !confirm("Create a new one anyway?")? {
            return Err("not created: wt select / wt add if it is one of them".to_string());
        }
    }

    let keys: Vec<String> = tickets.iter().flat_map(|t| ticket_keys(t)).collect();
    let issue = keys.first().map(|k| fetch_issue(config, k)).transpose()?;
    let wt_name = match (name, &issue) {
        (Some(name), _) => name,
        (None, Some(issue)) => name_from_issue(issue),
        (None, None) => name_from_text(&target)
            .ok_or("no latin worktree name comes out of the title — give one with -n")?,
    };
    let branch = branch.unwrap_or_else(|| branch_name(config, keys.first(), &wt_name));
    let main = current_repo(config)?;

    let vault = vault(config)?;
    let path = vault
        .path
        .join(&vault.new_task_dir)
        .join(format!("{target}.md"));
    let mut frontmatter = Vec::new();
    if !keys.is_empty() {
        frontmatter.push(("tickets", list(&keys)));
    }
    if !tags.is_empty() {
        frontmatter.push(("tags", list(&tags)));
    }
    frontmatter.push(("done", Value::Bool(false)));
    let body = message
        .as_deref()
        .map(|m| format!("{m}\n"))
        .unwrap_or_default();
    let note = Note::create(&path, &frontmatter, &body).map_err(err)?;
    let task = Task::from_note(note);
    println!("note: {}", path.display());

    let tree = worktree::worktree_dir(&main, &wt_name);
    let brief = brief_text(
        &tree,
        &branch,
        Some(&task),
        issue.as_ref(),
        &keys,
        message.as_deref(),
    );
    open_work(
        &task.name,
        &main,
        &wt_name,
        &branch,
        brief,
        Some(&task.note.path),
        !bg,
    )
}

/// `wt add [<task>] <KEY>`: the ticket joins the task and gets a tab in the
/// task's herdr workspace, in a new worktree or `--in` one the task has.
pub(super) fn cli_add(
    config: &Config,
    args: Vec<String>,
    in_tree: Option<String>,
    name: Option<String>,
    message: Option<String>,
    bg: bool,
) -> Result<(), String> {
    let (task_arg, key) = match args.as_slice() {
        [key] => (None, key.clone()),
        [task, key] => (Some(task.clone()), key.clone()),
        _ => return Err("wt add [<task>] <KEY>".to_string()),
    };
    if !is_ticket_key(&key) {
        return Err(format!("\"{key}\" is not a ticket key: wt add [<task>] <KEY>"));
    }
    let mut task = resolve_task(config, task_arg.as_deref())?;

    if !task.tickets.contains(&key) {
        let _lock = table::lock().map_err(err)?;
        task.tickets.push(key.clone());
        task.note.set("tickets", list(&task.tickets));
        task.note.save().map_err(err)?;
        println!("{key} added to \"{}\"", task.name);
    }

    let issue = fetch_issue(config, &key)?;
    let main = current_repo(config)?;
    let (tree, branch, wt_name) = match in_tree {
        Some(existing) => {
            let dir = worktree::worktree_dir(&main, &existing);
            if !dir.exists() {
                return Err(format!("no worktree {}", dir.display()));
            }
            let branch = crate::client::git::current_branch(&dir).map_err(err)?;
            (dir, branch, existing)
        }
        None => {
            let wt_name = name.unwrap_or_else(|| name_from_issue(&issue));
            let branch = branch_name(config, Some(&key), &wt_name);
            (worktree::worktree_dir(&main, &wt_name), branch, wt_name)
        }
    };
    let brief = brief_text(
        &tree,
        &branch,
        Some(&task),
        Some(&issue),
        &task.tickets,
        message.as_deref(),
    );
    open_work(
        &task.name,
        &main,
        &wt_name,
        &branch,
        brief,
        Some(&task.note.path),
        !bg,
    )
}

enum Work {
    Task(usize),
    Ticket(usize),
}

/// `wt select [<task | KEY>]`: back to existing work — focus its agent, or start
/// claude in its worktree resuming the last session, restoring the worktree if
/// it was removed.
pub(super) fn cli_select(
    config: &Config,
    target: Option<String>,
    tree_name: Option<String>,
    pick_session: bool,
    bg: bool,
) -> Result<(), String> {
    let table = table::build(config, load(config)?, false);
    let key = target.clone().filter(|t| is_ticket_key(t));
    let work = match target {
        Some(target) => find_work(&table, &target)
            .ok_or_else(|| format!("no task or ticket \"{target}\""))?,
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
    if let Some(key) = &key {
        if trees.iter().any(|&i| table.tree(i).keys.contains(key)) {
            trees.retain(|&i| table.tree(i).keys.contains(key));
        }
    }
    trees.sort_by_key(|&i| std::cmp::Reverse(!table.tree(i).agents.is_empty()));
    let tree = if trees.len() == 1 {
        table.tree(trees[0])
    } else {
        let items: Vec<String> = trees
            .iter()
            .map(|&i| {
                let t = table.tree(i);
                format!("{}  {}", t.wt.name(), t.agents.join(", "))
            })
            .collect();
        table.tree(trees[fuzzy("Worktree", &items)?])
    };

    if let Some(label) = tree.agents.first() {
        let agent = label.rsplit_once(':').map_or(label.as_str(), |(n, _)| n);
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

/// `wt note "<text>"`: an agent's finding, appended to `## Agent notes` of the
/// task it works on (by its worktree, or by its herdr workspace).
pub(super) fn cli_note(config: &Config, text: Vec<String>) -> Result<(), String> {
    let text = text.join(" ").trim().to_string();
    if text.is_empty() {
        return Err("wt note \"<text>\"".to_string());
    }
    let mut table = table::build(config, load(config)?, false);
    let index = current_task(&table)
        .ok_or("no task here: run it from the task's worktree or herdr workspace")?;
    let mut task = table.tasks.swap_remove(index).task;

    let agent = std::env::var("HERDR_PANE_ID")
        .ok()
        .and_then(|pane| {
            herdr::list_agents()
                .ok()?
                .into_iter()
                .find(|a| a.pane_id == pane)?
                .name
        })
        .unwrap_or_else(|| "agent".to_string());
    let line = format!(
        "{} {agent}: {text}",
        chrono::Local::now().format("%Y-%m-%d")
    );

    let _lock = table::lock().map_err(err)?;
    for _ in 0..3 {
        task.note.append_agent_note(&line);
        match task.note.save() {
            Ok(_) => {
                println!("written to \"{}\"", task.name);
                return Ok(());
            }
            Err(NoteError::Changed(_)) => task.note.reload().map_err(err)?,
            Err(e) => return Err(err(e)),
        }
    }
    Err(format!("\"{}\" keeps changing, not written", task.name))
}

/// The open task of the current worktree, or of the current herdr workspace.
fn current_task(table: &Table) -> Option<usize> {
    let open = |r: &TaskRow| !r.task.done;
    if let Some(root) = std::env::current_dir()
        .ok()
        .and_then(|d| worktree::worktree_root(&d).ok())
    {
        let by_tree = table
            .tasks
            .iter()
            .position(|r| open(r) && r.trees.iter().any(|&t| table.tree(t).wt.path == root));
        if by_tree.is_some() {
            return by_tree;
        }
    }
    let space = std::env::var("HERDR_WORKSPACE_ID").ok()?;
    table
        .tasks
        .iter()
        .position(|r| open(r) && r.space.as_deref() == Some(space.as_str()))
}

/// A task by name (exact, then a unique partial match), otherwise the current
/// one, otherwise a picker.
fn resolve_task(config: &Config, arg: Option<&str>) -> Result<Task, String> {
    let mut tasks = load(config)?;
    if let Some(arg) = arg {
        if let Some(i) = tasks.iter().position(|t| same_name(&t.name, arg)) {
            return Ok(tasks.swap_remove(i));
        }
        let needle = arg.to_lowercase();
        let partial: Vec<usize> = (0..tasks.len())
            .filter(|&i| !tasks[i].done && tasks[i].name.to_lowercase().contains(&needle))
            .collect();
        return match partial.as_slice() {
            [i] => Ok(tasks.swap_remove(*i)),
            [] => Err(format!("no task \"{arg}\": wt start \"{arg}\" creates it")),
            _ => Err(format!("\"{arg}\" matches several tasks, be more specific")),
        };
    }
    let mut table = table::build(config, tasks, false);
    if let Some(i) = current_task(&table) {
        return Ok(table.tasks.swap_remove(i).task);
    }
    let open: Vec<usize> = (0..table.tasks.len())
        .filter(|&i| !table.tasks[i].task.done)
        .collect();
    let items: Vec<String> = open
        .iter()
        .map(|&i| table.tasks[i].task.name.clone())
        .collect();
    let chosen = open[fuzzy("Task", &items)?];
    Ok(table.tasks.swap_remove(chosen).task)
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
        let agents: Vec<&String> = row
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

/// My open Jira tickets that belong to no task, plus "new task…".
fn pick_start_target(config: &Config, tasks: &[Task]) -> Result<String, String> {
    let client = table::jira_client(config)?;
    let issues = client
        .search_issues_brief(
            "assignee = currentUser() AND statusCategory != Done ORDER BY updated DESC",
            50,
        )
        .map_err(err)?;
    let free: Vec<&JiraIssue> = issues
        .iter()
        .filter(|i| !tasks.iter().any(|t| t.tickets.contains(&i.key)))
        .collect();
    let mut items = vec!["+ new task…".to_string()];
    items.extend(
        free.iter()
            .map(|i| format!("{}  {}", i.key, i.fields.summary)),
    );
    match fuzzy("Start", &items)? {
        0 => Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Task name")
            .interact_text()
            .map_err(err),
        i => Ok(free[i - 1].key.clone()),
    }
}

fn similar_tasks(tasks: &[Task], name: &str, tags: &[String]) -> Vec<String> {
    let words: Vec<String> = name
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_string)
        .collect();
    tasks
        .iter()
        .filter(|t| !t.done)
        .filter(|t| {
            let title = t.name.to_lowercase();
            t.tags.iter().any(|tag| tags.contains(tag))
                || words.iter().any(|w| title.contains(w.as_str()))
        })
        .map(|t| t.name.clone())
        .collect()
}

/// A fresh claude in a tab of the herdr workspace `space`, pointed at the task
/// note and the brief: it reads them and waits — the work starts with analysis,
/// on my command.
fn launch(
    space: &str,
    cwd: &Path,
    label: &str,
    note: Option<&Path>,
    brief: &Path,
    focus: bool,
) -> Result<(), String> {
    let prompt = agent_prompt(note, brief);
    let args: Vec<&str> = prompt.as_deref().into_iter().collect();
    open_agent(space, cwd, label, &args, focus)
}

/// Points claude at the task note and the brief; it reads them and waits.
fn agent_prompt(note: Option<&Path>, brief: &Path) -> Option<String> {
    let mut context = Vec::new();
    if let Some(note) = note {
        context.push(format!("заметка задачи {}", note.display()));
    }
    if brief.exists() {
        context.push(format!("бриф с тикетом {}", brief.display()));
    }
    (!context.is_empty()).then(|| {
        format!(
            "Контекст: {}. Прочитай и жди моей команды.",
            context.join(", ")
        )
    })
}

/// What `wt prepare` does in the new tab, handed over in a file.
#[derive(Serialize, Deserialize)]
struct PrepareSpec {
    main: PathBuf,
    name: String,
    branch: String,
    brief: String,
    note: Option<PathBuf>,
    pane: String,
    agent: String,
}

/// A tab for the worktree `name` in the herdr workspace `space`. Fetch and
/// checkout of a big repo take half a minute, so they run in the tab itself:
/// `wt prepare` shows the progress there, writes the brief and becomes claude,
/// and `wt start` returns at once. Without herdr it all happens here.
fn open_work(
    space: &str,
    main: &Path,
    name: &str,
    branch: &str,
    brief: String,
    note: Option<&Path>,
    focus: bool,
) -> Result<(), String> {
    let dir = worktree::worktree_dir(main, name);
    if !herdr::available() {
        let dir = worktree::create_worktree(main, name, branch).map_err(err)?;
        println!("worktree: {} ({branch})", dir.display());
        let brief = write_brief(&dir, &brief)?;
        let prompt = agent_prompt(note, &brief).unwrap_or_default();
        println!("not in herdr — run it yourself: cd {} && claude '{prompt}'", dir.display());
        return Ok(());
    }

    let cwd = if dir.exists() { &dir } else { main };
    let (tab, pane) = open_tab(space, cwd, name)?;
    let agent = free_agent_name(name);
    let spec = PrepareSpec {
        main: main.to_path_buf(),
        name: name.to_string(),
        branch: branch.to_string(),
        brief,
        note: note.map(Path::to_path_buf),
        pane: pane.clone(),
        agent: agent.clone(),
    };
    let spec_path = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("worktable")
        .join(format!("prepare-{pane}.json").replace(':', "-"));
    if let Some(parent) = spec_path.parent() {
        std::fs::create_dir_all(parent).map_err(err)?;
    }
    std::fs::write(&spec_path, serde_json::to_vec(&spec).map_err(err)?).map_err(err)?;

    let exe = std::env::current_exe().map_err(err)?;
    let command = format!(
        "{} prepare {} && cd {}",
        sh_quote(&exe),
        sh_quote(&spec_path),
        sh_quote(&dir)
    );
    herdr::run_in_pane(&pane, &command).map_err(err)?;
    if focus {
        let _ = herdr::focus_tab(&tab);
    }
    println!("tab {name} → workspace \"{space}\": the worktree ({branch}) is being prepared there, then claude {agent} starts");
    Ok(())
}

/// `wt prepare <spec>`, run by `open_work` in the new tab: creates the worktree
/// with progress on screen, writes the brief and execs claude in the worktree.
pub(super) fn cli_prepare(spec_path: &Path) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let spec: PrepareSpec =
        serde_json::from_slice(&std::fs::read(spec_path).map_err(err)?).map_err(err)?;
    let dir = worktree::worktree_dir(&spec.main, &spec.name);
    let started = std::time::Instant::now();

    if !dir.exists() {
        println!("{} ({})", dir.display(), spec.branch);
        let plan = progress::step("worktree plan", || {
            worktree::plan_worktree(&spec.main, &spec.name, &spec.branch)
        })?;
        if let Some(remote_branch) = &plan.fetch {
            progress::step(&format!("git fetch origin {remote_branch}"), || {
                worktree::fetch(&spec.main, remote_branch)
            })?;
        }
        progress::loud_step("git worktree add", || {
            worktree::add_worktree(&spec.main, &plan, true)
        })?;
        worktree::copy_worktree_includes(&spec.main, &dir);
    }
    let brief = write_brief(&dir, &spec.brief)?;
    println!("ready in {:.0} s, starting claude", started.elapsed().as_secs_f64());

    let _ = std::fs::remove_file(spec_path);
    let _ = herdr::name_agent_when_up(&spec.pane, &spec.agent);
    std::env::set_current_dir(&dir).map_err(err)?;
    let mut claude = std::process::Command::new("claude");
    claude.args(agent_prompt(spec.note.as_deref(), &brief));
    Err(format!("could not start claude: {}", claude.exec()))
}

fn sh_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

/// Opens `cwd` as a tab in the herdr workspace `space` (created when missing),
/// starts a named claude there with `claude_args` and focuses it unless `focus`
/// is false.
fn open_agent(
    space: &str,
    cwd: &Path,
    label: &str,
    claude_args: &[&str],
    focus: bool,
) -> Result<(), String> {
    if !herdr::available() {
        println!(
            "not in herdr — run it yourself: cd {} && claude {}",
            cwd.display(),
            claude_args.join(" ")
        );
        return Ok(());
    }
    let (tab, pane) = open_tab(space, cwd, label)?;
    let agent = free_agent_name(label);
    herdr::start_claude(&agent, &pane, claude_args).map_err(err)?;
    if focus {
        let _ = herdr::focus_tab(&tab);
    }
    println!("agent {agent} → workspace \"{space}\", tab {label}");
    Ok(())
}

/// A new tab `label` in the herdr workspace `space`, created when missing;
/// returns the tab and its pane.
fn open_tab(space: &str, cwd: &Path, label: &str) -> Result<(String, String), String> {
    let existing = herdr::list_workspaces()
        .map_err(err)?
        .into_iter()
        .find(|w| same_name(&w.label, space));
    match existing {
        Some(ws) => herdr::create_tab(&ws.workspace_id, cwd, label).map_err(err),
        None => {
            let (_, tab, pane) = herdr::create_workspace(space, cwd).map_err(err)?;
            herdr::rename_tab(&tab, label).map_err(err)?;
            Ok((tab, pane))
        }
    }
}

fn free_agent_name(base: &str) -> String {
    let base = herdr::agent_name(base);
    let taken: Vec<String> = herdr::list_agents()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|a| a.name)
        .collect();
    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .map(|n| {
            let suffix = format!("-{n}");
            let head: String = base.chars().take(32 - suffix.len()).collect();
            format!("{head}{suffix}")
        })
        .find(|candidate| !taken.contains(candidate))
        .unwrap_or(base)
}

/// The agent's brief: task note, ticket text, what to do, definition of done.
fn brief_text(
    tree: &Path,
    branch: &str,
    task: Option<&Task>,
    issue: Option<&JiraIssue>,
    task_tickets: &[String],
    message: Option<&str>,
) -> String {
    let mut brief = String::new();
    match (task, issue) {
        (Some(task), _) => brief.push_str(&format!("# {}\n\n", task.name)),
        (None, Some(issue)) => {
            brief.push_str(&format!("# {}: {}\n\n", issue.key, issue.fields.summary))
        }
        (None, None) => {}
    }
    if let Some(task) = task {
        brief.push_str(&format!(
            "Заметка задачи — общий контекст по всем её тикетам, прочитай целиком: {}\n",
            task.note.path.display()
        ));
    }
    brief.push_str(&format!(
        "Ты работаешь в ворктри {}, ветка {branch}.\n",
        tree.display()
    ));
    if let Some(issue) = issue {
        brief.push_str(&format!(
            "\n## {}: {}\n\n{}\n",
            issue.key,
            issue.fields.summary,
            issue
                .fields
                .description
                .as_deref()
                .unwrap_or("(без описания)")
                .trim()
        ));
    }
    let others: Vec<&String> = task_tickets
        .iter()
        .filter(|k| issue.is_none_or(|i| &i.key != *k))
        .collect();
    if !others.is_empty() {
        let list: Vec<&str> = others.iter().map(|k| k.as_str()).collect();
        brief.push_str(&format!("\nДругие тикеты задачи: {}.\n", list.join(", ")));
    }
    let todo = match (message, issue) {
        (Some(m), _) => m,
        (None, Some(_)) => "См. тикет выше.",
        (None, None) => "См. заметку задачи.",
    };
    brief.push_str(&format!("\n## Что нужно\n\n{todo}\n"));
    if task.is_some() {
        brief.push_str(
            "\n## Находки\n\nРешения и неочевидные находки — не прогресс — записывай командой \
             `wt note \"<текст>\"` из ворктри: это раздел «Agent notes» в заметке задачи, \
             его читают агенты по соседним тикетам.\n",
        );
    }
    brief.push_str(&format!("\n## Готово, когда\n\n{DONE_DEFINITION}\n"));
    brief
}

/// The brief goes to the worktree's git dir: never committed, removed with the
/// worktree. herdr can't pass multi-line arguments, so the prompt points at it.
fn write_brief(tree: &Path, brief: &str) -> Result<PathBuf, String> {
    let path = worktree::git_dir(tree).map_err(err)?.join("task.md");
    std::fs::write(&path, brief).map_err(err)?;
    Ok(path)
}

/// A ticket summary as a note file name: without the characters Obsidian and
/// file systems refuse, one line, of reasonable length.
fn note_title(summary: &str) -> String {
    let cleaned: String = summary
        .chars()
        .map(|c| if "*\"\\/<>:|?#^[]".contains(c) || c.is_control() { ' ' } else { c })
        .collect();
    let title = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    title.chars().take(120).collect::<String>().trim_end_matches(['.', ' ']).to_string()
}

fn fetch_issue(config: &Config, key: &str) -> Result<JiraIssue, String> {
    table::jira_client(config)?
        .get_issue_by_key(key)
        .map_err(|e| format!("{key}: {e}"))
}

fn name_from_issue(issue: &JiraIssue) -> String {
    name_from_text(&issue.fields.summary).unwrap_or_else(|| issue.key.to_lowercase())
}

/// The first four latin words, slugified.
fn name_from_text(text: &str) -> Option<String> {
    let slug = slugify(text);
    let words: Vec<&str> = slug.split('-').filter(|w| !w.is_empty()).take(4).collect();
    (!words.is_empty()).then(|| words.join("-"))
}

fn branch_name(config: &Config, key: Option<&String>, name: &str) -> String {
    let prefix = &config.workflow.branch_prefix;
    match key {
        Some(key) => format!("{prefix}{key}-{name}"),
        None => format!("{prefix}{name}"),
    }
}

/// The main checkout of the configured repo the cwd is in, otherwise of the first.
fn current_repo(config: &Config) -> Result<PathBuf, String> {
    let mains: Vec<PathBuf> = config
        .repos
        .iter()
        .filter_map(|r| worktree::main_checkout(&r.path).ok())
        .collect();
    let here = std::env::current_dir()
        .ok()
        .and_then(|d| worktree::main_checkout(&d).ok());
    if let Some(main) = here.filter(|m| mains.contains(m)) {
        return Ok(main);
    }
    mains
        .into_iter()
        .next()
        .ok_or_else(|| "no [[repo]] in ~/.config/worktable/config.toml".to_string())
}

/// The repo the old jg commands work in: `given` (`-r`, the cwd by default) when it
/// is inside a configured repo, otherwise the first configured one — `wt done`
/// from the vault means ucp, not the vault's own git. Also that repo's GitLab project.
pub(super) fn legacy_repo(config: &Config, given: &Path) -> (PathBuf, Option<String>) {
    let here = worktree::main_checkout(given).ok();
    let repos: Vec<(PathBuf, &str)> = config
        .repos
        .iter()
        .filter_map(|r| Some((worktree::main_checkout(&r.path).ok()?, r.gitlab_project.as_str())))
        .collect();
    if let Some((_, project)) = repos.iter().find(|(main, _)| Some(main) == here.as_ref()) {
        return (given.to_path_buf(), Some(project.to_string()));
    }
    match repos.first() {
        Some((main, project)) => (main.clone(), Some(project.to_string())),
        None => (given.to_path_buf(), None),
    }
}

fn list(items: &[String]) -> Value {
    Value::Sequence(items.iter().cloned().map(Value::String).collect())
}

fn fuzzy(prompt: &str, items: &[String]) -> Result<usize, String> {
    if !std::io::stdin().is_terminal() {
        return Err(format!(
            "{prompt}: needs a choice, but there is no terminal — pass an argument"
        ));
    }
    FuzzySelect::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .items(items)
        .default(0)
        .interact_opt()
        .map_err(err)?
        .ok_or_else(|| "cancelled".to_string())
}

fn confirm(prompt: &str) -> Result<bool, String> {
    Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .default(false)
        .interact()
        .map_err(err)
}

#[cfg(test)]
mod tests {
    use super::note_title;

    #[test]
    fn ticket_summary_as_note_title() {
        assert_eq!(note_title("javadoc-и ко всем точкам расширения"), "javadoc-и ко всем точкам расширения");
        assert_eq!(note_title("s2s: читать [table]/path?  "), "s2s читать table path");
    }
}
