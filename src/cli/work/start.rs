//! `wt start` and `wt add`: new tasks and tickets with their worktree tabs.

use super::brief::brief_text;
use super::common::{
    Terminal, Ui, branch_name, choose_repo, err, fetch_issue, find_task, is_ticket_key, list,
    name_from_issue, name_from_text, resolve_task,
};
use super::launch::open_work;
use crate::cli::{AddArgs, StartArgs, load};
use crate::client::jira::{JiraClient, JiraIssue};
use crate::client::worktree;
use crate::config::Config;
use crate::vault::note::Note;
use crate::vault::task::{Task, ticket_keys, vault_config};
use serde_yaml::Value;

/// `wt start <task name | KEY>`: a new task (note, herdr workspace, worktree tab
/// with claude), or work on a ticket without a note.
pub(in crate::cli) fn cli_start(config: &Config, args: StartArgs) -> Result<(), String> {
    start(config, args, &mut Terminal)
}

pub(crate) fn start(config: &Config, args: StartArgs, ui: &mut dyn Ui) -> Result<(), String> {
    start_with(config, load(config)?, args, ui)
}

fn start_with(
    config: &Config,
    tasks: Vec<Task>,
    mut args: StartArgs,
    ui: &mut dyn Ui,
) -> Result<(), String> {
    let target = match args.target.take() {
        Some(target) => target,
        None => pick_start_target(config, &tasks, ui)?,
    };
    let existing = find_task(&tasks, &target, false)?;

    if is_ticket_key(&target) {
        if let Some(i) = existing {
            let task = &tasks[i];
            ui.say(format!(
                "{target} is already in task \"{}\" — adding it there",
                task.name
            ));
            let add_args = AddArgs {
                args: vec![task.name.clone(), target],
                in_tree: None,
                name: args.name,
                message: args.message,
                bg: args.bg,
            };
            return add_with(config, tasks, add_args, ui);
        }
        // work always gets a task: a note for shared context and agents' findings,
        // named after the ticket
        let issue = fetch_issue(config, &target)?;
        let title = note_title(&issue.fields.summary);
        if title.is_empty() {
            return Err(format!("{target} has no summary to name the task after"));
        }
        let mut keys = vec![target.clone()];
        keys.extend(args.tickets.into_iter().filter(|k| k != &target));
        let args = StartArgs {
            target: Some(title),
            tickets: keys,
            ..args
        };
        return start_with(config, tasks, args, ui);
    }

    if let Some(i) = existing {
        return Err(format!(
            "task \"{0}\" already exists: wt select \"{0}\" to get back to it, wt add \"{0}\" <KEY> to add a ticket",
            tasks[i].name
        ));
    }
    let StartArgs {
        tickets,
        tags,
        message,
        name,
        branch,
        bg,
        ..
    } = args;
    let similar = similar_tasks(&tasks, &target, &tags);
    if !similar.is_empty() && ui.can_ask() {
        ui.say(format!("similar tasks: {}", similar.join(", ")));
        if !ui.confirm("Create a new one anyway?")? {
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
    let main = choose_repo(config, ui)?;

    let vault = vault_config(config)?;
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
    ui.say(format!("note: {}", path.display()));

    let tree = worktree::worktree_dir(&main, &wt_name);
    let brief = brief_text(
        &tree,
        &branch,
        Some(&task),
        issue.as_ref(),
        &keys,
        message.as_deref(),
    );
    open_work(&task, &main, &wt_name, &branch, brief, !bg, ui)
}

/// `wt add [<task>] <KEY>`: the ticket joins the task and gets a tab in the
/// task's herdr workspace, in a new worktree or `--in` one the task has.
pub(in crate::cli) fn cli_add(config: &Config, args: AddArgs) -> Result<(), String> {
    add(config, args, &mut Terminal)
}

pub(crate) fn add(config: &Config, args: AddArgs, ui: &mut dyn Ui) -> Result<(), String> {
    add_with(config, load(config)?, args, ui)
}

fn add_with(
    config: &Config,
    tasks: Vec<Task>,
    args: AddArgs,
    ui: &mut dyn Ui,
) -> Result<(), String> {
    let AddArgs {
        args,
        in_tree,
        name,
        message,
        bg,
    } = args;
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
    let mut task = resolve_task(config, tasks, task_arg.as_deref(), ui)?;

    if !task.tickets.contains(&key) {
        let _lock = crate::vault::lock().map_err(err)?;
        task.tickets.push(key.clone());
        task.note.set("tickets", list(&task.tickets));
        task.note.save().map_err(err)?;
        ui.say(format!("{key} added to \"{}\"", task.name));
    }

    let issue = fetch_issue(config, &key)?;
    let main = match &in_tree {
        Some(existing) => super::common::repo_with_worktree(config, existing)
            .ok_or_else(|| format!("no worktree {existing}"))?,
        None => choose_repo(config, ui)?,
    };
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
    open_work(&task, &main, &wt_name, &branch, brief, !bg, ui)
}

/// My open Jira tickets that belong to no task, plus "new task…".
fn pick_start_target(config: &Config, tasks: &[Task], ui: &mut dyn Ui) -> Result<String, String> {
    let client = JiraClient::from_env(&config.jira.host)?;
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
    match ui.pick("Start", &items)? {
        0 => ui.input("Task name"),
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
