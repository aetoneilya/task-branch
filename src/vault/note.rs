use serde_yaml::{Mapping, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NoteError {
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),

    #[error("{0}: invalid frontmatter: {1}")]
    Yaml(PathBuf, serde_yaml::Error),

    #[error("{0}: changed while being updated, skipped")]
    Changed(PathBuf),
}

pub(crate) type Result<T> = std::result::Result<T, NoteError>;

/// A markdown note with optional YAML frontmatter.
///
/// Keys are read through a YAML parser, but written line by line: `set` replaces
/// only the lines of the keys it touches, so the rest of the file stays exactly as
/// Obsidian or the user left it.
#[derive(Debug)]
pub struct Note {
    pub path: PathBuf,
    text: String,
    modified: SystemTime,
    frontmatter: Mapping,
}

impl Note {
    pub fn read(path: &Path) -> Result<Note> {
        let io_err = |err| NoteError::Io(path.to_path_buf(), err);
        let text = fs::read_to_string(path).map_err(io_err)?;
        let modified = fs::metadata(path)
            .and_then(|m| m.modified())
            .map_err(io_err)?;
        let frontmatter = parse_frontmatter(&text).map_err(|e| NoteError::Yaml(path.into(), e))?;
        Ok(Note {
            path: path.to_path_buf(),
            text,
            modified,
            frontmatter,
        })
    }

    /// The text after the frontmatter.
    pub fn body(&self) -> &str {
        match frontmatter_range(&self.text) {
            Some((_, end)) => {
                let rest = &self.text[end..];
                rest.split_once('\n').map_or("", |(_, body)| body)
            }
            None => &self.text,
        }
    }

    /// File name without `.md`.
    pub fn name(&self) -> String {
        self.path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    pub fn has(&self, key: &str) -> bool {
        self.frontmatter.contains_key(key)
    }

    pub fn string(&self, key: &str) -> Option<String> {
        self.frontmatter.get(key).and_then(scalar_string)
    }

    /// A list value, or a single scalar as a one-element list.
    pub fn strings(&self, key: &str) -> Vec<String> {
        match self.frontmatter.get(key) {
            Some(Value::Sequence(items)) => items.iter().filter_map(scalar_string).collect(),
            Some(value) => scalar_string(value).into_iter().collect(),
            None => Vec::new(),
        }
    }

    pub fn flag(&self, key: &str) -> bool {
        match self.frontmatter.get(key) {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
            _ => false,
        }
    }

    /// Writes a new note with `keys` as frontmatter; refuses to overwrite.
    pub fn create(path: &Path, keys: &[(&str, Value)], body: &str) -> Result<Note> {
        let io_err = |err| NoteError::Io(path.to_path_buf(), err);
        let mut text = format!("---\n---\n{body}");
        for (key, value) in keys {
            text = set_key(&text, key, value);
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(io_err)?;
        }
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()))
            .map_err(io_err)?;
        Note::read(path)
    }

    /// Appends `line` as the last item under `## Agent notes`, adding the heading
    /// at the end of the note on first use.
    pub fn append_agent_note(&mut self, line: &str) {
        self.text = append_under_heading(&self.text, AGENT_NOTES, line);
    }

    /// Re-reads the file, for a retry after `save` found it changed.
    pub fn reload(&mut self) -> Result<()> {
        *self = Note::read(&self.path)?;
        Ok(())
    }

    /// Sets a top-level key in memory; `save` writes it.
    pub fn set(&mut self, key: &str, value: Value) {
        self.text = set_key(&self.text, key, &value);
        self.frontmatter
            .insert(Value::String(key.to_string()), value);
    }

    /// Writes the note if `set` changed it. Refuses when the file was modified
    /// since it was read; writes a temp file and renames it over the note.
    pub fn save(&mut self) -> Result<bool> {
        let io_err = |err| NoteError::Io(self.path.clone(), err);
        let on_disk = fs::read_to_string(&self.path).map_err(io_err)?;
        if on_disk == self.text {
            return Ok(false);
        }
        let modified = fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .map_err(io_err)?;
        if modified != self.modified {
            return Err(NoteError::Changed(self.path.clone()));
        }

        let file_name = self.path.file_name().unwrap_or_default().to_string_lossy();
        let tmp = self.path.with_file_name(format!(".{file_name}.wt-tmp"));
        fs::write(&tmp, &self.text).map_err(io_err)?;
        fs::rename(&tmp, &self.path).map_err(io_err)?;
        self.modified = fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .map_err(io_err)?;
        Ok(true)
    }
}

const AGENT_NOTES: &str = "## Agent notes";

fn append_under_heading(text: &str, heading: &str, line: &str) -> String {
    let item = format!("- {line}\n");
    let Some(pos) = text.match_indices(heading).map(|(i, _)| i).find(|&i| {
        (i == 0 || text[..i].ends_with('\n')) && text[i + heading.len()..].starts_with(['\n', '\r'])
    }) else {
        let sep = if text.is_empty() || text.ends_with("\n\n") {
            ""
        } else if text.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        return format!("{text}{sep}{heading}\n{item}");
    };
    // the section ends at the next heading of the same or a higher level
    let body_start = pos + heading.len();
    let section_end = text[body_start..]
        .match_indices("\n#")
        .map(|(i, _)| body_start + i + 1)
        .find(|&i| {
            let level = text[i..].chars().take_while(|&c| c == '#').count();
            level <= 2 && text[i + level..].starts_with(' ')
        })
        .unwrap_or(text.len());
    let mut section = text[..section_end].trim_end_matches('\n').to_string();
    section.push('\n');
    section.push_str(&item);
    let rest = &text[section_end..];
    if rest.is_empty() {
        section
    } else {
        format!("{section}\n{rest}")
    }
}

fn scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Byte range of the YAML between the opening `---` line and the closing one.
fn frontmatter_range(text: &str) -> Option<(usize, usize)> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let start = text.len() - rest.len();
    let mut offset = start;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Some((start, offset));
        }
        offset += line.len();
    }
    None
}

fn parse_frontmatter(text: &str) -> std::result::Result<Mapping, serde_yaml::Error> {
    let Some((start, end)) = frontmatter_range(text) else {
        return Ok(Mapping::new());
    };
    match serde_yaml::from_str::<Value>(&text[start..end])? {
        Value::Mapping(mapping) => Ok(mapping),
        _ => Ok(Mapping::new()),
    }
}

/// Renders `key: value` the way Obsidian writes properties: lists as block lists
/// indented by two spaces, scalars quoted only when YAML needs it.
fn render_key(key: &str, value: &Value) -> String {
    match value {
        Value::Sequence(items) if items.is_empty() => format!("{key}: []\n"),
        Value::Sequence(items) => {
            let mut out = format!("{key}:\n");
            for item in items {
                out.push_str(&format!("  - {}\n", render_scalar(item)));
            }
            out
        }
        other => format!("{key}: {}\n", render_scalar(other)),
    }
}

fn render_scalar(value: &Value) -> String {
    serde_yaml::to_string(value)
        .unwrap_or_default()
        .trim_end()
        .to_string()
}

/// True for the line that starts top-level `key`.
fn starts_key(line: &str, key: &str) -> bool {
    line.strip_prefix(key)
        .and_then(|rest| rest.strip_prefix(':'))
        .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t', '\n', '\r']))
}

/// Lines after a key line that still belong to its value: indented lines and
/// block list items, which YAML also allows at column zero.
fn continues_value(line: &str) -> bool {
    line.starts_with([' ', '\t']) || line.starts_with("- ") || line.trim_end() == "-"
}

fn set_key(text: &str, key: &str, value: &Value) -> String {
    let rendered = render_key(key, value);
    let Some((start, end)) = frontmatter_range(text) else {
        return format!("---\n{rendered}---\n{text}");
    };

    let yaml = &text[start..end];
    let mut out = String::with_capacity(text.len() + rendered.len());
    out.push_str(&text[..start]);

    let mut lines = yaml.split_inclusive('\n').peekable();
    let mut replaced = false;
    while let Some(line) = lines.next() {
        if !replaced && starts_key(line, key) {
            while lines.peek().is_some_and(|next| continues_value(next)) {
                lines.next();
            }
            out.push_str(&rendered);
            replaced = true;
        } else {
            out.push_str(line);
        }
    }
    if !replaced {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&rendered);
    }
    out.push_str(&text[end..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Value {
        Value::Sequence(items.iter().map(|s| Value::String(s.to_string())).collect())
    }

    #[test]
    fn replaces_block_list_and_keeps_other_lines() {
        let text = "---\ndone: false\ntags:\n  - a\n  - b\nproject: x # mine\n---\nbody\n";
        let out = set_key(text, "tags", &list(&["c"]));
        assert_eq!(
            out,
            "---\ndone: false\ntags:\n  - c\nproject: x # mine\n---\nbody\n"
        );
    }

    #[test]
    fn replaces_flow_list_and_column_zero_items() {
        let flow = "---\nsessions: [a, b]\ndone: false\n---\n";
        assert_eq!(
            set_key(flow, "sessions", &list(&["x"])),
            "---\nsessions:\n  - x\ndone: false\n---\n"
        );
        let col0 = "---\nsessions:\n- a\n- b\ndone: false\n---\n";
        assert_eq!(
            set_key(col0, "sessions", &list(&["x"])),
            "---\nsessions:\n  - x\ndone: false\n---\n"
        );
    }

    #[test]
    fn appends_missing_key_before_closing_fence() {
        let text = "---\ndone: false\n---\nbody";
        let out = set_key(text, "state", &Value::String("active".into()));
        assert_eq!(out, "---\ndone: false\nstate: active\n---\nbody");
    }

    #[test]
    fn creates_frontmatter_when_absent() {
        let out = set_key("# title\n", "tickets", &list(&["UCP-1"]));
        assert_eq!(out, "---\ntickets:\n  - UCP-1\n---\n# title\n");
        assert_eq!(parse_frontmatter(&out).unwrap().len(), 1);
    }

    #[test]
    fn does_not_match_key_prefixes() {
        let text = "---\nstate_old: x\n---\n";
        let out = set_key(text, "state", &Value::String("idea".into()));
        assert_eq!(out, "---\nstate_old: x\nstate: idea\n---\n");
    }

    #[test]
    fn quotes_only_when_needed() {
        assert_eq!(
            render_scalar(&Value::String("a9c0 wt 2026-10-08".into())),
            "a9c0 wt 2026-10-08"
        );
        assert_eq!(
            render_scalar(&Value::String("2026-10-08".into())),
            "2026-10-08"
        );
        assert_eq!(render_scalar(&Value::String("true".into())), "'true'");
    }

    #[test]
    fn agent_notes_section() {
        let first = append_under_heading("---\n---\nbody\n", AGENT_NOTES, "a");
        assert_eq!(first, "---\n---\nbody\n\n## Agent notes\n- a\n");
        let second = append_under_heading(&first, AGENT_NOTES, "b");
        assert_eq!(second, "---\n---\nbody\n\n## Agent notes\n- a\n- b\n");
        let later = format!("{second}\n## Mine\ntext\n");
        assert_eq!(
            append_under_heading(&later, AGENT_NOTES, "c"),
            "---\n---\nbody\n\n## Agent notes\n- a\n- b\n- c\n\n## Mine\ntext\n"
        );
        let nested = "## Agent notes\n- a\n### sub\nx\n";
        assert_eq!(
            append_under_heading(nested, AGENT_NOTES, "b"),
            "## Agent notes\n- a\n### sub\nx\n- b\n"
        );
    }

    #[test]
    fn empty_frontmatter() {
        let out = set_key("---\n---\nbody", "state", &Value::String("idea".into()));
        assert_eq!(out, "---\nstate: idea\n---\nbody");
    }
}
