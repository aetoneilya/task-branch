//! The task tree as list rows: tasks → tickets → MRs, then the task's worktrees
//! → agents; tickets without a task and worktrees without a ticket after them.

use crate::table::look::{self, Piece, Tone};
use crate::table::view::{self, Hide, Node};
pub use crate::table::view::{AgentAt, Kind};
use crate::table::{Agent, AgentStatus, RowState, Table};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::HashSet;

pub struct Row {
    pub id: String,
    pub kind: Kind,
    /// The task or loose ticket the row is under; itself for a root.
    pub root: Kind,
    pub line: Line<'static>,
    pub parent: Option<usize>,
    pub has_children: bool,
}

/// The view's tree as list rows, folded nodes without their children.
pub fn build(
    table: &Table,
    all: bool,
    hide: Hide,
    filter: &str,
    folded: &HashSet<String>,
) -> Vec<Row> {
    let options = view::Options {
        all,
        filter,
        hide,
        ..Default::default()
    };
    let mut rows = Rows {
        table,
        folded,
        rows: Vec::new(),
    };
    for node in &view::build(table, &options) {
        rows.add(node, None, &node.kind, "", "");
    }
    rows.rows
}

struct Rows<'a> {
    table: &'a Table,
    folded: &'a HashSet<String>,
    rows: Vec<Row>,
}

impl Rows<'_> {
    /// What a folded row hides: `▸ 2 tasks · 3 wt · 4 MR · 1 working`.
    fn folded_summary(&self, node: &Node) -> Vec<Span<'static>> {
        fn count(table: &Table, node: &Node, n: &mut [usize; 5]) {
            for child in &node.children {
                match child.kind {
                    Kind::Task(_) => n[0] += 1,
                    Kind::Tree(_) => n[1] += 1,
                    Kind::Mr(_) => n[2] += 1,
                    Kind::Agent(at, a) => match table.agent(at, a).status {
                        AgentStatus::Working => n[3] += 1,
                        status if status.waiting() => n[4] += 1,
                        _ => {}
                    },
                    _ => {}
                }
                count(table, child, n);
            }
        }
        let mut n = [0; 5];
        count(self.table, node, &mut n);
        let parts: Vec<String> = [(n[0], "task"), (n[1], "wt"), (n[2], "MR")]
            .into_iter()
            .filter(|(k, _)| *k > 0)
            .map(|(k, what)| match (what, k) {
                ("task", k) if k > 1 => format!("{k} tasks"),
                (what, k) => format!("{k} {what}"),
            })
            .collect();
        let mut spans = vec![dim(format!("  ▸ {}", parts.join(" · ")))];
        if parts.is_empty() {
            spans = vec![dim(format!("  ▸ {}", node.children.len()))];
        }
        if n[3] > 0 {
            spans.push(Span::styled(
                format!(" · {} working", n[3]),
                style(Tone::Green),
            ));
        }
        if n[4] > 0 {
            spans.push(Span::styled(
                format!(" · {} waiting", n[4]),
                style(Tone::Red),
            ));
        }
        spans
    }

    /// The node's row under `parent`, drawn after `indent` and `glyph`, then
    /// its children unless folded.
    fn add(&mut self, node: &Node, parent: Option<usize>, root: &Kind, glyph: &str, indent: &str) {
        if let Kind::Header(section) = node.kind {
            self.rows.push(Row {
                id: node.id.clone(),
                kind: node.kind.clone(),
                root: node.kind.clone(),
                line: Line::from(dim(format!(
                    "── {} ──────────────────────────────",
                    section.title()
                ))),
                parent: None,
                has_children: false,
            });
            return;
        }
        // a subtask is a root of its own for what keys act on
        let root = if let Kind::Task(_) = node.kind {
            &node.kind
        } else {
            root
        };
        let is_folded = self.folded.contains(&node.id);
        let mut spans = vec![dim(format!("{indent}{glyph}"))];
        spans.extend(label(self.table, &node.kind));
        if is_folded && !node.children.is_empty() {
            spans.extend(self.folded_summary(node));
        }
        let index = self.rows.len();
        self.rows.push(Row {
            id: node.id.clone(),
            kind: node.kind.clone(),
            root: root.clone(),
            line: Line::from(spans),
            parent,
            has_children: !node.children.is_empty(),
        });
        if is_folded {
            return;
        }
        // roots have no glyph: their children start at the left edge
        let child_indent = match glyph {
            "" => String::new(),
            "├ " => format!("{indent}│ "),
            _ => format!("{indent}  "),
        };
        let count = node.children.len();
        for (n, child) in node.children.iter().enumerate() {
            let glyph = if n + 1 == count { "└ " } else { "├ " };
            self.add(child, Some(index), root, glyph, &child_indent);
        }
    }
}

fn style(tone: Tone) -> Style {
    let fg = |color| Style::default().fg(color);
    match tone {
        Tone::Plain => Style::default(),
        Tone::Bold => Style::default().add_modifier(Modifier::BOLD),
        Tone::Dim => fg(Color::DarkGray),
        Tone::Green => fg(Color::Green),
        Tone::Yellow => fg(Color::Yellow),
        Tone::Blue => fg(Color::Blue),
        Tone::Cyan => fg(Color::Cyan),
        Tone::Magenta => fg(Color::Magenta),
        Tone::Red => fg(Color::Red),
    }
}

impl From<Piece> for Span<'static> {
    fn from(piece: Piece) -> Span<'static> {
        Span::styled(piece.text, style(piece.tone))
    }
}

pub fn spans(pieces: impl IntoIterator<Item = Piece>) -> Vec<Span<'static>> {
    pieces.into_iter().map(Span::from).collect()
}

pub fn dim(text: impl Into<String>) -> Span<'static> {
    Piece::dim(text).into()
}

/// `◉ name status`, the name in `tone`.
/// The status is the last span: `agent_status_span`. An agent idle for a day
/// or more fades.
pub fn agent_spans(agent: &Agent, tone: Tone) -> Vec<Span<'static>> {
    let stale = idle_since(agent).is_some_and(|t| chrono::Utc::now().timestamp() - t >= 86_400);
    let tone = if stale { Tone::Dim } else { tone };
    vec![
        Piece::new(format!("{} {}", look::AGENT, agent.name), tone).into(),
        agent_status_span(agent),
    ]
}

/// When an agent that is not working last did something.
fn idle_since(agent: &Agent) -> Option<i64> {
    agent
        .active
        .filter(|_| agent.status != AgentStatus::Working)
        .map(|t| t as i64)
}

/// ` ⠹ working` with a moving spinner, or ` idle 2h` with how long ago the
/// agent last did something.
pub fn agent_status_span(agent: &Agent) -> Span<'static> {
    let mut piece = look::agent_status(agent.status);
    if agent.status == AgentStatus::Working {
        piece.text = format!("{} {}", super::anim::spinner(), piece.text);
    } else if let Some(t) = idle_since(agent) {
        piece.text = format!("{} {}", piece.text, look::ago(t));
    }
    piece.after(" ").into()
}

fn label(table: &Table, kind: &Kind) -> Vec<Span<'static>> {
    let pieces = match kind {
        Kind::Task(i) => {
            let row = &table.tasks[*i];
            let mut pieces = vec![
                look::state(row.row_state(), false),
                Piece::new(row.task.name.clone(), Tone::Bold),
            ];
            if !row.task.tags.is_empty() {
                pieces.push(Piece::dim(format!(" #{}", row.task.tags.join(" #"))));
            }
            pieces.extend(look::progress(table, *i));
            pieces
        }
        Kind::Loose(i) => {
            let loose = &table.loose[*i];
            let ticket = table.tickets.get(&loose.key);
            let mut pieces = vec![
                look::state(loose.state.map(RowState::Task), false),
                look::key(&loose.key),
            ];
            // nothing done on it yet: its Jira status says more
            if loose.state.is_none()
                && let Some(t) = ticket
            {
                pieces.push(look::status(t).after(" "));
            }
            pieces.push(Piece::plain(format!(" {}", look::summary(ticket))));
            pieces
        }
        Kind::Header(_) => Vec::new(),
        Kind::Ticket(key) => look::ticket(key, table.tickets.get(key)),
        Kind::Mr(m) => {
            let mr = table.mr(*m);
            let mut pieces = vec![look::mr_iid(mr, false), look::mr_state(mr).after(" ")];
            pieces.extend(look::review(mr));
            pieces.push(Piece::dim(format!(" {}", look::mr_title(mr))));
            if let Some(t) = mr.mr.updated_at.as_deref().and_then(look::unix) {
                pieces.push(Piece::dim(format!(" · {}", look::ago(t))));
            }
            pieces
        }
        Kind::Tree(t) => {
            let tree = table.tree(*t);
            let mut pieces = vec![Piece::plain(format!("{} {}", look::TREE, tree.wt.name()))];
            if !tree.keys.is_empty() {
                pieces.push(look::key(&tree.keys.join(" ")).after(" "));
            }
            pieces.extend(look::tree_marks(tree));
            pieces
        }
        Kind::Agent(at, a) => return agent_spans(table.agent(*at, *a), Tone::Plain),
    };
    spans(pieces)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tones_map_to_ratatui_styles() {
        let span = Span::from(Piece::dim("x"));
        assert_eq!(span.style.fg, Some(Color::DarkGray));
        let bold = Span::from(Piece::new("x", Tone::Bold));
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(Span::from(Piece::plain("x")).style, Style::default());
    }
}
