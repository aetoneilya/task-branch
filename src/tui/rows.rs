//! The task tree as list rows: tasks → tickets → MRs, then the task's worktrees
//! → agents; tickets without a task and worktrees without a ticket after them.

use crate::table::look::{self, Piece, Tone};
use crate::table::view::{self, Hide, Node};
pub use crate::table::view::{AgentAt, Kind};
use crate::table::{Agent, RowState, Table};
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
        let is_folded = self.folded.contains(&node.id);
        let mut spans = vec![dim(format!("{indent}{glyph}"))];
        spans.extend(label(self.table, &node.kind));
        if is_folded && !node.children.is_empty() {
            spans.push(dim(format!("  ▸ {}", node.children.len())));
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
pub fn agent_spans(agent: &Agent, tone: Tone) -> Vec<Span<'static>> {
    spans([
        Piece::new(format!("{} {}", look::AGENT, agent.name), tone),
        look::agent_status(agent.status).after(" "),
    ])
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
