//! The task tree as list rows: tasks → tickets → MRs, then the task's worktrees
//! → agents; tickets without a task and worktrees without a ticket after them.

use crate::table::{Table, Tree};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

static LEADING_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z][A-Z0-9]+-\d+:?\s*").unwrap());

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
                    while let Some(c) = chars.next() {
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

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Task(usize),
    Loose(usize),
    Ticket(String),
    Mr(usize),
    Tree(usize),
    /// A worktree's agent: tree index, index in `Tree::agents`.
    Agent(usize, usize),
    /// A section title; never selected.
    Header,
}

struct Node {
    id: String,
    kind: Kind,
    children: Vec<Node>,
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

/// `all` adds done tasks, tickets and worktrees without activity, merged MRs;
/// `filter` keeps the roots whose names, tickets or branches contain it.
pub fn build(table: &Table, all: bool, filter: &str, folded: &HashSet<String>) -> Vec<Row> {
    let filter = filter.to_lowercase();
    let keep = |text: String| filter.is_empty() || text.to_lowercase().contains(&filter);
    let mut roots = Vec::new();

    // the table keeps tasks by state, newest first; done ones come last
    for (i, row) in table.tasks.iter().enumerate() {
        if row.task.done && !all {
            continue;
        }
        let summaries: Vec<&str> = row
            .task
            .tickets
            .iter()
            .chain(&row.found_tickets)
            .filter_map(|k| table.tickets.get(k))
            .map(|t| t.summary.as_str())
            .collect();
        let text = format!(
            "{} #{} {} {} {} {}",
            row.task.name,
            row.task.tags.join(" #"),
            row.task.tickets.join(" "),
            row.found_tickets.join(" "),
            summaries.join(" "),
            branches(table, &row.trees)
        );
        if !keep(text) {
            continue;
        }
        // an MR from a shown worktree's branch hangs under that worktree
        let (trees, held) = tree_nodes(table, &row.trees, all);
        let free_mr = |m: usize| !held.contains(&m) && (all || table.mr(m).opened());
        let mut children = Vec::new();
        for key in row.task.tickets.iter().chain(&row.found_tickets) {
            let mrs = row
                .mrs
                .iter()
                .copied()
                .filter(|&m| table.mr(m).keys.contains(key) && free_mr(m))
                .map(mr_node)
                .collect();
            children.push(Node {
                id: format!("ticket:{}:{key}", row.task.name),
                kind: Kind::Ticket(key.clone()),
                children: mrs,
            });
        }
        children.extend(
            row.mrs
                .iter()
                .copied()
                .filter(|&m| {
                    let keys = &table.mr(m).keys;
                    !keys.iter().any(|k| row.task.tickets.contains(k) || row.found_tickets.contains(k))
                        && free_mr(m)
                })
                .map(mr_node),
        );
        children.extend(trees);
        roots.push(Node {
            id: format!("task:{}", row.task.name),
            kind: Kind::Task(i),
            children,
        });
    }

    // tickets without a task that I work on (a worktree or an open MR), and
    // worktrees without a ticket; then, apart, open Jira tickets assigned to me
    // that nothing is done on yet
    let mut no_task = Vec::new();
    let mut assigned = Vec::new();
    for (i, loose) in table.loose.iter().enumerate() {
        let ticket = table.tickets.get(&loose.key);
        let live = loose.trees.iter().any(|&t| !table.tree(t).finished);
        let open = loose.mrs.iter().any(|&m| table.mr(m).opened());
        let closed = ticket.is_some_and(|t| t.closed());
        let worked_on = live || (open && !closed);
        let mine = ticket.is_some_and(|t| t.mine);
        if !worked_on && !mine && !all {
            continue;
        }
        let text = format!(
            "{} {} {}",
            loose.key,
            ticket.map(|t| t.summary.as_str()).unwrap_or(""),
            branches(table, &loose.trees)
        );
        if !keep(text) {
            continue;
        }
        let (trees, held) = tree_nodes(table, &loose.trees, all);
        let mut children: Vec<Node> = loose
            .mrs
            .iter()
            .copied()
            .filter(|&m| !held.contains(&m) && (all || table.mr(m).opened()))
            .map(mr_node)
            .collect();
        children.extend(trees);
        let node = Node {
            id: format!("loose:{}", loose.key),
            kind: Kind::Loose(i),
            children,
        };
        if !worked_on && mine {
            assigned.push(node);
        } else {
            no_task.push(node);
        }
    }

    for node in tree_nodes(table, &table.other_trees, all).0 {
        if let Kind::Tree(t) = node.kind {
            if keep(format!("{} {}", table.tree(t).wt.name(), branches(table, &[t]))) {
                no_task.push(node);
            }
        }
    }
    if all {
        for &m in &table.other_mrs {
            if keep(table.mr(m).mr.title().to_string()) {
                no_task.push(mr_node(m));
            }
        }
    }
    // in progress before new, then newest first (the table's order)
    assigned.sort_by_key(|n| match &n.kind {
        Kind::Loose(i) => match table.tickets.get(&table.loose[*i].key) {
            Some(t) if t.category == "indeterminate" => 0,
            _ => 1,
        },
        _ => 2,
    });
    for (title, section) in [("no task", no_task), ("assigned to me in Jira", assigned)] {
        if !section.is_empty() {
            roots.push(Node {
                id: format!("header:{title}"),
                kind: Kind::Header,
                children: Vec::new(),
            });
            roots.extend(section);
        }
    }

    let mut rows = Vec::new();
    for node in &roots {
        flatten(table, node, None, &node.kind, "", "", folded, &mut rows);
    }
    rows
}

fn branches(table: &Table, trees: &[usize]) -> String {
    trees
        .iter()
        .filter_map(|&t| table.tree(t).wt.branch.clone())
        .collect::<Vec<_>>()
        .join(" ")
}

fn mr_node(m: usize) -> Node {
    Node {
        id: format!("mr:{m}"),
        kind: Kind::Mr(m),
        children: Vec::new(),
    }
}

/// Worktrees with agents first, then by changed files; finished ones only with
/// `all`. Under each, the MR of its branch and its agents. Also returns the MRs
/// placed so, which go nowhere else.
fn tree_nodes(table: &Table, trees: &[usize], all: bool) -> (Vec<Node>, Vec<usize>) {
    let mut trees: Vec<usize> = trees
        .iter()
        .copied()
        .filter(|&t| all || !table.tree(t).finished)
        .collect();
    trees.sort_by_key(|&t| {
        let tree = table.tree(t);
        (
            std::cmp::Reverse(!tree.agents.is_empty()),
            std::cmp::Reverse(tree.changes.unwrap_or(0)),
            tree.wt.name(),
        )
    });
    let held: Vec<usize> = trees.iter().filter_map(|&t| table.tree(t).mr).collect();
    let nodes = trees
        .into_iter()
        .map(|t| {
            let tree = table.tree(t);
            let mr = tree.mr.filter(|&m| all || table.mr(m).opened()).map(mr_node);
            let agents = (0..tree.agents.len()).map(|a| Node {
                id: format!("agent:{}:{a}", tree.wt.path.display()),
                kind: Kind::Agent(t, a),
                children: Vec::new(),
            });
            Node {
                id: format!("tree:{}", tree.wt.path.display()),
                kind: Kind::Tree(t),
                children: mr.into_iter().chain(agents).collect(),
            }
        })
        .collect();
    (nodes, held)
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
        let title = node.id.trim_start_matches("header:");
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
        flatten(table, child, Some(index), root, glyph, &child_indent, folded, rows);
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

pub fn task_state(table: &Table, i: usize) -> String {
    let row = &table.tasks[i];
    if row.task.done {
        return "done".to_string();
    }
    row.state
        .map(str::to_string)
        .or_else(|| row.task.state.clone())
        .unwrap_or_default()
}

pub fn status_color(category: &str) -> Color {
    match category {
        "done" => Color::Green,
        "indeterminate" => Color::Yellow,
        _ => Color::DarkGray,
    }
}

pub fn agent_color(status: &str) -> Color {
    match status {
        "working" => Color::Green,
        "blocked" => Color::Red,
        "idle" | "done" => Color::Blue,
        _ => Color::DarkGray,
    }
}

fn label(table: &Table, kind: &Kind) -> Vec<Span<'static>> {
    match kind {
        Kind::Task(i) => {
            let row = &table.tasks[*i];
            let mut spans = vec![
                state_mark(&task_state(table, *i)),
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
            let live: Vec<&Tree> = loose
                .trees
                .iter()
                .map(|&t| table.tree(t))
                .filter(|t| !t.finished)
                .collect();
            let open = loose.mrs.iter().any(|&m| table.mr(m).opened());
            if live.is_empty() && !open {
                // nothing done on it yet: its Jira status says more
                let mut spans = vec![dim("○ "), colored(loose.key.clone(), Color::Cyan)];
                if let Some(t) = ticket {
                    spans.push(colored(format!(" {}", t.status), status_color(&t.category)));
                }
                spans.push(Span::raw(format!(" {summary}")));
                return spans;
            }
            let review = open && live.iter().all(|t| t.clean());
            vec![
                state_mark(if review { "review" } else { "active" }),
                colored(loose.key.clone(), Color::Cyan),
                Span::raw(format!(" {summary}")),
            ]
        }
        Kind::Header => Vec::new(),
        Kind::Ticket(key) => match table.tickets.get(key) {
            Some(t) => vec![
                colored(key.clone(), Color::Cyan),
                colored(format!(" {}", t.status), status_color(&t.category)),
                Span::raw(format!(" {}", clean(&t.summary))),
            ],
            None => vec![colored(key.clone(), Color::Cyan)],
        },
        Kind::Mr(m) => {
            let mr = &table.mr(*m).mr;
            let state_color = match mr.state() {
                "opened" => Color::Green,
                "merged" => Color::Magenta,
                _ => Color::DarkGray,
            };
            vec![
                colored(format!("!{}", mr.iid()), Color::Blue),
                colored(format!(" {}", mr.state()), state_color),
                dim(format!(" {}", clean(&LEADING_KEY.replace(mr.title(), "")))),
            ]
        }
        Kind::Tree(t) => tree_label(table.tree(*t)),
        Kind::Agent(t, a) => {
            let label = &table.tree(*t).agents[*a];
            let (name, status) = label.rsplit_once(':').unwrap_or((label, ""));
            vec![
                Span::raw(format!("◉ {name}")),
                colored(format!(" {status}"), agent_color(status)),
            ]
        }
    }
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
