//! The agent's brief and first prompt.

use super::common::err;
use crate::client::jira::JiraIssue;
use crate::client::worktree;
use crate::vault::task::Task;
use std::path::{Path, PathBuf};

const DONE_DEFINITION: &str = "\
If you change code, you are done when:
- it compiles, and the tests of the affected modules ran and pass — include the output;
- nothing is committed or pushed: Ilya reviews the diff first;
- you end with a short summary: what changed, how it was checked, open questions.
If the task is ambiguous or touches more than a couple of files, start with a plan and agree on it.";

/// Points claude at the task note and the brief; it reads them and waits.
pub(super) fn agent_prompt(note: Option<&Path>, brief: &Path) -> Option<String> {
    let mut context = Vec::new();
    if let Some(note) = note {
        context.push(format!("task note {}", note.display()));
    }
    if brief.exists() {
        context.push(format!("brief with the ticket {}", brief.display()));
    }
    (!context.is_empty()).then(|| {
        format!(
            "Context: {}. Read it and wait for my command.",
            context.join(", ")
        )
    })
}

/// The agent's brief: task note, ticket text, what to do, definition of done.
pub(super) fn brief_text(
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
            "The task note is the shared context for all of the task's tickets — read it in full: {}\n",
            task.note.path.display()
        ));
    }
    brief.push_str(&format!(
        "You work in the worktree {}, branch {branch}.\n",
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
                .unwrap_or("(no description)")
                .trim()
        ));
    }
    let others: Vec<&String> = task_tickets
        .iter()
        .filter(|k| issue.is_none_or(|i| &i.key != *k))
        .collect();
    if !others.is_empty() {
        let list: Vec<&str> = others.iter().map(|k| k.as_str()).collect();
        brief.push_str(&format!(
            "\nOther tickets of the task: {}.\n",
            list.join(", ")
        ));
    }
    let todo = match (message, issue) {
        (Some(m), _) => m,
        (None, Some(_)) => "See the ticket above.",
        (None, None) => "See the task note.",
    };
    brief.push_str(&format!("\n## What is needed\n\n{todo}\n"));
    if task.is_some() {
        brief.push_str(
            "\n## Findings\n\nRecord decisions and non-obvious findings — not progress — with \
             `wt note \"<text>\"` from the worktree: it goes to the \"Agent notes\" section of \
             the task note, which agents on the task's other tickets read.\n",
        );
    }
    brief.push_str(&format!("\n## Done when\n\n{DONE_DEFINITION}\n"));
    brief
}

/// The brief goes to the worktree's git dir: never committed, removed with the
/// worktree. herdr can't pass multi-line arguments, so the prompt points at it.
pub(super) fn write_brief(tree: &Path, brief: &str) -> Result<PathBuf, String> {
    let path = brief_path(tree)?;
    std::fs::write(&path, brief).map_err(err)?;
    Ok(path)
}

/// Where `write_brief` puts the brief of the worktree `tree`.
pub(super) fn brief_path(tree: &Path) -> Result<PathBuf, String> {
    Ok(worktree::git_dir(tree).map_err(err)?.join("task.md"))
}
