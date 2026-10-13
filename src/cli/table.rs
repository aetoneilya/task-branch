use super::hyperlink;
use crate::config::{Config, VaultConfig};
use crate::table::{self, Table, Tree};
use crate::vault::task::{Task, load_tasks};
use serde_yaml::Value;
use std::io::IsTerminal;
use std::sync::LazyLock;

const SUMMARY_WIDTH: usize = 70;

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
    let mut table = table::build(config, load(config)?, true);
    let updated = if no_write {
        Vec::new()
    } else {
        table::write_back(&mut table)
    };
    if all {
        print_full(&table, tag);
    } else {
        print_active(&table, tag);
    }
    print_notes(&table, &updated);
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
            rendered: if *COLOR { hyperlink(url, &painted) } else { painted },
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

static LEADING_KEY: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[A-Z][A-Z0-9]+-\d+:?\s*").unwrap());

fn state_cell(state: &str) -> Cell {
    let (mark, style) = match state {
        "active" => ("●", Style::Green),
        "review" => ("◐", Style::Yellow),
        "finished" => ("◆", Style::Magenta),
        "no ticket" => ("◌", Style::Dim),
        _ => ("○", Style::Dim),
    };
    Cell::styled(format!("{mark} {state}"), style)
}

/// Active work as a tree: open tasks, then tickets without a task that have a
/// worktree or an open MR, then unfinished worktrees without a ticket; under each
/// its worktrees and the open MRs no worktree holds.
fn print_active(table: &Table, tag: Option<String>) {
    let mut rows: Vec<[Cell; 4]> = Vec::new();
    let mut finished_tasks = Vec::new();

    for row in table.tasks.iter().filter(|r| !r.task.done) {
        if tag.as_ref().is_some_and(|tag| !row.task.tags.contains(tag)) {
            continue;
        }
        let state = row
            .state
            .map(str::to_string)
            .or_else(|| row.task.state.clone())
            .unwrap_or_default();
        if state == "finished" {
            finished_tasks.push(row.task.name.clone());
            continue;
        }
        let mut name = Cell::styled(truncate(&row.task.name, NAME_WIDTH), Style::Bold);
        let tags: String = row.task.tags.iter().map(|t| format!(" #{t}")).collect();
        let room = NAME_WIDTH.saturating_sub(name.text.chars().count());
        if !tags.is_empty() && room > 2 {
            name.push(Cell::styled(truncate(&tags, room), Style::Dim));
        }
        rows.push([name, state_cell(&state), Cell::default(), Cell::default()]);
        rows.extend(children(table, &row.trees, &row.mrs, None));
    }

    let mut hidden = 0;
    if tag.is_none() {
        for loose in &table.loose {
            let ticket = table.tickets.get(&loose.key);
            let live_trees: Vec<usize> = loose
                .trees
                .iter()
                .copied()
                .filter(|&i| !table.tree(i).finished)
                .collect();
            let open: Vec<usize> = loose
                .mrs
                .iter()
                .copied()
                .filter(|&i| table.mr(i).opened())
                .collect();
            let closed = ticket.is_some_and(|t| t.closed());
            if live_trees.is_empty() && (open.is_empty() || closed) {
                hidden += 1;
                continue;
            }
            let in_review = !open.is_empty() && live_trees.iter().all(|&i| table.tree(i).clean());
            let mut name = Cell::link(&loose.key, &table.ticket_url(&loose.key), Style::Cyan);
            let summary = ticket.map(|t| t.summary.as_str()).unwrap_or("");
            let room = NAME_WIDTH.saturating_sub(loose.key.chars().count() + 1);
            name.push(Cell::plain(format!(" {}", truncate(summary, room))));
            rows.push([
                name,
                state_cell(if in_review { "review" } else { "active" }),
                Cell::default(),
                Cell::default(),
            ]);
            rows.extend(children(table, &live_trees, &open, Some(&loose.key)));
        }
        for &i in &table.other_trees {
            let tree = table.tree(i);
            if tree.finished {
                continue;
            }
            rows.push([
                tree_name(tree, NAME_WIDTH),
                state_cell("no ticket"),
                open_mrs(table, &tree.mr.into_iter().collect::<Vec<_>>()),
                agents_cell(table, &[i]),
            ]);
        }
        hidden += table.other_mrs.len();
    }

    if rows.is_empty() {
        println!("{}", paint("no active work", Style::Dim));
    } else {
        let header = ["", "", "MR", "AGENT"].map(|h| Cell::styled(h, Style::Dim));
        print_rows(std::iter::once(&header).chain(&rows));
    }

    let finished_trees = table.trees.iter().filter(|t| t.finished).count();
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
    if hidden > 0 {
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

fn open_mrs(table: &Table, mrs: &[usize]) -> Cell {
    let cells = mrs
        .iter()
        .map(|&i| &table.mr(i).mr)
        .filter(|mr| mr.state() == "opened")
        .map(|mr| Cell::link(format!("!{}", mr.iid()), mr.web_url(), Style::Blue))
        .collect();
    Cell::join(cells, " ")
}

/// Branch lines under a root: its unfinished worktrees, busiest first, each with
/// its ticket keys (but the root's own `key`), open MR and agents; then the open
/// MRs no worktree holds, the ticket key moved from the title to the keys column.
fn children(table: &Table, trees: &[usize], mrs: &[usize], key: Option<&str>) -> Vec<[Cell; 4]> {
    let mut live: Vec<usize> = trees
        .iter()
        .copied()
        .filter(|&i| !table.tree(i).finished)
        .collect();
    live.sort_by_key(|&i| {
        let t = table.tree(i);
        (
            std::cmp::Reverse(!t.agents.is_empty()),
            std::cmp::Reverse(t.changes.unwrap_or(0)),
            t.wt.name(),
        )
    });
    let held: Vec<usize> = live.iter().filter_map(|&i| table.tree(i).mr).collect();
    let loose_mrs: Vec<usize> = mrs
        .iter()
        .copied()
        .filter(|&i| table.mr(i).opened() && !held.contains(&i))
        .collect();

    let count = live.len() + loose_mrs.len();
    let branch = |n: usize| {
        let glyph = if n + 1 == count { "└─ " } else { "├─ " };
        Cell::styled(glyph, Style::Dim)
    };
    let room = NAME_WIDTH - 3;
    let keys_cell = |keys: &[String]| {
        let links = keys
            .iter()
            .filter(|k| Some(k.as_str()) != key)
            .map(|k| Cell::link(k, &table.ticket_url(k), Style::Cyan))
            .collect();
        Cell::join(links, " ")
    };
    let mut lines = Vec::new();
    for (n, &i) in live.iter().enumerate() {
        let tree = table.tree(i);
        let mut name = branch(n);
        name.push(tree_name(tree, room));
        lines.push([
            name,
            keys_cell(&tree.keys),
            open_mrs(table, &tree.mr.into_iter().collect::<Vec<_>>()),
            agents_cell(table, &[i]),
        ]);
    }
    for (n, &i) in loose_mrs.iter().enumerate() {
        let mut name = branch(live.len() + n);
        let title = table.mr(i).mr.title();
        let title = LEADING_KEY.replace(title, "");
        name.push(Cell::styled(truncate(&title, room), Style::Dim));
        lines.push([
            name,
            keys_cell(&table.mr(i).keys),
            open_mrs(table, &[i]),
            Cell::default(),
        ]);
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
    let room = width.saturating_sub(marks.text.chars().count());
    let mut cell = Cell::plain(truncate(&tree.wt.name(), room));
    cell.push(marks);
    cell
}

/// Agents as `name status`, the status colored by what it needs from me.
fn agents_cell(table: &Table, trees: &[usize]) -> Cell {
    let cells = trees
        .iter()
        .flat_map(|&i| &table.tree(i).agents)
        .map(|label| {
            let (name, status) = label.rsplit_once(':').unwrap_or((label, ""));
            let style = match status {
                "working" => Style::Green,
                "blocked" => Style::Red,
                "idle" | "done" => Style::Blue,
                _ => Style::Dim,
            };
            let mut cell = Cell::plain(name);
            cell.push(Cell::styled(format!(" {status}"), style));
            cell
        })
        .collect();
    Cell::join(cells, ", ")
}

/// Everything: tasks with all their tickets, MRs and worktrees, every ticket and
/// MR without a task.
fn print_full(table: &Table, tag: Option<String>) {
    let mut out = Vec::new();
    for row in &table.tasks {
        if let Some(tag) = &tag {
            if !row.task.tags.iter().any(|t| t == tag) {
                continue;
            }
        }
        print_task(&mut out, &table, row);
    }

    if tag.is_none() {
        if !table.loose.is_empty() {
            out.push("Tickets without a task".to_string());
            for loose in &table.loose {
                print_ticket(&mut out, &table, &loose.key, &loose.mrs, "  ");
                for &i in &loose.trees {
                    out.push(format!("    {}", tree_line(table.tree(i))));
                }
            }
            out.push(String::new());
        }
        if !table.other_trees.is_empty() || !table.other_mrs.is_empty() {
            out.push("Without a ticket".to_string());
            for &i in &table.other_trees {
                out.push(format!("  {}", tree_line(table.tree(i))));
            }
            for &i in &table.other_mrs {
                let mr = &table.mr(i).mr;
                out.push(format!("  {}  {}", mr_link(&table, i), mr.title()));
            }
            out.push(String::new());
        }
    }

    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    println!("{}", out.join("\n"));
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

fn print_task(out: &mut Vec<String>, table: &Table, row: &table::TaskRow) {
    let task = &row.task;
    let state = if task.done {
        "done".to_string()
    } else {
        row.state
            .map(str::to_string)
            .or_else(|| task.state.clone())
            .unwrap_or_default()
    };
    let marker = match state.as_str() {
        "done" => "✓",
        "finished" => "◆",
        "review" => "◐",
        "active" => "●",
        _ => "○",
    };
    let tags: String = task.tags.iter().map(|t| format!("  #{t}")).collect();
    out.push(format!("{marker} {}  {state}{tags}", task.name));

    let mut shown_mrs: Vec<usize> = Vec::new();
    for key in &task.tickets {
        let mrs: Vec<usize> = row
            .mrs
            .iter()
            .copied()
            .filter(|&i| table.mr(i).keys.contains(key))
            .collect();
        print_ticket(out, table, key, &mrs, "  ");
        shown_mrs.extend(mrs);
    }
    let keyless: Vec<usize> = row
        .mrs
        .iter()
        .copied()
        .filter(|i| !shown_mrs.contains(i))
        .collect();
    for i in keyless {
        out.push(format!(
            "  {}  {}",
            mr_link(table, i),
            table.mr(i).mr.title()
        ));
    }
    for &i in &row.trees {
        out.push(format!("  {}", tree_line(table.tree(i))));
    }

    let sessions: Vec<_> = task.sessions.iter().chain(&row.new_sessions).collect();
    if let Some(last) = sessions.iter().map(|s| s.date.as_str()).max() {
        out.push(format!("  ↺ {} session(s), last {last}", sessions.len()));
    }
    out.push(String::new());
}

fn print_ticket(out: &mut Vec<String>, table: &Table, key: &str, mrs: &[usize], indent: &str) {
    let line = match table.tickets.get(key) {
        Some(t) => format!(
            "{indent}{}  {}  {}",
            hyperlink(&t.url, key),
            t.status,
            truncate(&t.summary, SUMMARY_WIDTH)
        ),
        None => format!("{indent}{key}"),
    };
    out.push(line);
    if !mrs.is_empty() {
        let links: Vec<String> = mrs.iter().map(|&i| mr_link(table, i)).collect();
        out.push(format!("{indent}    {}", links.join("  ")));
    }
}

fn mr_link(table: &Table, i: usize) -> String {
    let mr = &table.mr(i).mr;
    hyperlink(mr.web_url(), &format!("!{} {}", mr.iid(), mr.state()))
}

fn tree_line(tree: &Tree) -> String {
    let local = match (tree.changes, tree.unpushed) {
        (Some(0), Some(0) | None) => "clean".to_string(),
        (Some(c), Some(0) | None) => format!("{c} changed"),
        (Some(0), Some(u)) => format!("{u} unpushed"),
        (Some(c), Some(u)) => format!("{c} changed, {u} unpushed"),
        (None, _) => "git status failed".to_string(),
    };
    let mut line = format!(
        "⌂ {}  {}  {local}",
        tree.wt.name(),
        tree.wt.branch.as_deref().unwrap_or("(detached)")
    );
    if !tree.agents.is_empty() {
        line.push_str(&format!("  ◉ {}", tree.agents.join(", ")));
    }
    if tree.finished {
        line.push_str("  → wt done");
    }
    line
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
