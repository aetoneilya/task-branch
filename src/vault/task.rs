use crate::config::VaultConfig;
use crate::vault::note::{Note, NoteError};
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

static TICKET_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[A-Z][A-Z0-9]+-\d+\b").unwrap());

/// Jira keys mentioned in `text`, in order, without duplicates.
pub fn ticket_keys(text: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for m in TICKET_KEY.find_iter(text) {
        if !keys.iter().any(|k| k == m.as_str()) {
            keys.push(m.as_str().to_string());
        }
    }
    keys
}

/// A claude session held in one of the task's worktrees, stored in the note as
/// `<session id> <worktree name> <date>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRef {
    pub id: String,
    pub worktree: String,
    pub date: String,
}

impl SessionRef {
    fn parse(entry: &str) -> Option<SessionRef> {
        let mut parts = entry.split_whitespace();
        Some(SessionRef {
            id: parts.next()?.to_string(),
            worktree: parts.next().unwrap_or_default().to_string(),
            date: parts.next().unwrap_or_default().to_string(),
        })
    }

    pub fn entry(&self) -> String {
        format!("{} {} {}", self.id, self.worktree, self.date)
    }
}

/// The `state:` a task note carries, derived from its tickets, MRs and
/// worktrees; a closed task has `done: true` instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Active,
    Review,
    Idea,
    Finished,
}

impl TaskState {
    pub fn parse(text: &str) -> Option<TaskState> {
        match text {
            "active" => Some(TaskState::Active),
            "review" => Some(TaskState::Review),
            "idea" => Some(TaskState::Idea),
            "finished" => Some(TaskState::Finished),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Active => "active",
            TaskState::Review => "review",
            TaskState::Idea => "idea",
            TaskState::Finished => "finished",
        }
    }
}

/// A task note: shared context for one piece of work and the tickets it covers.
#[derive(Debug)]
pub struct Task {
    pub note: Note,
    pub name: String,
    pub tickets: Vec<String>,
    pub tags: Vec<String>,
    pub done: bool,
    pub state: Option<TaskState>,
    pub sessions: Vec<SessionRef>,
    /// When the note was created, RFC 3339; set by `load_tasks`.
    pub created: Option<String>,
}

impl Task {
    pub fn from_note(note: Note) -> Task {
        Task {
            name: note.name(),
            tickets: note
                .strings("tickets")
                .iter()
                .flat_map(|t| ticket_keys(t))
                .collect(),
            tags: note.strings("tags"),
            done: note.flag("done"),
            state: note.string("state").as_deref().and_then(TaskState::parse),
            sessions: note
                .strings("sessions")
                .iter()
                .filter_map(|s| SessionRef::parse(s))
                .collect(),
            created: None,
            note,
        }
    }
}

/// Task notes under the configured folders; notes whose frontmatter does not
/// parse are reported and left alone.
pub fn load_tasks(vault: &VaultConfig) -> (Vec<Task>, Vec<NoteError>) {
    let mut tasks = Vec::new();
    let mut errors = Vec::new();
    let added = git_added(vault);
    for dir in &vault.task_dirs {
        for path in markdown_files(&vault.path.join(dir)) {
            match Note::read(&path) {
                Ok(note) => {
                    let mut task = Task::from_note(note);
                    task.created = path
                        .strip_prefix(&vault.path)
                        .ok()
                        .and_then(|rel| added.get(rel).cloned())
                        .or_else(|| birth_time(&path));
                    tasks.push(task);
                }
                Err(err) => errors.push(err),
            }
        }
    }
    tasks.sort_by(|a, b| a.name.cmp(&b.name));
    (tasks, errors)
}

/// When each note first appeared in the vault's git history (the vault is
/// auto-committed). The file's own birth time is useless: saving a note goes
/// through a temp file and a rename.
fn git_added(vault: &VaultConfig) -> HashMap<PathBuf, String> {
    let mut args = vec![
        "-c".to_string(),
        "core.quotePath=false".to_string(),
        "log".to_string(),
        "--diff-filter=A".to_string(),
        "--name-only".to_string(),
        "--format=@%aI".to_string(),
        "--".to_string(),
    ];
    args.extend(
        vault
            .task_dirs
            .iter()
            .map(|d| d.to_string_lossy().to_string()),
    );
    let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(&vault.path)
        .args(&args)
        .output()
    else {
        return HashMap::new();
    };
    let mut added = HashMap::new();
    let mut date = String::new();
    // newest commits first: the last date seen for a path is its first add
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(d) = line.strip_prefix('@') {
            date = d.to_string();
        } else if !line.is_empty() {
            added.insert(PathBuf::from(line), date.clone());
        }
    }
    added
}

fn birth_time(path: &Path) -> Option<String> {
    let created = std::fs::metadata(path).ok()?.created().ok()?;
    Some(chrono::DateTime::<chrono::Local>::from(created).to_rfc3339())
}

fn markdown_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if path.is_dir() {
            files.extend(markdown_files(&path));
        } else if path.extension().is_some_and(|e| e == "md") {
            files.push(path);
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_keys_once() {
        assert_eq!(
            ticket_keys("i.udalov/UCP-61083-a2i, UCP-61083: x, see UCP-7"),
            vec!["UCP-61083", "UCP-7"]
        );
        assert!(ticket_keys("utf-8 and x86-64").is_empty());
    }

    #[test]
    fn session_entry_roundtrip() {
        let s = SessionRef::parse("a9c0 a2i-config 2026-10-08").unwrap();
        assert_eq!(s.worktree, "a2i-config");
        assert_eq!(s.entry(), "a9c0 a2i-config 2026-10-08");
    }
}
