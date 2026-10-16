//! What is shown and how it nests, for the TUI and `wt status` alike: tasks →
//! their tickets → MRs no shown worktree holds, then the task's worktrees → the
//! MR of their branch, agents. After the tasks, a `no task` section (tickets I
//! work on that belong to no task, worktrees without a ticket) and an `assigned
//! to me in Jira` one (my open tickets nothing is done on yet).

use super::{Category, Table};
use serde::{Deserialize, Serialize};

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
    Header(Section),
}

impl Kind {
    pub fn is_header(&self) -> bool {
        matches!(self, Kind::Header(_))
    }
}

/// The sections after the tasks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    /// Tickets I work on that belong to no task, worktrees without a ticket.
    NoTask,
    /// My open tickets nothing is done on yet.
    Assigned,
}

impl Section {
    pub fn title(self) -> &'static str {
        match self {
            Section::NoTask => "no task",
            Section::Assigned => "assigned to me in Jira",
        }
    }
}

#[derive(Debug)]
pub struct Node {
    /// Stable across rebuilds: keeps the TUI's selection and folding.
    pub id: String,
    pub kind: Kind,
    pub children: Vec<Node>,
}

impl Node {
    fn leaf(id: String, kind: Kind) -> Node {
        Node {
            id,
            kind,
            children: Vec::new(),
        }
    }
}

#[derive(Default)]
pub struct Options<'a> {
    /// Adds done tasks, tickets and worktrees without activity, merged MRs.
    pub all: bool,
    /// Keeps the roots whose names, tickets, summaries or branches contain it.
    pub filter: &'a str,
    /// Only tasks with this tag, and none of the sections.
    pub tag: Option<&'a str>,
    pub hide: Hide,
}

/// Kinds of rows left out of the view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hide {
    /// Tasks and everything under them.
    pub tasks: bool,
    /// Ticket rows under tasks: their MRs move up to the task, once each.
    pub tickets: bool,
    pub mrs: bool,
    /// Worktrees, and their agents with them; an MR a worktree held goes under
    /// its ticket.
    pub trees: bool,
    pub agents: bool,
    pub no_task: bool,
    pub assigned: bool,
}

impl Hide {
    pub fn any(&self) -> bool {
        *self != Hide::default()
    }
}

pub fn build(table: &Table, options: &Options) -> Vec<Node> {
    let all = options.all;
    let hide = options.hide;
    let filter = options.filter.to_lowercase();
    let keep = |text: String| filter.is_empty() || text.to_lowercase().contains(&filter);
    let mut roots = Vec::new();

    // the table keeps tasks by state, newest first; done ones come last
    for (i, row) in table.tasks.iter().enumerate() {
        if hide.tasks || (row.task.done && !all) {
            continue;
        }
        if options
            .tag
            .is_some_and(|tag| !row.task.tags.iter().any(|t| t == tag))
        {
            continue;
        }
        let summaries: Vec<&str> = row
            .tickets()
            .filter_map(|k| table.tickets.get(k))
            .map(|t| t.summary.as_str())
            .collect();
        let keys: Vec<&str> = row.tickets().map(String::as_str).collect();
        let text = format!(
            "{} #{} {} {} {}",
            row.task.name,
            row.task.tags.join(" #"),
            keys.join(" "),
            summaries.join(" "),
            branches(table, &row.trees)
        );
        if !keep(text) {
            continue;
        }
        // an MR from a shown worktree's branch hangs under that worktree
        let (trees, held) = tree_nodes(table, &row.trees, all, hide);
        let free_mr = |m: usize| !hide.mrs && !held.contains(&m) && (all || table.mr(m).opened());
        let mut children = Vec::new();
        let mut lifted = Vec::new();
        for key in row.tickets() {
            let mrs = row
                .mrs
                .iter()
                .copied()
                .filter(|&m| table.mr(m).keys.contains(key) && free_mr(m));
            if hide.tickets {
                // an MR with the keys of two tickets is under both
                for m in mrs {
                    if !lifted.contains(&m) {
                        lifted.push(m);
                        children.push(mr_node(m));
                    }
                }
                continue;
            }
            children.push(Node {
                id: format!("ticket:{}:{key}", row.task.name),
                kind: Kind::Ticket(key.clone()),
                children: mrs.map(mr_node).collect(),
            });
        }
        children.extend(
            row.mrs
                .iter()
                .copied()
                .filter(|&m| {
                    !table
                        .mr(m)
                        .keys
                        .iter()
                        .any(|k| row.tickets().any(|t| t == k))
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
    if options.tag.is_some() {
        return roots;
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
        let (trees, held) = tree_nodes(table, &loose.trees, all, hide);
        let mut children: Vec<Node> = loose
            .mrs
            .iter()
            .copied()
            .filter(|&m| !hide.mrs && !held.contains(&m) && (all || table.mr(m).opened()))
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

    for node in tree_nodes(table, &table.other_trees, all, hide).0 {
        if let Kind::Tree(t) = node.kind
            && keep(format!(
                "{} {}",
                table.tree(t).wt.name(),
                branches(table, &[t])
            ))
        {
            no_task.push(node);
        }
    }
    if all && !hide.mrs {
        for &m in &table.other_mrs {
            if keep(table.mr(m).mr.title().to_string()) {
                no_task.push(mr_node(m));
            }
        }
    }
    // in progress before new, then newest first (the table's order)
    assigned.sort_by_key(|n| match &n.kind {
        Kind::Loose(i) => match table.tickets.get(&table.loose[*i].key) {
            Some(t) if t.category == Category::InProgress => 0,
            _ => 1,
        },
        _ => 2,
    });
    for (section, nodes, hidden) in [
        (Section::NoTask, no_task, hide.no_task),
        (Section::Assigned, assigned, hide.assigned),
    ] {
        if !hidden && !nodes.is_empty() {
            roots.push(Node::leaf(
                format!("header:{}", section.title()),
                Kind::Header(section),
            ));
            roots.extend(nodes);
        }
    }
    roots
}

fn branches(table: &Table, trees: &[usize]) -> String {
    trees
        .iter()
        .filter_map(|&t| table.tree(t).wt.branch.clone())
        .collect::<Vec<_>>()
        .join(" ")
}

fn mr_node(m: usize) -> Node {
    Node::leaf(format!("mr:{m}"), Kind::Mr(m))
}

/// Worktrees with agents first, then by changed files, then by name; finished
/// ones only with `all`. Under each, the MR of its branch and its agents. Also
/// returns the MRs placed so, which go nowhere else.
fn tree_nodes(table: &Table, trees: &[usize], all: bool, hide: Hide) -> (Vec<Node>, Vec<usize>) {
    if hide.trees {
        return (Vec::new(), Vec::new());
    }
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
            let mr = tree
                .mr
                .filter(|&m| !hide.mrs && (all || table.mr(m).opened()))
                .map(mr_node);
            let agents = (0..tree.agents.len()).filter(|_| !hide.agents).map(|a| {
                Node::leaf(
                    format!("agent:{}:{a}", tree.wt.path.display()),
                    Kind::Agent(t, a),
                )
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

#[cfg(test)]
mod tests {
    use super::super::fixtures::*;
    use super::super::matching::assemble;
    use super::*;

    /// Each node as `<indent><id>`, the tree drawn by indentation.
    fn outline(nodes: &[Node]) -> Vec<String> {
        fn walk(nodes: &[Node], depth: usize, out: &mut Vec<String>) {
            for node in nodes {
                out.push(format!(
                    "{}{}",
                    "  ".repeat(depth),
                    node.id.replace(ROOT, "")
                ));
                walk(&node.children, depth + 1, out);
            }
        }
        let mut out = Vec::new();
        walk(nodes, 0, &mut out);
        out
    }

    #[test]
    fn mrs_nest_under_their_worktree_else_under_their_ticket() {
        let table = table(
            vec![task("a2i", &["UCP-1", "UCP-2"])],
            vec![],
            vec![
                mr(10, "i.udalov/UCP-1-a", "a", "opened"),
                mr(11, "i.udalov/UCP-1-b", "b", "opened"),
                mr(12, "i.udalov/UCP-1-c", "merged one", "merged"),
            ],
            vec![tree("a", "i.udalov/UCP-1-a")],
        );
        let (m10, m11, m12) = (table.mr_index(10), table.mr_index(11), table.mr_index(12));
        assert_eq!(
            outline(&build(&table, &Options::default())),
            vec![
                "task:a2i".to_string(),
                "  ticket:a2i:UCP-1".to_string(),
                format!("    mr:{m11}"),
                "  ticket:a2i:UCP-2".to_string(),
                "  tree:/a".to_string(),
                format!("    mr:{m10}"),
            ]
        );
        let all = build(
            &table,
            &Options {
                all: true,
                ..Default::default()
            },
        );
        assert!(outline(&all).contains(&format!("    mr:{m12}")));
    }

    #[test]
    fn hidden_tickets_leave_their_mrs_to_the_task_once() {
        let table = table(
            vec![task("a2i", &["UCP-1", "UCP-2"])],
            vec![],
            vec![mr(10, "i.udalov/UCP-1-UCP-2", "both", "opened")],
            vec![tree("a", "i.udalov/UCP-1-a")],
        );
        let m10 = table.mr_index(10);
        let options = Options {
            hide: Hide {
                tickets: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            outline(&build(&table, &options)),
            vec![
                "task:a2i".to_string(),
                format!("  mr:{m10}"),
                "  tree:/a".to_string(),
            ]
        );
    }

    #[test]
    fn hidden_kinds_leave_the_view() {
        let mut inputs = inputs(
            vec![task("a2i", &["UCP-1"])],
            vec![ticket("UCP-5", Category::New, true)],
            vec![
                mr(10, "i.udalov/UCP-1-a", "a", "opened"),
                mr(11, "i.udalov/UCP-1-b", "b", "opened"),
            ],
            vec![tree("a", "i.udalov/UCP-1-a"), tree("misc", "i.udalov/misc")],
        );
        inputs.agents = vec![agent("x", "a", "w")];
        let table = assemble(inputs);
        let (m10, m11) = (table.mr_index(10), table.mr_index(11));
        let shown = |hide: Hide| {
            outline(&build(
                &table,
                &Options {
                    hide,
                    ..Default::default()
                },
            ))
        };
        assert_eq!(
            shown(Hide {
                mrs: true,
                agents: true,
                assigned: true,
                ..Default::default()
            }),
            vec![
                "task:a2i",
                "  ticket:a2i:UCP-1",
                "  tree:/a",
                "header:no task",
                "tree:/misc"
            ]
        );
        // without worktrees, the MR one held goes under its ticket
        assert_eq!(
            shown(Hide {
                trees: true,
                no_task: true,
                assigned: true,
                ..Default::default()
            }),
            vec![
                "task:a2i".to_string(),
                "  ticket:a2i:UCP-1".to_string(),
                format!("    mr:{m10}"),
                format!("    mr:{m11}"),
            ]
        );
        assert_eq!(
            shown(Hide {
                tasks: true,
                ..Default::default()
            }),
            vec![
                "header:no task",
                "tree:/misc",
                "header:assigned to me in Jira",
                "loose:UCP-5"
            ]
        );
    }

    #[test]
    fn worktrees_go_agents_first_then_by_changes_then_by_name() {
        let mut busy = tree("b-busy", "UCP-1-b");
        busy.changes = Some(5);
        let mut inputs = inputs(
            vec![task("t", &["UCP-1"])],
            vec![],
            vec![],
            vec![
                tree("c", "UCP-1-c"),
                busy,
                tree("a", "UCP-1-a"),
                tree("z-agent", "UCP-1-z"),
            ],
        );
        inputs.agents = vec![agent("x", "z-agent", "w")];
        let table = assemble(inputs);
        let trees: Vec<String> = build(&table, &Options::default())[0]
            .children
            .iter()
            .filter(|n| matches!(n.kind, Kind::Tree(_)))
            .map(|n| n.id.replace(ROOT, ""))
            .collect();
        assert_eq!(
            trees,
            vec!["tree:/z-agent", "tree:/b-busy", "tree:/a", "tree:/c"]
        );
    }

    #[test]
    fn sections_after_the_tasks() {
        let table = table(
            vec![
                task("t", &["UCP-1"]),
                task_note("closed", "done: true", "2026-10-01"),
            ],
            vec![
                ticket("UCP-5", Category::New, true),
                ticket("UCP-6", Category::InProgress, true),
                ticket("UCP-7", Category::Done, false),
                ticket("UCP-8", Category::New, false),
            ],
            vec![
                mr(20, "UCP-7-x", "closed ticket, open MR", "opened"),
                mr(21, "UCP-8-x", "y", "opened"),
            ],
            vec![tree("misc", "i.udalov/misc")],
        );
        let roots: Vec<String> = build(&table, &Options::default())
            .iter()
            .map(|n| n.id.replace(ROOT, ""))
            .collect();
        assert_eq!(
            roots,
            vec![
                "task:t",
                "header:no task",
                "loose:UCP-8",
                "tree:/misc",
                "header:assigned to me in Jira",
                "loose:UCP-6",
                "loose:UCP-5",
            ]
        );

        let roots: Vec<String> = build(
            &table,
            &Options {
                all: true,
                ..Default::default()
            },
        )
        .iter()
        .map(|n| n.id.clone())
        .collect();
        assert!(roots.contains(&"task:closed".to_string()));
        assert!(roots.contains(&"loose:UCP-7".to_string()));

        let tagged = build(
            &table,
            &Options {
                tag: Some("x"),
                ..Default::default()
            },
        );
        assert!(tagged.is_empty());
        let filtered = build(
            &table,
            &Options {
                filter: "misc",
                ..Default::default()
            },
        );
        let roots: Vec<String> = filtered.iter().map(|n| n.id.replace(ROOT, "")).collect();
        assert_eq!(roots, vec!["header:no task", "tree:/misc"]);
    }
}
