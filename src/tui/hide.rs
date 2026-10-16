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
const ITEMS: [(&str, Flag); 3] = [
    ("tickets under tasks", |h| &mut h.tickets),
    ("MRs", |h| &mut h.mrs),
    ("worktrees and agents", |h| &mut h.trees),
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
        .map(|(label, _)| *label)
        .collect::<Vec<_>>()
        .join(", ")
}

impl App {
    pub(super) fn open_hide(&mut self) {
        self.dialog = Some(Dialog::Hide { selected: 0 });
    }

    /// ↑↓ move, space or ⏎ toggles; Esc, q or v closes.
    /// The dialog is taken out of `self` while its key is handled.
    pub(super) fn on_hide_key(&mut self, key: KeyEvent, selected: usize) {
        let selected = match key.code {
            KeyCode::Esc | KeyCode::Char('q' | 'v') => return,
            KeyCode::Up | KeyCode::Char('k') => selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => (selected + 1).min(ITEMS.len() - 1),
            KeyCode::Char(' ') | KeyCode::Enter => {
                self.toggle(selected);
                selected
            }
            _ => selected,
        };
        self.dialog = Some(Dialog::Hide { selected });
    }

    fn toggle(&mut self, index: usize) {
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
                    format!("{}  {label}", if shown { "[x]" } else { "[ ]" }),
                    style,
                ),
            ])
        })
        .collect()
}
