use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// A Claude Code conversation, stored as `<projects dir>/<launch cwd slug>/<id>.jsonl`.
/// The launch directory is not where the work happened: an agent started in the
/// main checkout often moves into a worktree, so the transcript's `cwd` fields
/// are what ties a session to a worktree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    /// Day of the first message, YYYY-MM-DD (UTC).
    pub date: String,
    /// Working directories of the transcript's records, most frequent first.
    pub cwds: Vec<(String, usize)>,
    /// Where claude was launched: `--resume` finds the session only from there.
    pub first_cwd: Option<String>,
    pub last_cwd: Option<String>,
    /// Unix time the transcript last changed: the session's last activity. Taken
    /// from the file, not cached with the parsed transcript.
    #[serde(skip)]
    pub modified: u64,
}

impl Session {
    /// Names of the worktrees under `root` the session worked in, most used first.
    pub fn worktrees_under(&self, root: &Path) -> Vec<String> {
        let root = format!("{}/", root.to_string_lossy());
        let mut names: Vec<String> = Vec::new();
        for (cwd, _) in &self.cwds {
            let Some(rest) = cwd.strip_prefix(&root) else {
                continue;
            };
            let name = rest.split('/').next().unwrap_or(rest).to_string();
            if !name.is_empty() && !names.contains(&name) {
                names.push(name);
            }
        }
        names
    }
}

fn projects_dir() -> Option<PathBuf> {
    let base = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))?;
    Some(base.join("projects"))
}

/// Claude names a project directory after the cwd with every character outside
/// [A-Za-z0-9-] replaced by '-'.
fn slug(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[derive(Serialize, Deserialize)]
struct Entry {
    len: u64,
    mtime: u64,
    session: Session,
}

/// Parsed transcripts keyed by file, reused while a file's size and mtime stay
/// the same: transcripts are large (hundreds of MB in a busy repo).
#[derive(Default)]
pub struct Cache {
    entries: HashMap<String, Entry>,
    dirty: bool,
}

impl Cache {
    fn path() -> PathBuf {
        crate::config::cache_dir().join("sessions-v2.json")
    }

    pub fn load() -> Cache {
        let entries = std::fs::read(Cache::path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Cache {
            entries,
            dirty: false,
        }
    }

    pub fn save(&mut self) {
        self.prune();
        if !self.dirty {
            return;
        }
        let path = Cache::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec(&self.entries) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }

    /// Forgets transcripts that are gone: Claude Code deletes old ones.
    fn prune(&mut self) {
        let before = self.entries.len();
        self.entries.retain(|file, _| Path::new(file).exists());
        self.dirty |= self.entries.len() != before;
    }

    fn session(&mut self, file: &Path) -> Option<Session> {
        let meta = std::fs::metadata(file).ok()?;
        let len = meta.len();
        let mtime = meta
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_secs();
        let key = file.to_string_lossy().to_string();
        if let Some(entry) = self.entries.get(&key)
            && entry.len == len
            && entry.mtime == mtime
        {
            return Some(Session {
                modified: mtime,
                ..entry.session.clone()
            });
        }
        let session = Session {
            modified: mtime,
            ..parse(file)?
        };
        self.entries.insert(
            key,
            Entry {
                len,
                mtime,
                session: session.clone(),
            },
        );
        self.dirty = true;
        Some(session)
    }
}

/// Sessions launched with `cwd` as their working directory. They outlive the
/// directory itself, e.g. a removed worktree.
pub fn sessions_in(cwd: &Path, cache: &mut Cache) -> Vec<Session> {
    let Some(dir) = projects_dir().map(|p| p.join(slug(cwd))) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .filter_map(|p| cache.session(&p))
        .collect()
}

fn parse(file: &Path) -> Option<Session> {
    let id = file.file_stem()?.to_string_lossy().to_string();
    let bytes = std::fs::read(file).ok()?;
    let text = String::from_utf8_lossy(&bytes);

    // transcripts start with metadata lines; the first timestamp is a message's
    let date = field_values(&text, "timestamp")
        .first()
        .and_then(|ts| ts.get(..10))?
        .to_string();

    let mut counts: HashMap<&str, usize> = HashMap::new();
    let all_cwds = field_values(&text, "cwd");
    let first_cwd = all_cwds.first().map(|c| c.to_string());
    let mut last_cwd = None;
    for cwd in all_cwds {
        *counts.entry(cwd).or_default() += 1;
        last_cwd = Some(cwd);
    }
    let mut cwds: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(cwd, n)| (cwd.to_string(), n))
        .collect();
    cwds.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    Some(Session {
        id,
        date,
        cwds,
        first_cwd,
        last_cwd: last_cwd.map(str::to_string),
        modified: 0,
    })
}

/// Values of `"<name>":"…"` string fields, in order. Paths and timestamps carry
/// no escaped quotes, so the value ends at the next quote.
fn field_values<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let pattern = format!("\"{name}\":\"");
    text.match_indices(&pattern)
        .filter_map(|(pos, _)| {
            let start = pos + pattern.len();
            let len = text[start..].find('"')?;
            Some(&text[start..start + len])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_matches_claude_project_dirs() {
        assert_eq!(
            slug(Path::new(
                "/Users/i.udalov/code/vk/ucp/.claude/worktrees/a2i-config"
            )),
            "-Users-i-udalov-code-vk-ucp--claude-worktrees-a2i-config"
        );
    }

    #[test]
    fn worktrees_from_cwds() {
        let session = Session {
            id: "x".into(),
            date: "2026-10-08".into(),
            cwds: vec![
                ("/r/.claude/worktrees/a2i/libraries/config".into(), 30),
                ("/r".into(), 20),
                ("/r/.claude/worktrees/b".into(), 5),
                ("/r/.claude/worktrees/a2i".into(), 3),
            ],
            first_cwd: None,
            last_cwd: None,
            modified: 0,
        };
        assert_eq!(
            session.worktrees_under(Path::new("/r/.claude/worktrees")),
            vec!["a2i", "b"]
        );
    }

    #[test]
    fn forgets_deleted_transcripts() {
        let dir = std::env::temp_dir().join(format!("worktable-sessions-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let kept = dir.join("kept.jsonl");
        std::fs::write(&kept, r#"{"cwd":"/a","timestamp":"2026-10-08T10:00:00Z"}"#).unwrap();
        let mut cache = Cache::default();
        assert!(cache.session(&kept).is_some_and(|s| s.modified > 0));
        let gone = dir.join("gone.jsonl");
        std::fs::write(&gone, r#"{"cwd":"/b","timestamp":"2026-10-09T10:00:00Z"}"#).unwrap();
        cache.session(&gone).unwrap();
        std::fs::remove_file(&gone).unwrap();
        cache.dirty = false;

        cache.prune();
        let files: Vec<&String> = cache.entries.keys().collect();
        assert_eq!(files, vec![&kept.to_string_lossy().to_string()]);
        assert!(cache.dirty);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reads_string_fields() {
        let text = r#"{"cwd":"/a","timestamp":"2026-10-08T10:00:00Z"}
{"cwd":"/b/c"}"#;
        assert_eq!(field_values(text, "cwd"), vec!["/a", "/b/c"]);
    }
}
