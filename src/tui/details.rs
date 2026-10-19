//! The details pane: its lines and the lazily loaded parts.

use super::{
    App, Update,
    rows::{self, AgentAt, Kind, dim, spans},
};
use crate::client::jira::JiraClient;
use crate::client::{herdr, worktree};
use crate::config::Config;
use crate::table::look::{self, Piece, Tone, clean};
use ratatui::text::{Line, Span};
use std::path::{Path, PathBuf};

/// The slow part of a row's details, loaded on selection.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Detail {
    /// A ticket's description, by key.
    Description(String),
    /// A worktree's git status and commits.
    Git {
        path: PathBuf,
        branch: Option<String>,
    },
    /// An agent's recent screen, by pane: unnamed agents and duplicate names
    /// read the right screen.
    Screen(String),
}

fn bold(text: impl Into<String>) -> Span<'static> {
    Piece::new(text, Tone::Bold).into()
}

impl App {
    pub(super) fn detail(&self, kind: &Kind) -> Option<Detail> {
        let table = &self.table;
        match kind {
            Kind::Loose(i) => Some(Detail::Description(table.loose[*i].key.clone())),
            Kind::Ticket(key) => Some(Detail::Description(key.clone())),
            Kind::Tree(t) => {
                let wt = &table.tree(*t).wt;
                Some(Detail::Git {
                    path: wt.path.clone(),
                    branch: wt.branch.clone(),
                })
            }
            Kind::Agent(at, a) => Some(Detail::Screen(table.agent(*at, *a).pane.clone())),
            _ => None,
        }
    }

    pub(super) fn load_details(&mut self) {
        if !self.preview_shown {
            return;
        }
        let Some(detail) = self.current().and_then(|r| self.detail(&r.kind)) else {
            return;
        };
        let loaded = self.details.contains_key(&detail) && !self.stale.contains(&detail);
        if loaded || self.pending.contains(&detail) {
            return;
        }
        self.stale.remove(&detail);
        self.pending.insert(detail.clone());
        let (config, tx) = (self.config.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let text = load_detail(&config, &detail);
            let _ = tx.send(Update::Detail(detail, text));
        });
    }

    pub(super) fn details_lines(&self) -> Vec<Line<'static>> {
        let Some(row) = self.current() else {
            return vec![Line::from("no active work — . shows everything")];
        };
        let table = &self.table;
        let mut lines: Vec<Line<'static>> = Vec::new();
        match &row.kind {
            Kind::Task(i) => {
                let row = &table.tasks[*i];
                let state = row.row_state();
                lines.push(Line::from(spans([
                    look::state(state, false),
                    Piece::new(row.task.name.clone(), Tone::Bold),
                ])));
                let mut meta = vec![dim(state.map_or("", |s| s.as_str()))];
                if !row.task.tags.is_empty() {
                    meta.push(dim(format!("  #{}", row.task.tags.join(" #"))));
                }
                lines.push(Line::from(meta));
                lines.push(Line::from(dim(display_path(&row.task.note.path))));
                for key in row.tickets() {
                    lines.push(Line::from(spans(look::ticket(key, table.tickets.get(key)))));
                }
                lines.push(Line::from(""));
                lines.extend(row.task.note.body().lines().map(|l| Line::from(clean(l))));
            }
            Kind::Loose(i) => self.ticket_lines(&table.loose[*i].key, &mut lines),
            Kind::Ticket(key) => self.ticket_lines(key, &mut lines),
            Kind::Mr(m) => {
                let mr = table.mr(*m);
                lines.push(Line::from(vec![
                    look::mr_iid(mr, false).into(),
                    bold(format!(" {}", clean(mr.mr.title()))),
                ]));
                lines.push(Line::from(dim(format!(
                    "{} · {}",
                    mr.mr.state().as_str(),
                    mr.mr.source_branch()
                ))));
                if let Some(review) = &mr.review {
                    let pipeline = look::pipeline(review).unwrap_or_else(|| Piece::dim("none"));
                    lines.push(Line::from(vec![Span::raw("pipeline "), pipeline.into()]));
                    lines.push(Line::from(if review.approved_by.is_empty() {
                        dim("no approvals")
                    } else {
                        Piece::new(
                            format!("approved by {}", review.approved_by.join(", ")),
                            Tone::Green,
                        )
                        .into()
                    }));
                    if review.unresolved > 0 {
                        lines.push(Line::from(Span::from(Piece::new(
                            format!("{} unresolved thread(s)", review.unresolved),
                            Tone::Yellow,
                        ))));
                    }
                }
                if let Some(status) = &mr.mr.detailed_merge_status {
                    lines.push(Line::from(dim(format!(
                        "merge: {}",
                        status.replace('_', " ")
                    ))));
                }
                if !mr.keys.is_empty() {
                    lines.push(Line::from(dim(mr.keys.join(" "))));
                }
                lines.push(Line::from(dim(mr.mr.web_url().to_string())));
            }
            Kind::Tree(t) => {
                let tree = table.tree(*t);
                lines.push(Line::from(bold(format!(
                    "{} {}",
                    look::TREE,
                    tree.wt.name()
                ))));
                lines.push(Line::from(dim(display_path(&tree.wt.path))));
                lines.push(Line::from(dim(tree.wt.branch.clone().unwrap_or_default())));
                let mut state = Vec::new();
                if let Some(c) = tree.changes {
                    state.push(Piece::new(format!("changed {c}"), Tone::Yellow));
                }
                if let Some(u) = tree.unpushed {
                    state.push(Piece::new(format!("  unpushed {u}"), Tone::Magenta));
                }
                if let Some(m) = tree.mr {
                    let mr = &table.mr(m).mr;
                    let text = format!("  !{} {}", mr.iid(), mr.state().as_str());
                    state.push(Piece::new(text, Tone::Blue));
                }
                lines.push(Line::from(spans(state)));
                for agent in &tree.agents {
                    lines.push(Line::from(rows::agent_spans(agent, Tone::Plain)));
                }
            }
            Kind::Header(_) => {}
            Kind::Agent(at, a) => {
                lines.push(Line::from(rows::agent_spans(
                    table.agent(*at, *a),
                    Tone::Bold,
                )));
                let place = match at {
                    AgentAt::Tree(t) => format!("in {}", table.tree(*t).wt.name()),
                    AgentAt::Task(_) => "in none of the task's worktrees".to_string(),
                };
                lines.push(Line::from(dim(place)));
            }
        }
        if let Some(detail) = self.detail(&row.kind) {
            lines.push(Line::from(""));
            match self.details.get(&detail) {
                Some(text) => lines.extend(text.lines().map(|l| Line::from(clean(l)))),
                None => lines.push(Line::from(dim(format!(
                    "{} loading",
                    super::anim::spinner()
                )))),
            }
        }
        lines
    }

    /// A ticket's key, summary, status and Jira page.
    fn ticket_lines(&self, key: &str, lines: &mut Vec<Line<'static>>) {
        match self.table.tickets.get(key) {
            Some(t) => {
                lines.push(Line::from(vec![
                    look::key(key).into(),
                    bold(format!(" {}", clean(&t.summary))),
                ]));
                lines.push(Line::from(Span::from(look::status(t))));
            }
            None => lines.push(Line::from(Span::from(look::key(key)))),
        }
        lines.push(Line::from(dim(self.table.ticket_url(key))));
    }
}

/// Jira wiki markup as plain text worth reading: headings, lists, code blocks,
/// links, monospace, images and tables lose their syntax.
fn jira_text(markup: &str) -> String {
    use regex::Regex;
    use std::sync::LazyLock;
    static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^h([1-6])\.\s*").unwrap());
    static BLOCK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\{(code|noformat|quote|panel)(:[^}]*)?\}\s*$").unwrap());
    static BULLET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([*#-]+)\s+").unwrap());
    static LINK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\[([^|\]]+)\|([^\]]+)\]").unwrap());
    static BARE_LINK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\[(https?://[^\]]+)\]").unwrap());
    static MONO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{\{(.+?)\}\}").unwrap());
    static IMAGE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"!([^!|\s]+)(\|[^!]*)?!").unwrap());
    static COLOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{color(:[^}]*)?\}").unwrap());

    let mut out = Vec::new();
    let mut in_block = false;
    for line in markup.lines() {
        if BLOCK.is_match(line.trim()) {
            in_block = !in_block;
            out.push("────".to_string());
            continue;
        }
        if in_block {
            out.push(format!("  {line}"));
            continue;
        }
        let mut text = line.to_string();
        if let Some(c) = HEADING.captures(&text) {
            let level: usize = c[1].parse().unwrap_or(1);
            text = format!("{} {}", "#".repeat(level), &text[c[0].len()..]);
        } else if let Some(c) = BULLET.captures(&text) {
            let depth = c[1].len();
            text = format!("{}• {}", "  ".repeat(depth - 1), &text[c[0].len()..]);
        } else if text.trim() == "----" {
            text = "────────".to_string();
        }
        text = LINK.replace_all(&text, "$1 ($2)").into_owned();
        text = BARE_LINK.replace_all(&text, "$1").into_owned();
        text = MONO.replace_all(&text, "`$1`").into_owned();
        text = IMAGE.replace_all(&text, "[image $1]").into_owned();
        text = COLOR.replace_all(&text, "").into_owned();
        text = text.replace("||", " | ");
        out.push(text);
    }
    out.join("\n")
}

fn display_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    match dirs::home_dir() {
        Some(home) => path.replacen(&*home.to_string_lossy(), "~", 1),
        None => path.to_string(),
    }
}

/// Ticket description, worktree's git status and unpushed commits, or the
/// agent's recent screen.
fn load_detail(config: &Config, detail: &Detail) -> String {
    match detail {
        Detail::Description(key) => match JiraClient::from_env(&config.jira.host)
            .and_then(|c| c.get_issue_by_key(key).map_err(|e| e.to_string()))
        {
            Ok(issue) => issue
                .fields
                .description
                .map(|d| jira_text(&d))
                .unwrap_or_else(|| "(no description)".to_string()),
            Err(e) => format!("! {e}"),
        },
        Detail::Git { path, branch } => git_detail(path, branch.as_deref()),
        Detail::Screen(pane) => herdr::read_agent(pane, 30).unwrap_or_else(|e| format!("! {e}")),
    }
}

fn git_detail(path: &Path, branch: Option<&str>) -> String {
    let status = worktree::short_status(path).unwrap_or_default();
    let log = |range: &str| worktree::log_oneline(path, range, 15).unwrap_or_default();
    let unpushed = match branch {
        Some(branch) if worktree::on_origin(path, branch) => log(&format!("origin/{branch}..HEAD")),
        _ => "branch not pushed".to_string(),
    };
    let commits = log("origin/HEAD..HEAD");
    let mut out = String::new();
    if status.is_empty() {
        out.push_str("nothing uncommitted\n");
    } else {
        out.push_str(&format!("uncommitted:\n{status}\n"));
    }
    if !unpushed.is_empty() {
        out.push_str(&format!("\nunpushed:\n{unpushed}\n"));
    }
    if !commits.is_empty() {
        out.push_str(&format!("\nbranch commits:\n{commits}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::jira_text;

    #[test]
    fn jira_markup_reads_as_text() {
        let markup = "h2. Цель\n* пункт с {{code}}\n** вложенный [ссылка|https://x.y]\n{code:java}\nint a;\n{code}\n!shot.png|thumbnail!";
        assert_eq!(
            jira_text(markup),
            "## Цель\n• пункт с `code`\n  • вложенный ссылка (https://x.y)\n────\n  int a;\n────\n[image shot.png]"
        );
    }
}
