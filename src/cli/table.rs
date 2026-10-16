use super::{StatusArgs, colors, load};
use crate::config::Config;
use crate::table::look::{self, Piece, Tone};
use crate::table::view::{self, Kind, Node};
use crate::table::{self, RowState, Table, Tree};
use crate::trace;
use crate::vault::task::TaskState;
use std::sync::LazyLock;

pub(super) fn cli_status(config: &Config, args: StatusArgs) -> Result<(), String> {
    let tasks = trace::time("tasks", || load(config))?;
    let mut table = trace::time("build", || table::build(config, tasks, true));
    let updated = if args.no_write {
        Vec::new()
    } else {
        trace::time("write back", || table::write_back(&mut table))
    };
    trace::time("render", || {
        print_status(&table, args.tag.as_deref(), args.all);
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
        Cell::styled(text, Tone::Plain)
    }

    fn styled(text: impl Into<String>, tone: Tone) -> Cell {
        let text = text.into();
        Cell {
            rendered: paint(&text, tone),
            text,
        }
    }

    fn pieces(pieces: impl IntoIterator<Item = Piece>) -> Cell {
        let mut cell = Cell::default();
        for piece in pieces {
            cell.push(piece.into());
        }
        cell
    }

    /// A terminal hyperlink, on a terminal only: piped output stays plain text.
    fn link(piece: Piece, url: &str) -> Cell {
        let painted = paint(&piece.text, piece.tone);
        Cell {
            rendered: if *COLOR {
                hyperlink(url, &painted)
            } else {
                painted
            },
            text: piece.text,
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

impl From<Piece> for Cell {
    fn from(piece: Piece) -> Cell {
        Cell::styled(piece.text, piece.tone)
    }
}

/// An OSC 8 terminal hyperlink.
fn hyperlink(url: &str, text: &str) -> String {
    format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", url, text)
}

static COLOR: LazyLock<bool> = LazyLock::new(|| colors(&std::io::stdout()));

/// `text` in the tone's ANSI color, when stdout takes colors.
fn paint(text: &str, tone: Tone) -> String {
    let code = match tone {
        Tone::Plain => return text.to_string(),
        Tone::Bold => "1",
        Tone::Dim => "2",
        Tone::Green => "32",
        Tone::Yellow => "33",
        Tone::Blue => "34",
        Tone::Magenta => "35",
        Tone::Cyan => "36",
        Tone::Red => "1;31",
    };
    if *COLOR && !text.is_empty() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

const NAME_WIDTH: usize = 46;

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
            hide: view::Hide {
                tickets: !all,
                ..Default::default()
            },
        },
    );
    let mut rows: Vec<[Cell; 4]> = Vec::new();
    let mut finished_tasks = Vec::new();
    let mut loose_shown = 0;

    for node in &nodes {
        match &node.kind {
            Kind::Header(section) => {
                rows.push(Default::default());
                rows.push([
                    Cell::styled(format!("── {}", section.title()), Tone::Dim),
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
                let mut name = Cell::styled(truncate(&row.task.name, NAME_WIDTH), Tone::Bold);
                let tags: String = row.task.tags.iter().map(|t| format!(" #{t}")).collect();
                let room = NAME_WIDTH.saturating_sub(name.text.chars().count());
                if !tags.is_empty() && room > 2 {
                    name.push(Cell::styled(truncate(&tags, room), Tone::Dim));
                }
                rows.push([
                    name,
                    look::state(row.row_state(), true).into(),
                    Cell::default(),
                    agents_cell(table, node),
                ]);
                rows.extend(children(table, node, None, ""));
            }
            Kind::Loose(i) => {
                loose_shown += 1;
                let loose = &table.loose[*i];
                let key = &loose.key;
                let ticket = table.tickets.get(key);
                let mut name = Cell::link(look::key(key), &table.ticket_url(key));
                let room = NAME_WIDTH.saturating_sub(key.chars().count() + 1);
                name.push(Cell::plain(format!(
                    " {}",
                    truncate(&look::summary(ticket), room)
                )));
                // nothing done on it yet: its Jira status says more
                let state = match (loose.state, ticket) {
                    (None, Some(ticket)) => look::status(ticket).after("○ "),
                    (state, _) => look::state(state.map(RowState::Task), true),
                };
                rows.push([name, state.into(), Cell::default(), Cell::default()]);
                rows.extend(children(table, node, Some(key), ""));
            }
            Kind::Tree(t) => rows.push([
                tree_name(table.tree(*t), NAME_WIDTH),
                look::no_ticket().into(),
                mr_cell(table, node),
                agents_cell(table, node),
            ]),
            Kind::Mr(m) => rows.push([
                Cell::styled(
                    truncate(&look::mr_title(table.mr(*m)), NAME_WIDTH),
                    Tone::Dim,
                ),
                Cell::default(),
                mr_link(table, *m),
                Cell::default(),
            ]),
            Kind::Ticket(_) | Kind::Agent(..) => {}
        }
    }

    if rows.is_empty() {
        println!("{}", paint("no active work", Tone::Dim));
    } else {
        let header = ["", "", "MR", "AGENT"].map(|h| Cell::styled(h, Tone::Dim));
        print_rows(std::iter::once(&header).chain(&rows));
    }
    if all {
        return;
    }

    let finished_trees = table.trees.iter().filter(|t| t.finished).count();
    let hidden = table.loose.len().saturating_sub(loose_shown) + table.other_mrs.len();
    let mut footer = Vec::new();
    if !finished_tasks.is_empty() {
        footer.push(format!(
            "{} {}",
            paint("finished, can be closed:", Tone::Dim),
            finished_tasks.join(", ")
        ));
    }
    if finished_trees > 0 && tag.is_none() {
        footer.push(format!(
            "{} {}",
            paint(
                &format!("{finished_trees} worktree(s) with merged MRs →"),
                Tone::Dim
            ),
            paint("wt done --merged", Tone::Bold)
        ));
    }
    if hidden > 0 && tag.is_none() {
        footer.push(format!(
            "{} {}",
            paint(
                &format!("{hidden} ticket(s) and MR(s) without activity →"),
                Tone::Dim
            ),
            paint("wt status --all", Tone::Bold)
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

/// `!iid`, linked and colored by the MR's state, then what an open one waits on.
fn mr_link(table: &Table, m: usize) -> Cell {
    let mr = table.mr(m);
    let mut cell = Cell::link(look::mr_iid(mr, true), mr.mr.web_url());
    cell.push(Cell::pieces(look::review(mr)));
    cell
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

/// Lines under a root or a ticket, each prefixed by `indent`: its worktrees,
/// then its tickets (with `--all`) and MRs. Keys are listed but the root's own `key`.
fn children(table: &Table, node: &Node, key: Option<&str>, indent: &str) -> Vec<[Cell; 4]> {
    let (trees, rest): (Vec<&Node>, Vec<&Node>) = node
        .children
        .iter()
        .partition(|n| matches!(n.kind, Kind::Tree(_)));
    let items: Vec<&Node> = trees.into_iter().chain(rest).collect();

    let room = NAME_WIDTH.saturating_sub(indent.chars().count() + 3);
    let keys_cell = |keys: &[String]| {
        let links = keys
            .iter()
            .filter(|k| Some(k.as_str()) != key)
            .map(|k| Cell::link(look::key(k), &table.ticket_url(k)))
            .collect();
        Cell::join(links, " ")
    };
    let mut lines = Vec::new();
    for (n, item) in items.iter().enumerate() {
        let last = n + 1 == items.len();
        let mut name = Cell::styled(
            format!("{indent}{}", if last { "└─ " } else { "├─ " }),
            Tone::Dim,
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
                    truncate(&look::mr_title(table.mr(*m)), room),
                    Tone::Dim,
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
                name.push(Cell::link(look::key(k), &table.ticket_url(k)));
                let room = room.saturating_sub(k.chars().count() + 1);
                name.push(Cell::plain(format!(
                    " {}",
                    truncate(&look::summary(ticket), room)
                )));
                let status = ticket.map(|t| look::status(t).into()).unwrap_or_default();
                lines.push([name, status, Cell::default(), Cell::default()]);
                let deeper = format!("{indent}{}", if last { "   " } else { "│  " });
                lines.extend(children(table, item, Some(k), &deeper));
            }
            _ => {}
        }
    }
    lines
}

/// The worktree's name with its marks: changed files, unpushed commits, merged
/// or quiet.
fn tree_name(tree: &Tree, width: usize) -> Cell {
    let marks = Cell::pieces(look::tree_marks(tree));
    let room = width.saturating_sub(marks.text.chars().count());
    let mut cell = Cell::plain(truncate(&tree.wt.name(), room));
    cell.push(marks);
    cell
}

/// The agents right under a worktree or a task as `name status`, the status
/// colored by what it needs from me.
fn agents_cell(table: &Table, node: &Node) -> Cell {
    let cells = node
        .children
        .iter()
        .filter_map(|n| match n.kind {
            Kind::Agent(at, a) => Some(table.agent(at, a)),
            _ => None,
        })
        .map(|agent| {
            Cell::pieces([
                Piece::plain(&agent.name),
                look::agent_status(agent.status).after(" "),
            ])
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
    let cut: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::truncate;

    #[test]
    fn truncates_to_any_width() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("abc", 0), "…");
    }
}
