//! `v`: which kinds of rows the tree shows. Each toggle applies at once and is
//! kept for the next run.

use super::App;
use super::dialog::Dialog;
use crate::table::view::Hide;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A kind's flag in `Hide`.
type Flag = fn(&mut Hide) -> &mut bool;

/// The toggles in the dialog, in order: a label and its flag.
const ITEMS: [(&str, Flag); 7] = [
    ("tasks", |h| &mut h.tasks),
    ("tickets under tasks", |h| &mut h.tickets),
    ("MRs", |h| &mut h.mrs),
    ("worktrees", |h| &mut h.trees),
    ("agents", |h| &mut h.agents),
    ("section: no task", |h| &mut h.no_task),
    ("section: assigned to me in Jira", |h| &mut h.assigned),
];

/// What the TUI keeps between runs.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    hide: Hide,
}

fn path() -> PathBuf {
    crate::config::cache_dir().join("tui.json")
}

/// The last run's choice; none if never saved or unreadable.
pub(super) fn load() -> Hide {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|s| serde_json::from_str::<Saved>(&s).ok())
        .unwrap_or_default()
        .hide
}

fn save(hide: Hide) -> Result<(), String> {
    let path = path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(&Saved { hide }).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("can't save {}: {e}", path.display()))
}

/// The hidden kinds for the list's title: "MRs, agents".
pub(super) fn summary(hide: Hide) -> String {
    let mut hide = hide;
    ITEMS
        .iter()
        .filter(|(_, flag)| *flag(&mut hide))
        .map(|(label, _)| label.trim_start_matches("section: "))
        .collect::<Vec<_>>()
        .join(", ")
}

impl App {
    pub(super) fn open_hide(&mut self) {
        self.dialog = Some(Dialog::Hide { selected: 0 });
    }

    /// ↑↓ move, space or ⏎ toggles, a digit toggles its item; Esc, q or v closes.
    /// The dialog is taken out of `self` while its key is handled.
    pub(super) fn on_hide_key(&mut self, key: KeyEvent, selected: usize) {
        let digit = |c: char| {
            c.to_digit(10)
                .map(|d| d as usize)
                .filter(|d| (1..=ITEMS.len()).contains(d))
        };
        let index = match key.code {
            KeyCode::Esc | KeyCode::Char('q' | 'v') => return,
            KeyCode::Up | KeyCode::Char('k') => {
                self.dialog = Some(Dialog::Hide {
                    selected: selected.saturating_sub(1),
                });
                return;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.dialog = Some(Dialog::Hide {
                    selected: (selected + 1).min(ITEMS.len() - 1),
                });
                return;
            }
            KeyCode::Char(' ') | KeyCode::Enter => selected,
            KeyCode::Char(c) if digit(c).is_some() => digit(c).unwrap_or(1) - 1,
            _ => {
                self.dialog = Some(Dialog::Hide { selected });
                return;
            }
        };
        self.dialog = Some(Dialog::Hide { selected: index });
        let flag = ITEMS[index].1(&mut self.hide);
        *flag = !*flag;
        self.rebuild();
        if let Err(e) = save(self.hide) {
            self.say(format!("! {e}"));
        }
    }
}

/// The dialog's rows: a check for what is shown, the selected one marked.
pub(super) fn lines(hide: Hide, selected: usize) -> Vec<Line<'static>> {
    let mut hide = hide;
    ITEMS
        .iter()
        .enumerate()
        .map(|(i, (label, flag))| {
            let shown = !*flag(&mut hide);
            let mark = if i == selected { "› " } else { "  " };
            let style = if shown {
                Style::default()
            } else {
                Style::default().fg(Color::DarkGray)
            };
            Line::from(vec![
                Span::raw(mark),
                Span::styled(
                    format!("{} {}  {label}", i + 1, if shown { "[x]" } else { "[ ]" }),
                    style,
                ),
            ])
        })
        .collect()
}
