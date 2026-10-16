use crate::client::gitlab::MrState;
use crate::config::{Config, VaultConfig};
use crate::table::view::{self, Kind, Node};
use crate::table::{self, AgentStatus, Category, Table, Ticket, Tree};
use crate::trace;
use crate::vault::task::TaskState;
use crate::vault::task::{Task, load_tasks};
use serde_yaml::Value;
use std::io::IsTerminal;
use std::sync::LazyLock;

pub(super) fn vault(config: &Config) -> Result<&VaultConfig, String> {
    config
        .vault
        .as_ref()
        .ok_or_else(|| "no [vault] section in ~/.config/worktable/config.toml".to_string())
}

pub(super) fn load(config: &Config) -> Result<Vec<Task>, String> {
    let (tasks, errors) = load_tasks(vault(config)?);
    for err in errors {
        eprintln!("skipped {err}");
    }
    Ok(tasks)
}

pub(super) fn cli_status(
    config: &Config,
    tag: Option<String>,
    all: bool,
    no_write: bool,
) -> Result<(), String> {
    let tasks = trace::time("tasks", || load(config))?;
    let mut table = trace::time("build", || table::build(config, tasks, true));
    let updated = if no_write {
        Vec::new()
    } else {
        trace::time("write back", || table::write_back(&mut table))
    };
    trace::time("render", || {
        print_status(&table, tag.as_deref(), all);
        print_notes(&table, &updated);
    });
    Ok(())
}

/// Visible text and the same text with colors and terminal hyperlinks.
#[derive(Default)]
struct Cell {
    text: String,
    rendered: String,
}

impl Cell {
    fn plain(text: impl Into<String>) -> Cell {
        Cell::styled(text, Style::Plain)
    }

    fn styled(text: impl Into<String>, style: Style) -> Cell {
        let text = text.into();
        Cell {
            rendered: paint(&text, style),
            text,
        }
    }

    /// A terminal hyperlink, on a terminal only: piped output stays plain text.
    fn link(text: impl Into<String>, url: &str, style: Style) -> Cell {
        let text = text.into();
        let painted = paint(&text, style);
        Cell {
            rendered: if *COLOR {
                hyperlink(url, &painted)
            } else {
                painted
            },
            text,
        }
    }

    fn push(&mut self, other: Cell) {
        self.text.push_str(&other.text);
        self.rendered.push_str(&other.rendered);
    }

    fn join(cells: Vec<Cell>, sep: &str) -> Cell {
        let mut out = Cell::default();
        for (i, cell) in cells.into_iter().enumerate() {
            if i > 0 {
                out.push(Cell::plain(sep));
            }
            out.push(cell);
        }
        out
    }
}

#[derive(Clone, Copy)]
enum Style {
    Plain,
    Bold,
    Dim,
    Green,
    Yellow,
    Blue,
    Cyan,
    Magenta,
    Red,
}

/// An OSC 8 terminal hyperlink.
fn hyperlink(url: &str, text: &str) -> String {
    format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", url, text)
}

static COLOR: LazyLock<bool> =
    LazyLock::new(|| std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none());

/// Colors only on a terminal, and never with NO_COLOR set.
fn paint(text: &str, style: Style) -> String {
    let code = match style {
        Style::Plain => return text.to_string(),
        Style::Bold => "1",
        Style::Dim => "2",
        Style::Green => "32",
        Style::Yellow => "33",
        Style::Blue => "34",
        Style::Magenta => "35",
        Style::Cyan => "36",
        Style::Red => "1;31",
    };
    if *COLOR && !text.is_empty() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

const NAME_WIDTH: usize = 46;

fn state_cell(state: &str) -> Cell {
    let (mark, style) = match state {
        "active" => ("●", Style::Green),
        "review" => ("◐", Style::Yellow),
        "finished" => ("◆", Style::Magenta),
        "done" => ("✓", Style::Dim),
        "no ticket" => ("◌", Style::Dim),
        _ => ("○", Style::Dim),
    };
    Cell::styled(format!("{mark} {state}"), style)
}

/// A ticket's Jira status, colored by its category.
fn status_cell(ticket: &Ticket, mark: &str) -> Cell {
    let style = match ticket.category {
        Category::Done => Style::Green,
        Category::InProgress => Style::Yellow,
        Category::New | Category::Unknown => Style::Dim,
    };
    Cell::styled(format!("{mark}{}", ticket.status), style)
}

/// The view as compact rows: a root (task, loose ticket, worktree without a
/// ticket) with its state, then its worktrees with their ticket keys, MR and
/// agents, then the MRs no worktree holds; section titles between. Ticket rows
/// show only with `--all`: otherwise their MRs move up, the key in the keys column.
fn print_status(table: &Table, tag: Option<&str>, all: bool) {
    let nodes = view::build(
        table,
        &view::Options {
            all,
            filter: "",
            tag,
        },
    );
    let mut rows: Vec<[Cell; 4]> = Vec::new();
    let mut finished_tasks = Vec::new();
    let mut loose_shown = 0;

    for node in &nodes {
        match &node.kind {
            Kind::Header => {
                rows.push(Default::default());
                rows.push([
                    Cell::styled(format!("── {}", node.title()), Style::Dim),
                    Cell::default(),
                    Cell::default(),
                    Cell::default(),
                ]);
            }
            Kind::Task(i) => {
                let row = &table.tasks[*i];
                if !row.task.done && row.current_state() == Some(TaskState::Finished) {
                    finished_tasks.push(row.task.name.clone());
                }
                let mut name = Cell::styled(truncate(&row.task.name, NAME_WIDTH), Style::Bold);
                let tags: String = row.task.tags.iter().map(|t| format!(" #{t}")).collect();
                let room = NAME_WIDTH.saturating_sub(name.text.chars().count());
                if !tags.is_empty() && room > 2 {
                    name.push(Cell::styled(truncate(&tags, room), Style::Dim));
                }
                rows.push([
                    name,
                    state_cell(row.state_label()),
                    Cell::default(),
                    Cell::default(),
                ]);
                rows.extend(children(table, node, None, all, ""));
            }
            Kind::Loose(i) => {
                loose_shown += 1;
                let key = &table.loose[*i].key;
                let ticket = table.tickets.get(key);
                let mut name = Cell::link(key, &table.ticket_url(key), Style::Cyan);
                let summary = ticket.map(|t| t.summary.as_str()).unwrap_or("");
                let room = NAME_WIDTH.saturating_sub(key.chars().count() + 1);
                name.push(Cell::plain(format!(" {}", truncate(summary, room))));
                // nothing done on it yet: its Jira status says more
                let state = match (table.loose_state(*i), ticket) {
                    (Some(state), _) => state_cell(state.as_str()),
                    (None, Some(ticket)) => status_cell(ticket, "○ "),
                    (None, None) => state_cell(""),
                };
                rows.push([name, state, Cell::default(), Cell::default()]);
                rows.extend(children(table, node, Some(key), all, ""));
            }
            Kind::Tree(t) => rows.push([
                tree_name(table.tree(*t), NAME_WIDTH),
                state_cell("no ticket"),
                mr_cell(table, node),
                agents_cell(table, node),
            ]),
            Kind::Mr(m) => rows.push([
                Cell::styled(
                    truncate(&table.mr(*m).short_title(), NAME_WIDTH),
                    Style::Dim,
                ),
                Cell::default(),
                mr_link(table, *m),
                Cell::default(),
            ]),
            Kind::Ticket(_) | Kind::Agent(..) => {}
        }
    }

    if rows.is_empty() {
        println!("{}", paint("no active work", Style::Dim));
    } else {
        let header = ["", "", "MR", "AGENT"].map(|h| Cell::styled(h, Style::Dim));
        print_rows(std::iter::once(&header).chain(&rows));
    }
    if all {
        return;
    }

    let finished_trees = table.trees.iter().filter(|t| t.finished).count();
    let hidden = table.loose.len() - loose_shown + table.other_mrs.len();
    let mut footer = Vec::new();
    if !finished_tasks.is_empty() {
        footer.push(format!(
            "{} {}",
            paint("finished, can be closed:", Style::Dim),
            finished_tasks.join(", ")
        ));
    }
    if finished_trees > 0 && tag.is_none() {
        footer.push(format!(
            "{} {}",
            paint(
                &format!("{finished_trees} worktree(s) with merged MRs →"),
                Style::Dim
            ),
            paint("wt done --merged", Style::Bold)
        ));
    }
    if hidden > 0 && tag.is_none() {
        footer.push(format!(
            "{} {}",
            paint(
                &format!("{hidden} ticket(s) and MR(s) without activity →"),
                Style::Dim
            ),
            paint("wt status --all", Style::Bold)
        ));
    }
    if !footer.is_empty() {
        println!("\n{}", footer.join("\n"));
    }
}

fn print_rows<'a, const N: usize>(rows: impl Iterator<Item = &'a [Cell; N]> + Clone) {
    let mut widths = [0usize; N];
    for row in rows.clone() {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.text.chars().count());
        }
    }
    for row in rows {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            line.push_str(&cell.rendered);
            if i + 1 < row.len() {
                let pad = widths[i] - cell.text.chars().count();
                line.push_str(&" ".repeat(pad + 2));
            }
        }
        println!("{}", line.trim_end());
    }
}

/// `!iid`, linked and colored by the MR's state.
fn mr_link(table: &Table, m: usize) -> Cell {
    let mr = &table.mr(m).mr;
    let style = match mr.state() {
        MrState::Opened => Style::Blue,
        MrState::Merged => Style::Magenta,
        MrState::Closed | MrState::Locked | MrState::Other => Style::Dim,
    };
    Cell::link(format!("!{}", mr.iid()), mr.web_url(), style)
}

/// The MR the view hangs under a worktree.
fn mr_cell(table: &Table, tree: &Node) -> Cell {
    let links = tree
        .children
        .iter()
        .filter_map(|n| match n.kind {
            Kind::Mr(m) => Some(mr_link(table, m)),
            _ => None,
        })
        .collect();
    Cell::join(links, " ")
}

/// Lines under a root, each prefixed by `indent`: its worktrees, then its
/// tickets' and its own MRs. Keys are listed but the root's own `key`.
fn children(
    table: &Table,
    node: &Node,
    key: Option<&str>,
    all: bool,
    indent: &str,
) -> Vec<[Cell; 4]> {
    let mut items: Vec<&Node> = node
        .children
        .iter()
        .filter(|n| matches!(n.kind, Kind::Tree(_)))
        .collect();
    let mut mrs: Vec<usize> = Vec::new();
    for child in &node.children {
        match child.kind {
            Kind::Ticket(_) if all => items.push(child),
            // an MR with the keys of two tickets is under both
            Kind::Ticket(_) => {
                for mr in &child.children {
                    if let Kind::Mr(m) = mr.kind
                        && !mrs.contains(&m)
                    {
                        mrs.push(m);
                        items.push(mr);
                    }
                }
            }
            Kind::Mr(m) if !mrs.contains(&m) => {
                mrs.push(m);
                items.push(child);
            }
            _ => {}
        }
    }

    let room = NAME_WIDTH.saturating_sub(indent.chars().count() + 3);
    let keys_cell = |keys: &[String]| {
        let links = keys
            .iter()
            .filter(|k| Some(k.as_str()) != key)
            .map(|k| Cell::link(k, &table.ticket_url(k), Style::Cyan))
            .collect();
        Cell::join(links, " ")
    };
    let mut lines = Vec::new();
    for (n, item) in items.iter().enumerate() {
        let last = n + 1 == items.len();
        let mut name = Cell::styled(
            format!("{indent}{}", if last { "└─ " } else { "├─ " }),
            Style::Dim,
        );
        match &item.kind {
            Kind::Tree(t) => {
                let tree = table.tree(*t);
                name.push(tree_name(tree, room));
                lines.push([
                    name,
                    keys_cell(&tree.keys),
                    mr_cell(table, item),
                    agents_cell(table, item),
                ]);
            }
            Kind::Mr(m) => {
                name.push(Cell::styled(
                    truncate(&table.mr(*m).short_title(), room),
                    Style::Dim,
                ));
                lines.push([
                    name,
                    keys_cell(&table.mr(*m).keys),
                    mr_link(table, *m),
                    Cell::default(),
                ]);
            }
            Kind::Ticket(k) => {
                let ticket = table.tickets.get(k);
                name.push(Cell::link(k, &table.ticket_url(k), Style::Cyan));
                let summary = ticket.map(|t| t.summary.as_str()).unwrap_or("");
                let room = room.saturating_sub(k.chars().count() + 1);
                name.push(Cell::plain(format!(" {}", truncate(summary, room))));
                let status = ticket.map(|t| status_cell(t, "")).unwrap_or_default();
                lines.push([name, status, Cell::default(), Cell::default()]);
                let deeper = format!("{indent}{}", if last { "   " } else { "│  " });
                lines.extend(children(table, item, Some(k), all, &deeper));
            }
            _ => {}
        }
    }
    lines
}

/// The worktree's name with `+N` changed files and `↑N` unpushed commits.
fn tree_name(tree: &Tree, width: usize) -> Cell {
    let mut marks = Cell::default();
    if let Some(c) = tree.changes.filter(|&c| c > 0) {
        marks.push(Cell::styled(format!(" +{c}"), Style::Yellow));
    }
    if let Some(u) = tree.unpushed.filter(|&u| u > 0) {
        marks.push(Cell::styled(format!(" ↑{u}"), Style::Magenta));
    }
    if tree.finished {
        marks.push(Cell::styled(" ✓ merged", Style::Dim));
    }
    let room = width.saturating_sub(marks.text.chars().count());
    let mut cell = Cell::plain(truncate(&tree.wt.name(), room));
    cell.push(marks);
    cell
}

/// The worktree's agents as `name status`, the status colored by what it needs
/// from me.
fn agents_cell(table: &Table, tree: &Node) -> Cell {
    let cells = tree
        .children
        .iter()
        .filter_map(|n| match n.kind {
            Kind::Agent(t, a) => Some(&table.tree(t).agents[a]),
            _ => None,
        })
        .map(|agent| {
            let style = match agent.status {
                AgentStatus::Working => Style::Green,
                AgentStatus::Blocked => Style::Red,
                AgentStatus::Idle | AgentStatus::Done => Style::Blue,
                AgentStatus::Unknown => Style::Dim,
            };
            let mut cell = Cell::plain(&agent.name);
            cell.push(Cell::styled(format!(" {}", agent.status.as_str()), style));
            cell
        })
        .collect();
    Cell::join(cells, ", ")
}

fn print_notes(table: &Table, updated: &[String]) {
    let mut out = Vec::new();
    for (key, tasks) in &table.conflicts {
        out.push(format!("! {key} is in several tasks: {}", tasks.join(", ")));
    }
    for warning in &table.warnings {
        out.push(format!("! {warning}"));
    }
    if table
        .tasks
        .iter()
        .any(|r| !r.task.done && r.state.is_none())
    {
        out.push("! task states not recomputed: Jira or GitLab did not answer".to_string());
    }
    if !updated.is_empty() {
        out.push(format!(
            "updated {} note(s): {}",
            updated.len(),
            updated.join(", ")
        ));
    }
    if !out.is_empty() {
        println!("\n{}", out.join("\n"));
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let cut: String = text.chars().take(width - 1).collect();
    format!("{cut}…")
}

/// Adds `tickets:` to notes from before worktable: from the `jira` link, otherwise
/// from a key at the start of the file name. Dry run unless `apply`.
pub(super) fn cli_migrate(config: &Config, apply: bool) -> Result<(), String> {
    let tasks = load(config)?;
    let _lock = if apply {
        Some(table::lock().map_err(|err| format!("can't lock: {err}"))?)
    } else {
        None
    };

    let mut changed = 0;
    let mut without_ticket = Vec::new();
    for mut task in tasks {
        if task.note.has("tickets") {
            continue;
        }
        let keys = task.legacy_tickets();
        if keys.is_empty() {
            without_ticket.push(task.name.clone());
            continue;
        }
        println!("{}: + tickets: [{}]", task.name, keys.join(", "));
        changed += 1;
        if apply {
            let value = Value::Sequence(keys.into_iter().map(Value::String).collect());
            task.note.set("tickets", value);
            task.note.save().map_err(|err| err.to_string())?;
        }
    }

    if !without_ticket.is_empty() {
        println!("\nno ticket found, left as tasks without tickets:");
        for name in &without_ticket {
            println!("  {name}");
        }
    }
    let verb = if apply { "updated" } else { "would update" };
    println!(
        "\n{verb} {changed} note(s){}",
        if apply { "" } else { "; run with --apply" }
    );
    Ok(())
}
