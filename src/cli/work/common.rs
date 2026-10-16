//! Helpers shared by the work commands: errors, names, repo lookup, pickers.

use crate::cli::table::load;
use crate::client::jira::JiraIssue;
use crate::client::worktree;
use crate::config::Config;
use crate::table::{self, Table, TaskRow};
use crate::vault::task::{Task, ticket_keys};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, FuzzySelect};
use serde_yaml::Value;
use std::io::IsTerminal;
use std::path::PathBuf;

pub(super) fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

pub(super) fn is_ticket_key(text: &str) -> bool {
    ticket_keys(text).first().is_some_and(|k| k == text)
}

pub(super) fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

pub(super) fn fetch_issue(config: &Config, key: &str) -> Result<JiraIssue, String> {
    table::jira_client(config)?
        .get_issue_by_key(key)
        .map_err(|e| format!("{key}: {e}"))
}

pub(super) fn name_from_issue(issue: &JiraIssue) -> String {
    name_from_text(&issue.fields.summary).unwrap_or_else(|| issue.key.to_lowercase())
}

/// The first four latin words, slugified.
pub(super) fn name_from_text(text: &str) -> Option<String> {
    let slug = slugify(text);
    let words: Vec<&str> = slug.split('-').filter(|w| !w.is_empty()).take(4).collect();
    (!words.is_empty()).then(|| words.join("-"))
}

fn slugify(text: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;

    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }

    slug.trim_matches('-').to_string()
}

pub(super) fn branch_name(config: &Config, key: Option<&String>, name: &str) -> String {
    let prefix = &config.workflow.branch_prefix;
    match key {
        Some(key) => format!("{prefix}{key}-{name}"),
        None => format!("{prefix}{name}"),
    }
}

/// The main checkout of the configured repo the cwd is in, otherwise of the first.
pub(super) fn current_repo(config: &Config) -> Result<PathBuf, String> {
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

pub(super) fn list(items: &[String]) -> Value {
    Value::Sequence(items.iter().cloned().map(Value::String).collect())
}

pub(super) fn fuzzy(prompt: &str, items: &[String]) -> Result<usize, String> {
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

pub(super) fn confirm(prompt: &str) -> Result<bool, String> {
    Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .default(false)
        .interact()
        .map_err(err)
}

/// The open task of the current worktree, or of the current herdr workspace.
pub(super) fn current_task(table: &Table) -> Option<usize> {
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
pub(super) fn resolve_task(config: &Config, arg: Option<&str>) -> Result<Task, String> {
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
