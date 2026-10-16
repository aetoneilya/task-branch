//! The task tree as list rows: tasks → tickets → MRs, then the task's worktrees
//! → agents; tickets without a task and worktrees without a ticket after them.

use crate::client::gitlab::MrState;
pub use crate::table::view::Kind;
use crate::table::view::{self, Node};
use crate::table::{AgentStatus, Category, Mr, Table, Tree};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::HashSet;

/// Days after which an open MR or an untouched worktree gets flagged.
const STALE_DAYS: i64 = 7;

/// Text from notes, Jira, GitLab and agents made safe to draw: a tab or a
/// control character has no width for ratatui but moves the terminal's cursor,
/// so the line spills past its pane and leaves debris when it scrolls. Tabs
/// become spaces; escape sequences, other control characters and invisible
/// joiners/variation selectors are dropped.
pub fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\t' => out.push_str("    "),
            '\x1b' => {
                // CSI `ESC [ … final` or a two-character escape
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                } else {
                    chars.next();
                }
            }
            '\u{200b}'..='\u{200f}' | '\u{fe0e}' | '\u{fe0f}' | '\u{2060}' => {}
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

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
pub fn build(table: &Table, all: bool, filter: &str, folded: &HashSet<String>) -> Vec<Row> {
    let options = view::Options {
        all,
        filter,
        tag: None,
    };
    let mut rows = Vec::new();
    for node in &view::build(table, &options) {
        flatten(table, node, None, &node.kind, "", "", folded, &mut rows);
    }
    rows
}

#[allow(clippy::too_many_arguments)]
fn flatten(
    table: &Table,
    node: &Node,
    parent: Option<usize>,
    root: &Kind,
    glyph: &str,
    indent: &str,
    folded: &HashSet<String>,
    rows: &mut Vec<Row>,
) {
    if node.kind == Kind::Header {
        let title = node.title();
        rows.push(Row {
            id: node.id.clone(),
            kind: Kind::Header,
            root: Kind::Header,
            line: Line::from(dim(format!("── {title} ──────────────────────────────"))),
            parent: None,
            has_children: false,
        });
        return;
    }
    let is_folded = folded.contains(&node.id);
    let mut spans = vec![Span::styled(
        format!("{indent}{glyph}"),
        Style::default().fg(Color::DarkGray),
    )];
    spans.extend(label(table, &node.kind));
    if is_folded && !node.children.is_empty() {
        spans.push(dim(format!("  ▸ {}", node.children.len())));
    }
    let index = rows.len();
    rows.push(Row {
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
        flatten(
            table,
            child,
            Some(index),
            root,
            glyph,
            &child_indent,
            folded,
            rows,
        );
    }
}

fn dim(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(Color::DarkGray))
}

fn colored(text: impl Into<String>, color: Color) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(color))
}

pub fn state_mark(state: &str) -> Span<'static> {
    match state {
        "active" => colored("● ", Color::Green),
        "review" => colored("◐ ", Color::Yellow),
        "finished" => colored("◆ ", Color::Magenta),
        "done" => colored("✓ ", Color::DarkGray),
        _ => colored("○ ", Color::DarkGray),
    }
}

pub fn status_color(category: Category) -> Color {
    match category {
        Category::Done => Color::Green,
        Category::InProgress => Color::Yellow,
        Category::New | Category::Unknown => Color::DarkGray,
    }
}

pub fn agent_color(status: AgentStatus) -> Color {
    match status {
        AgentStatus::Working => Color::Green,
        AgentStatus::Blocked => Color::Red,
        AgentStatus::Idle | AgentStatus::Done => Color::Blue,
        AgentStatus::Unknown => Color::DarkGray,
    }
}

/// `◉ name status`, the status colored by what it needs from me.
pub fn agent_spans(agent: &crate::table::Agent) -> Vec<Span<'static>> {
    vec![
        Span::raw(format!("◉ {}", agent.name)),
        colored(
            format!(" {}", agent.status.as_str()),
            agent_color(agent.status),
        ),
    ]
}

fn label(table: &Table, kind: &Kind) -> Vec<Span<'static>> {
    match kind {
        Kind::Task(i) => {
            let row = &table.tasks[*i];
            let mut spans = vec![
                state_mark(row.state_label()),
                Span::styled(
                    row.task.name.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ];
            if !row.task.tags.is_empty() {
                spans.push(dim(format!(" #{}", row.task.tags.join(" #"))));
            }
            spans
        }
        Kind::Loose(i) => {
            let loose = &table.loose[*i];
            let ticket = table.tickets.get(&loose.key);
            let summary = clean(ticket.map(|t| t.summary.as_str()).unwrap_or(""));
            let Some(state) = table.loose_state(*i) else {
                // nothing done on it yet: its Jira status says more
                let mut spans = vec![dim("○ "), colored(loose.key.clone(), Color::Cyan)];
                if let Some(t) = ticket {
                    spans.push(colored(format!(" {}", t.status), status_color(t.category)));
                }
                spans.push(Span::raw(format!(" {summary}")));
                return spans;
            };
            vec![
                state_mark(state.as_str()),
                colored(loose.key.clone(), Color::Cyan),
                Span::raw(format!(" {summary}")),
            ]
        }
        Kind::Header => Vec::new(),
        Kind::Ticket(key) => match table.tickets.get(key) {
            Some(t) => vec![
                colored(key.clone(), Color::Cyan),
                colored(format!(" {}", t.status), status_color(t.category)),
                Span::raw(format!(" {}", clean(&t.summary))),
            ],
            None => vec![colored(key.clone(), Color::Cyan)],
        },
        Kind::Mr(m) => {
            let mr = &table.mr(*m).mr;
            let mut spans = vec![
                colored(format!("!{}", mr.iid()), Color::Blue),
                colored(
                    format!(" {}", mr.state().as_str()),
                    mr_state_color(mr.state()),
                ),
            ];
            spans.extend(review_spans(table.mr(*m)));
            spans.push(dim(format!(" {}", clean(&table.mr(*m).short_title()))));
            spans
        }
        Kind::Tree(t) => tree_label(table.tree(*t)),
        Kind::Agent(t, a) => agent_spans(&table.tree(*t).agents[*a]),
    }
}

pub fn mr_state_color(state: MrState) -> Color {
    match state {
        MrState::Opened => Color::Green,
        MrState::Merged => Color::Magenta,
        MrState::Closed | MrState::Locked | MrState::Other => Color::DarkGray,
    }
}

/// What an open MR waits on: draft, pipeline, approvals, unresolved threads,
/// conflicts — short enough for a row.
pub fn review_spans(mr: &Mr) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if !mr.opened() {
        return spans;
    }
    if mr.mr.draft {
        spans.push(dim(" draft"));
    }
    // waiting long for review is worth seeing
    let age = mr
        .mr
        .created_at
        .as_deref()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_days());
    if let Some(days) = age.filter(|&d| d >= STALE_DAYS) {
        spans.push(colored(format!(" {days}d"), Color::Yellow));
    }
    let Some(review) = &mr.review else {
        return spans;
    };
    match review.pipeline.as_deref() {
        Some("success") => spans.push(colored(" ci✓", Color::Green)),
        Some("failed") => spans.push(colored(" ci✗", Color::Red)),
        Some("canceled" | "skipped") => spans.push(dim(" ci–")),
        Some(_) => spans.push(colored(" ci…", Color::Yellow)),
        None => {}
    }
    if !review.approved_by.is_empty() {
        spans.push(colored(
            format!(" ✔{}", review.approved_by.len()),
            Color::Green,
        ));
    }
    match review.unresolved {
        0 => {}
        1 => spans.push(colored(" 1 thread", Color::Yellow)),
        n => spans.push(colored(format!(" {n} threads"), Color::Yellow)),
    }
    match mr.mr.detailed_merge_status.as_deref() {
        Some("conflict") => spans.push(colored(" conflict", Color::Red)),
        Some("need_rebase") => spans.push(colored(" rebase", Color::Red)),
        _ => {}
    }
    spans
}

fn tree_label(tree: &Tree) -> Vec<Span<'static>> {
    let mut spans = vec![Span::raw(format!("⌂ {}", tree.wt.name()))];
    if !tree.keys.is_empty() {
        spans.push(colored(format!(" {}", tree.keys.join(" ")), Color::Cyan));
    }
    if let Some(c) = tree.changes.filter(|&c| c > 0) {
        spans.push(colored(format!(" +{c}"), Color::Yellow));
    }
    if let Some(u) = tree.unpushed.filter(|&u| u > 0) {
        spans.push(colored(format!(" ↑{u}"), Color::Magenta));
    }
    if tree.finished {
        spans.push(dim(" ✓ merged"));
    } else if tree.agents.is_empty() && tree.changes == Some(0) {
        // nobody on it and no commits for a while: forgotten, or done and not cleaned up
        let quiet = tree
            .last_commit
            .map(|t| (chrono::Utc::now().timestamp() - t) / 86_400)
            .filter(|&d| d >= STALE_DAYS);
        if let Some(days) = quiet {
            spans.push(colored(format!(" quiet {days}d"), Color::Yellow));
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::clean;

    #[test]
    fn clean_keeps_text_and_drops_what_breaks_the_layout() {
        assert_eq!(clean("\t\t- [ ] объединить"), "        - [ ] объединить");
        assert_eq!(clean("\x1b[1;31mred\x1b[0m ok\r"), "red ok");
        assert_eq!(clean("✔\u{fe0f} done\u{200b}"), "✔ done");
    }
}
