//! How table items look, for the TUI and `wt status` alike: glyphs, labels and
//! colors as pieces of text with a tone, which each renderer maps to its own
//! styles. Where the pieces go stays with the renderers.

use super::{AgentStatus, Category, Mr, RowState, Table, Ticket, Tree};
use crate::client::gitlab::{MrReview, MrState};
use crate::vault::task::TaskState;

/// Days after which an open MR or an untouched worktree gets flagged.
const STALE_DAYS: i64 = 7;

/// A worktree row's icon in the TUI.
pub const TREE: &str = "⌂";
/// An agent row's icon in the TUI.
pub const AGENT: &str = "◉";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
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

/// Text in one tone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    pub text: String,
    pub tone: Tone,
}

impl Piece {
    pub fn new(text: impl Into<String>, tone: Tone) -> Piece {
        Piece {
            text: text.into(),
            tone,
        }
    }

    pub fn plain(text: impl Into<String>) -> Piece {
        Piece::new(text, Tone::Plain)
    }

    pub fn dim(text: impl Into<String>) -> Piece {
        Piece::new(text, Tone::Dim)
    }

    /// The same piece with `lead` in front, in its tone.
    pub fn after(mut self, lead: &str) -> Piece {
        self.text.insert_str(0, lead);
        self
    }
}

/// Text from notes, Jira, GitLab and agents made safe to draw: a tab or a
/// control character has no width for ratatui but moves the terminal's cursor,
/// so the line spills past its pane and leaves debris when it scrolls, and an
/// escape sequence restyles the terminal. Tabs become spaces; escape sequences,
/// other control characters and invisible joiners/variation selectors are dropped.
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

/// A row's state mark, colored by the state, followed by its label when
/// `labelled` (`● active`) or by a space alone (`● `); `○` without a state.
pub fn state(state: Option<RowState>, labelled: bool) -> Piece {
    let (mark, tone) = match state {
        Some(RowState::Task(TaskState::Active)) => ("●", Tone::Green),
        Some(RowState::Task(TaskState::Review)) => ("◐", Tone::Yellow),
        Some(RowState::Task(TaskState::Finished)) => ("◆", Tone::Magenta),
        Some(RowState::Done) => ("✓", Tone::Dim),
        Some(RowState::Task(TaskState::Idea)) | None => ("○", Tone::Dim),
    };
    let label = if labelled {
        state.map_or("", RowState::as_str)
    } else {
        ""
    };
    Piece::new(format!("{mark} {label}"), tone)
}

/// What `wt status` shows as the state of a worktree without a ticket.
pub fn no_ticket() -> Piece {
    Piece::dim("◌ no ticket")
}

pub fn key(key: &str) -> Piece {
    Piece::new(key, Tone::Cyan)
}

/// A ticket's Jira status, colored by its category.
pub fn status(ticket: &Ticket) -> Piece {
    let tone = match ticket.category {
        Category::Done => Tone::Green,
        Category::InProgress => Tone::Yellow,
        Category::New | Category::Unknown => Tone::Dim,
    };
    Piece::new(clean(&ticket.status), tone)
}

/// The ticket's summary, "" when it was not fetched.
pub fn summary(ticket: Option<&Ticket>) -> String {
    ticket.map(|t| clean(&t.summary)).unwrap_or_default()
}

/// `KEY Status summary`; the key alone when the ticket was not fetched.
pub fn ticket(key: &str, ticket: Option<&Ticket>) -> Vec<Piece> {
    let mut pieces = vec![self::key(key)];
    if let Some(t) = ticket {
        pieces.push(status(t).after(" "));
        pieces.push(Piece::plain(format!(" {}", clean(&t.summary))));
    }
    pieces
}

/// `!iid` in blue; `by_state` paints a merged or closed one in its state's
/// color, for rows that don't show the state.
pub fn mr_iid(mr: &Mr, by_state: bool) -> Piece {
    let iid = format!("!{}", mr.mr.iid());
    match mr.mr.state() {
        MrState::Opened => Piece::new(iid, Tone::Blue),
        _ if !by_state => Piece::new(iid, Tone::Blue),
        state => Piece::new(iid, mr_state_tone(state)),
    }
}

/// `opened`, `merged`, … colored.
pub fn mr_state(mr: &Mr) -> Piece {
    let state = mr.mr.state();
    Piece::new(state.as_str(), mr_state_tone(state))
}

fn mr_state_tone(state: MrState) -> Tone {
    match state {
        MrState::Opened => Tone::Green,
        MrState::Merged => Tone::Magenta,
        MrState::Closed | MrState::Locked | MrState::Other => Tone::Dim,
    }
}

/// The title without its leading ticket key, safe to draw.
pub fn mr_title(mr: &Mr) -> String {
    clean(&mr.short_title())
}

/// What an open MR waits on: draft, age, pipeline, approvals, unresolved
/// threads, conflicts — short enough for a row, each piece with its leading space.
pub fn review(mr: &Mr) -> Vec<Piece> {
    let mut pieces = Vec::new();
    if !mr.opened() {
        return pieces;
    }
    if mr.mr.draft {
        pieces.push(Piece::dim(" draft"));
    }
    // waiting long for review is worth seeing
    let age = mr
        .mr
        .created_at
        .as_deref()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_days());
    if let Some(days) = age.filter(|&d| d >= STALE_DAYS) {
        pieces.push(Piece::new(format!(" {days}d"), Tone::Yellow));
    }
    let Some(review) = &mr.review else {
        return pieces;
    };
    pieces.extend(pipeline(review));
    if !review.approved_by.is_empty() {
        pieces.push(Piece::new(
            format!(" ✔{}", review.approved_by.len()),
            Tone::Green,
        ));
    }
    match review.unresolved {
        0 => {}
        1 => pieces.push(Piece::new(" 1 thread", Tone::Yellow)),
        n => pieces.push(Piece::new(format!(" {n} threads"), Tone::Yellow)),
    }
    match mr.mr.detailed_merge_status.as_deref() {
        Some("conflict") => pieces.push(Piece::new(" conflict", Tone::Red)),
        Some("need_rebase") => pieces.push(Piece::new(" rebase", Tone::Red)),
        _ => {}
    }
    pieces
}

/// ` ci✓`, ` ci✗`, ` ci–` or ` ci…` by the pipeline's status; None without one.
pub fn pipeline(review: &MrReview) -> Option<Piece> {
    Some(match review.pipeline.as_deref()? {
        "success" => Piece::new(" ci✓", Tone::Green),
        "failed" => Piece::new(" ci✗", Tone::Red),
        "canceled" | "skipped" => Piece::dim(" ci–"),
        _ => Piece::new(" ci…", Tone::Yellow),
    })
}

/// `width` cells filled up to `done` (0..=1).
pub fn bar(done: f64, width: usize) -> String {
    let filled = ((done.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    "▰".repeat(filled) + &"▱".repeat(width - filled)
}

/// ` ▰▰▱▱▱ 2/5`: how many of a project's tasks are done; none without tasks.
pub fn progress(table: &Table, task: usize) -> Option<Piece> {
    let subtasks = table.subtasks(task);
    if subtasks.is_empty() {
        return None;
    }
    let done = subtasks
        .iter()
        .filter(|&&i| table.tasks[i].task.done)
        .count();
    let share = done as f64 / subtasks.len() as f64;
    let tone = if done == subtasks.len() {
        Tone::Green
    } else {
        Tone::Dim
    };
    Some(Piece::new(
        format!(" {} {done}/{}", bar(share, 8), subtasks.len()),
        tone,
    ))
}

/// An agent's status, colored by what it needs from me.
pub fn agent_status(status: AgentStatus) -> Piece {
    let tone = match status {
        AgentStatus::Working => Tone::Green,
        AgentStatus::Blocked => Tone::Red,
        AgentStatus::Idle | AgentStatus::Done => Tone::Blue,
        AgentStatus::Unknown => Tone::Dim,
    };
    Piece::new(status.as_str(), tone)
}

/// ` +N` changed files and ` ↑N` unpushed commits, then ` ✓ merged`, or
/// ` quiet Nd` for a worktree nobody has touched for a while.
pub fn tree_marks(tree: &Tree) -> Vec<Piece> {
    let mut pieces = Vec::new();
    if let Some(c) = tree.changes.filter(|&c| c > 0) {
        pieces.push(Piece::new(format!(" +{c}"), Tone::Yellow));
    }
    if let Some(u) = tree.unpushed.filter(|&u| u > 0) {
        pieces.push(Piece::new(format!(" ↑{u}"), Tone::Magenta));
    }
    if tree.finished {
        pieces.push(Piece::dim(" ✓ merged"));
    } else if tree.agents.is_empty() && tree.changes == Some(0) {
        // nobody on it and no commits for a while: forgotten, or done and not cleaned up
        let quiet = tree
            .last_commit
            .map(|t| (chrono::Utc::now().timestamp() - t) / 86_400)
            .filter(|&d| d >= STALE_DAYS);
        if let Some(days) = quiet {
            pieces.push(Piece::new(format!(" quiet {days}d"), Tone::Yellow));
        }
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{self, mr, tree};
    use super::*;
    use crate::client::gitlab::MrReview;

    fn texts(pieces: &[Piece]) -> String {
        pieces.iter().map(|p| p.text.as_str()).collect()
    }

    #[test]
    fn clean_keeps_text_and_drops_what_breaks_the_layout() {
        assert_eq!(clean("\t\t- [ ] объединить"), "        - [ ] объединить");
        assert_eq!(clean("\x1b[1;31mred\x1b[0m ok\r"), "red ok");
        assert_eq!(clean("✔\u{fe0f} done\u{200b}"), "✔ done");
    }

    #[test]
    fn state_marks_with_and_without_labels() {
        let active = Some(RowState::Task(TaskState::Active));
        assert_eq!(state(active, true), Piece::new("● active", Tone::Green));
        assert_eq!(state(active, false), Piece::new("● ", Tone::Green));
        assert_eq!(state(Some(RowState::Done), true), Piece::dim("✓ done"));
        assert_eq!(state(None, true), Piece::dim("○ "));
        assert_eq!(
            state(Some(RowState::Task(TaskState::Review)), true).tone,
            Tone::Yellow
        );
    }

    #[test]
    fn review_shows_what_an_open_mr_waits_on() {
        let mut open = mr(1, "UCP-1-a", "UCP-1: x", "opened");
        open.review = Some(MrReview {
            pipeline: Some("failed".to_string()),
            approved_by: vec!["a".to_string()],
            unresolved: 2,
        });
        assert_eq!(texts(&review(&open)), " ci✗ ✔1 2 threads");
        assert_eq!(review(&open)[0].tone, Tone::Red);
        assert_eq!(mr_title(&open), "x");

        let merged = mr(2, "UCP-1-b", "y", "merged");
        assert!(review(&merged).is_empty());
        assert_eq!(mr_iid(&merged, false).tone, Tone::Blue);
        assert_eq!(mr_iid(&merged, true).tone, Tone::Magenta);
    }

    #[test]
    fn tree_marks_flag_changes_and_quiet_worktrees() {
        let mut busy = tree("a", "UCP-1-a");
        busy.changes = Some(3);
        busy.unpushed = Some(1);
        assert_eq!(texts(&tree_marks(&busy)), " +3 ↑1");

        let mut quiet = tree("b", "UCP-1-b");
        quiet.last_commit = Some(chrono::Utc::now().timestamp() - 10 * 86_400);
        assert_eq!(texts(&tree_marks(&quiet)), " quiet 10d");
        quiet.finished = true;
        assert_eq!(texts(&tree_marks(&quiet)), " ✓ merged");
    }

    #[test]
    fn ticket_line_is_cleaned() {
        let mut t = fixtures::ticket("UCP-1", Category::InProgress, true);
        t.summary = "a\x1b[31m red\x1b[0m summary".to_string();
        assert_eq!(
            texts(&super::ticket("UCP-1", Some(&t))),
            "UCP-1 InProgress a red summary"
        );
        assert_eq!(texts(&super::ticket("UCP-2", None)), "UCP-2");
    }
}
