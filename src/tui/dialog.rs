//! Modal dialogs: work that checks first, asks, then reports as it goes, in the
//! TUI itself rather than in a `wt` process given the terminal.

use super::{App, Update};
use crate::cli::work::done::{self, TaskPlan, TreePlan};
use crate::config::Config;
use crate::vault;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

pub(super) enum Dialog {
    /// Checks before a confirmation; Esc stops waiting for them.
    Checking(String),
    /// What the action will do: y runs it. Blocked, it only closes.
    Confirm {
        title: String,
        lines: Vec<String>,
        blocked: bool,
        action: Action,
    },
    /// The action running, then finished: its report.
    Report {
        title: String,
        lines: Vec<String>,
        finished: bool,
    },
}

/// An action checked and ready to run.
pub(super) enum Action {
    DoneTask(Box<TaskPlan>),
    /// With a warning about the data the plan was made on.
    DoneTree(Box<TreePlan>, Option<String>),
}

impl Action {
    fn title(&self) -> String {
        match self {
            Action::DoneTask(plan) => format!("close task \"{}\"", plan.name()),
            Action::DoneTree(plan, _) => format!("remove worktree {}", plan.name()),
        }
    }

    /// What will happen, or what holds it back; and whether something does.
    fn lines(&self) -> (Vec<String>, bool) {
        let (blockers, mut lines) = match self {
            Action::DoneTask(plan) => (&plan.blockers, plan.steps()),
            Action::DoneTree(plan, warning) => {
                let mut lines = vec![plan.step()];
                lines.extend(warning.iter().map(|w| format!("! {w}")));
                (&plan.blockers, lines)
            }
        };
        if blockers.is_empty() {
            return (lines, false);
        }
        lines = vec!["not finished:".to_string()];
        lines.extend(blockers.iter().map(|b| format!("  {b}")));
        lines.push(String::new());
        lines.push("finish these, or wt done --force in a terminal".to_string());
        (lines, true)
    }

    fn run(self, config: &Config, say: &mut dyn FnMut(String)) -> Result<(), String> {
        match self {
            Action::DoneTask(plan) => done::finish_task(config, *plan, false, say),
            Action::DoneTree(plan, _) => done::finish_tree(*plan, false, say),
        }
    }
}

impl App {
    /// `d` on a task: closes it, with its worktrees and Jira tickets.
    pub(super) fn close_task(&mut self, name: String) {
        self.check(format!("close task \"{name}\""), move |config| {
            let (tasks, _) = vault::task::load(config)?;
            let plan = done::plan_task(config, tasks, &name, false)?;
            Ok(Action::DoneTask(Box::new(plan)))
        });
    }

    /// `d` on a worktree: removes it, with its branch and herdr tabs.
    pub(super) fn remove_tree(&mut self, tree: usize) {
        let wt = &self.table.tree(tree).wt;
        let path = wt.path.clone();
        self.check(format!("remove worktree {}", wt.name()), move |config| {
            let (table, warning) = done::trees_for_done(config, false)?;
            let tree = table
                .trees
                .iter()
                .position(|t| t.wt.path == path)
                .ok_or("the worktree is gone")?;
            let plan = TreePlan::new(table, tree, false)?;
            Ok(Action::DoneTree(Box::new(plan), warning))
        });
    }

    /// Plans in the background behind a "checking" dialog; the confirmation
    /// follows as `Update::Plan`.
    fn check<F>(&mut self, title: String, plan: F)
    where
        F: FnOnce(&Config) -> Result<Action, String> + Send + 'static,
    {
        self.dialog = Some(Dialog::Checking(title));
        let (config, tx) = (self.config.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Update::Plan(plan(&config)));
        });
    }

    pub(super) fn on_plan(&mut self, plan: Result<Action, String>) {
        // given up on meanwhile
        let Some(Dialog::Checking(title)) = self.dialog.take() else {
            return;
        };
        self.dialog = Some(match plan {
            Ok(action) => {
                let (lines, blocked) = action.lines();
                Dialog::Confirm {
                    title: action.title(),
                    lines,
                    blocked,
                    action,
                }
            }
            Err(e) => Dialog::Report {
                title,
                lines: e.lines().map(|l| format!("! {l}")).collect(),
                finished: true,
            },
        });
    }

    pub(super) fn on_said(&mut self, line: String) {
        if let Some(Dialog::Report { lines, .. }) = &mut self.dialog {
            lines.push(line);
        }
    }

    pub(super) fn on_finished(&mut self, result: Result<(), String>) {
        if let Some(Dialog::Report {
            lines, finished, ..
        }) = &mut self.dialog
        {
            if let Err(e) = result {
                lines.extend(e.lines().map(|l| format!("! {l}")));
            }
            *finished = true;
        }
        self.refresh();
    }

    pub(super) fn on_dialog_key(&mut self, key: KeyEvent) {
        let close = matches!(key.code, KeyCode::Esc | KeyCode::Char('n' | 'q'));
        match self.dialog.take() {
            Some(Dialog::Checking(_)) if close => {}
            Some(Dialog::Confirm {
                title,
                blocked: false,
                action,
                ..
            }) if matches!(key.code, KeyCode::Char('y') | KeyCode::Enter) => {
                self.run_action(title, action)
            }
            Some(Dialog::Confirm { .. }) if close => {}
            // running: nothing to do but wait
            Some(Dialog::Report { finished: true, .. }) => {}
            other => self.dialog = other,
        }
    }

    fn run_action(&mut self, title: String, action: Action) {
        self.dialog = Some(Dialog::Report {
            title,
            lines: Vec::new(),
            finished: false,
        });
        let (config, tx) = (self.config.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let said = tx.clone();
            let result = action.run(&config, &mut |line| {
                let _ = said.send(Update::Said(line));
            });
            let _ = tx.send(Update::Finished(result));
        });
    }

    pub(super) fn draw_dialog(&self, frame: &mut Frame) {
        let Some(dialog) = &self.dialog else { return };
        let (title, lines, hint) = match dialog {
            Dialog::Checking(title) => (title, &vec!["checking…".to_string()], "Esc cancel"),
            Dialog::Confirm {
                title,
                lines,
                blocked,
                ..
            } => (
                title,
                lines,
                if *blocked {
                    "Esc close"
                } else {
                    "y go   Esc cancel"
                },
            ),
            Dialog::Report {
                title,
                lines,
                finished,
            } => (
                title,
                lines,
                if *finished {
                    "any key closes"
                } else {
                    "working…"
                },
            ),
        };

        let area = frame.area();
        let longest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let width = (longest.max(title.chars().count()) as u16 + 6)
            .clamp(40, 100)
            .min(area.width.saturating_sub(4));
        let room = area.height.saturating_sub(6) as usize;
        // a long report keeps its end in view
        let shown = &lines[lines.len().saturating_sub(room)..];
        let height = shown.len() as u16 + 4;
        let popup = Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        };

        let mut text: Vec<Line> = shown
            .iter()
            .map(|l| {
                let style = if l.starts_with("! ") {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default()
                };
                Line::from(Span::styled(l.clone(), style))
            })
            .collect();
        text.push(Line::raw(""));
        text.push(Line::from(Span::styled(
            hint,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )));
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(text)
                .block(Block::bordered().title(format!(" {title} ")))
                .wrap(Wrap { trim: false }),
            popup,
        );
    }
}
