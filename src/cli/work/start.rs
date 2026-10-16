//! `wt start` and `wt add`: new tasks and tickets with their worktree tabs.

use super::brief::brief_text;
use super::common::{
    branch_name, confirm, current_repo, err, fetch_issue, fuzzy, is_ticket_key, list,
    name_from_issue, name_from_text, resolve_task, same_name,
};
use super::launch::open_work;
use crate::cli::table::{load, vault};
use crate::client::jira::JiraIssue;
use crate::client::worktree;
use crate::config::Config;
use crate::table;
use crate::vault::note::Note;
use crate::vault::task::{Task, ticket_keys};
use dialoguer::Input;
use dialoguer::theme::ColorfulTheme;
use serde_yaml::Value;
use std::io::IsTerminal;

/// `wt start <task name | KEY>`: a new task (note, herdr workspace, worktree tab
/// with claude), or work on a ticket without a note.
#[allow(clippy::too_many_arguments)]
pub(in crate::cli) fn cli_start(
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
            println!(
                "{target} is already in task \"{}\" — adding it there",
                task.name
            );
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
pub(in crate::cli) fn cli_add(
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
        return Err(format!(
            "\"{key}\" is not a ticket key: wt add [<task>] <KEY>"
        ));
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
            let branch = worktree::current_branch(&dir).map_err(err)?;
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

/// A ticket summary as a note file name: without the characters Obsidian and
/// file systems refuse, one line, of reasonable length.
fn note_title(summary: &str) -> String {
    let cleaned: String = summary
        .chars()
        .map(|c| {
            if "*\"\\/<>:|?#^[]".contains(c) || c.is_control() {
                ' '
            } else {
                c
            }
        })
        .collect();
    let title = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    title
        .chars()
        .take(120)
        .collect::<String>()
        .trim_end_matches(['.', ' '])
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::note_title;

    #[test]
    fn ticket_summary_as_note_title() {
        assert_eq!(
            note_title("javadoc-и ко всем точкам расширения"),
            "javadoc-и ко всем точкам расширения"
        );
        assert_eq!(
            note_title("s2s: читать [table]/path?  "),
            "s2s читать table path"
        );
    }
}
