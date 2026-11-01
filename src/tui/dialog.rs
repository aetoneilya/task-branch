//! Modal dialogs: commands run in the background and report and ask here, in
//! the TUI itself, rather than in a `wt` process given the terminal.

use super::{App, Update};
use crate::cli::work::Ui;
use crate::cli::work::done::{self, TaskPlan, TreePlan};
use crate::config::Config;
use crate::vault;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use std::sync::mpsc::{Sender, channel};

pub(super) enum Dialog {
    /// Checks before a confirmation; Esc stops waiting for them.
    Checking(String),
    /// What the action will do and its question: yes runs it, no or Esc
    /// doesn't. Blocked, it only closes.
    Confirm {
        title: String,
        lines: Vec<String>,
        blocked: bool,
        action: Action,
        /// The field on: yes or no.
        yes: bool,
    },
    /// Before `wt start <key>`: it creates a note, a worktree and an agent.
    Start {
        key: String,
        summary: String,
        yes: bool,
    },
    /// A command running, then finished: what it said, and what it asks.
    Report(Report),
    /// `v`: which kinds of rows are shown; the selected toggle.
    Hide {
        selected: usize,
    },
    /// "+ new task": the form for `wt start`.
    NewTask(super::new_task::Form),
    /// "+ new project" in the project dropdown.
    NewProject(super::project::Form),
    Settings(super::settings::Form),
}

impl Dialog {
    /// Waiting on something: its spinner has to move.
    pub(super) fn busy(&self) -> bool {
        match self {
            Dialog::Checking(_) => true,
            Dialog::Report(report) => !report.finished && report.question.is_none(),
            _ => false,
        }
    }
}

pub(super) struct Report {
    title: String,
    lines: Vec<String>,
    finished: bool,
    then: Then,
    question: Option<Question>,
    /// A question was answered with Esc: the command's error is that.
    cancelled: bool,
}

/// What a command that went well leaves behind.
#[derive(Clone, Copy)]
pub(super) enum Then {
    /// Its report, until a key.
    Show,
    /// Nothing: its last line goes to the footer.
    Close,
    /// Work was opened: a popup gets out of the way, as with Close otherwise.
    Navigate,
}

/// Where a running command waits for its answer; None is cancelled.
pub(super) enum Reply {
    /// An item's index; with no items, 0 is yes and 1 no.
    Pick(Sender<Option<usize>>),
    /// A line of text.
    Text(Sender<Option<String>>),
}

/// A question of a running command; it waits for the answer on `reply`.
struct Question {
    prompt: String,
    /// Empty: yes or no, or text.
    items: Vec<String>,
    /// What is typed: the filter of a pick, the answer of a text question.
    filter: String,
    /// Into the items the filter leaves.
    selected: usize,
    reply: Reply,
}

impl Question {
    fn is_text(&self) -> bool {
        matches!(self.reply, Reply::Text(_))
    }
}

impl Question {
    /// Indexes of the items holding every word of the filter.
    fn shown(&self) -> Vec<usize> {
        let words: Vec<String> = self
            .filter
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        (0..self.items.len())
            .filter(|&i| {
                let item = self.items[i].to_lowercase();
                words.iter().all(|w| item.contains(w.as_str()))
            })
            .collect()
    }
}

/// A command's `Ui` in the TUI: lines and questions go over the update channel
/// to the dialog, and a question waits for its answer. A closed dialog drops
/// the reply channel: the question is cancelled.
struct Remote(Sender<Update>);

impl Remote {
    fn ask(&self, prompt: &str, items: Vec<String>) -> Result<Option<usize>, String> {
        let (reply, answer) = channel();
        self.send_question(prompt, items, Reply::Pick(reply))?;
        answer.recv().map_err(|_| "cancelled".to_string())
    }

    fn ask_text(&self, prompt: &str) -> Result<Option<String>, String> {
        let (reply, answer) = channel();
        self.send_question(prompt, Vec::new(), Reply::Text(reply))?;
        answer.recv().map_err(|_| "cancelled".to_string())
    }

    fn send_question(&self, prompt: &str, items: Vec<String>, reply: Reply) -> Result<(), String> {
        let question = Update::Ask {
            prompt: prompt.to_string(),
            items,
            reply,
        };
        self.0.send(question).map_err(|_| "cancelled".to_string())
    }
}

impl Ui for Remote {
    fn say(&mut self, line: String) {
        let _ = self.0.send(Update::Said(line));
    }

    fn pick(&mut self, prompt: &str, items: &[String]) -> Result<usize, String> {
        self.ask(prompt, items.to_vec())?
            .ok_or_else(|| "cancelled".to_string())
    }

    fn confirm(&mut self, prompt: &str) -> Result<bool, String> {
        Ok(self.ask(prompt, Vec::new())? == Some(0))
    }

    fn input(&mut self, prompt: &str) -> Result<String, String> {
        self.ask_text(prompt)?
            .ok_or_else(|| "cancelled".to_string())
    }
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
            Action::DoneTask(plan) => {
                let mut lines = plan.steps();
                lines.push(String::new());
                lines.push(plan.question().to_string());
                (&plan.blockers, lines)
            }
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

    /// The field the choice starts on: no when something is lost.
    fn starts_on_yes(&self) -> bool {
        match self {
            Action::DoneTask(plan) => !plan.loses_changes(),
            Action::DoneTree(..) => true,
        }
    }

    fn run(self, config: &Config, ui: &mut dyn Ui) -> Result<(), String> {
        let say = &mut |line| ui.say(line);
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

    /// Runs a `wt` command in the background, its report and questions in a
    /// dialog.
    pub(super) fn run_command<F>(&mut self, title: String, then: Then, command: F)
    where
        F: FnOnce(&Config, &mut dyn Ui) -> Result<(), String> + Send + 'static,
    {
        self.dialog = Some(Dialog::Report(Report {
            title,
            lines: Vec::new(),
            finished: false,
            then,
            question: None,
            cancelled: false,
        }));
        let (config, tx) = (self.config.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let result = command(&config, &mut Remote(tx.clone()));
            let _ = tx.send(Update::Finished(result));
        });
    }

    pub(super) fn on_plan(&mut self, plan: Result<Action, String>) {
        // given up on meanwhile
        let Some(Dialog::Checking(title)) = self.dialog.take() else {
            return;
        };
        match plan {
            Ok(action) => {
                let (lines, blocked) = action.lines();
                self.dialog = Some(Dialog::Confirm {
                    title: action.title(),
                    lines,
                    blocked,
                    yes: action.starts_on_yes(),
                    action,
                });
            }
            Err(e) => {
                self.dialog = Some(Dialog::Report(Report {
                    title,
                    lines: e.lines().map(|l| format!("! {l}")).collect(),
                    finished: true,
                    then: Then::Show,
                    question: None,
                    cancelled: false,
                }))
            }
        }
    }

    pub(super) fn on_said(&mut self, line: String) {
        if let Some(Dialog::Report(report)) = &mut self.dialog {
            report.lines.push(line);
        }
    }

    pub(super) fn on_ask(&mut self, prompt: String, items: Vec<String>, reply: Reply) {
        // no dialog to ask in: dropping `reply` cancels the question
        if let Some(Dialog::Report(report)) = &mut self.dialog {
            report.question = Some(Question {
                prompt,
                items,
                filter: String::new(),
                selected: 0,
                reply,
            });
        }
    }

    pub(super) fn on_finished(&mut self, result: Result<(), String>) {
        self.refresh();
        let Some(Dialog::Report(report)) = &mut self.dialog else {
            return;
        };
        report.finished = true;
        report.question = None;
        if report.cancelled {
            self.dialog = None;
            self.say("cancelled");
            return;
        }
        if let Err(e) = result {
            report.lines.extend(e.lines().map(|l| format!("! {l}")));
            return;
        }
        let last = report.lines.last().cloned();
        match report.then {
            Then::Show => return,
            Then::Navigate if self.popup => self.quit = true,
            Then::Close | Then::Navigate => {}
        }
        self.dialog = None;
        if let Some(last) = last {
            self.say(last);
        }
    }

    pub(super) fn on_dialog_key(&mut self, key: KeyEvent) {
        let close = matches!(key.code, KeyCode::Esc | KeyCode::Char('q'));
        match self.dialog.take() {
            Some(Dialog::Checking(_)) if close => {}
            Some(Dialog::Confirm { .. }) if close => {}
            Some(Dialog::Confirm {
                title,
                lines,
                blocked: false,
                action,
                mut yes,
            }) => match choose(&mut yes, key.code) {
                Some(true) => {
                    self.run_command(title, Then::Show, move |config, ui| action.run(config, ui))
                }
                Some(false) => {}
                None => {
                    self.dialog = Some(Dialog::Confirm {
                        title,
                        lines,
                        blocked: false,
                        action,
                        yes,
                    })
                }
            },
            Some(Dialog::Start { .. }) if close => {}
            Some(Dialog::Start {
                key: ticket,
                summary,
                mut yes,
            }) => match choose(&mut yes, key.code) {
                Some(true) => self.start_work(ticket),
                Some(false) => {}
                None => {
                    self.dialog = Some(Dialog::Start {
                        key: ticket,
                        summary,
                        yes,
                    })
                }
            },
            Some(Dialog::Hide { selected }) => self.on_hide_key(key, selected),
            Some(Dialog::NewTask(form)) => self.on_new_task_key(key, form),
            Some(Dialog::NewProject(form)) => self.on_new_project_key(key, form),
            Some(Dialog::Settings(form)) => self.on_settings_key(key, form),
            Some(Dialog::Report(mut report)) => {
                if report.question.is_some() {
                    answer(&mut report, key);
                } else if report.finished {
                    return;
                }
                // running: nothing to do but wait
                self.dialog = Some(Dialog::Report(report));
            }
            other => self.dialog = other,
        }
    }

    pub(super) fn draw_dialog(&self, frame: &mut Frame) {
        let Some(dialog) = &self.dialog else { return };
        let red = Style::default().fg(Color::Red);
        let line = |l: &String| {
            let style = if l.starts_with("! ") {
                red
            } else {
                Style::default()
            };
            Line::from(Span::styled(l.clone(), style))
        };
        let (title, mut text, hint): (&str, Vec<Line>, &str) = match dialog {
            Dialog::Checking(title) => (
                title,
                vec![Line::raw(format!("{} checking", super::anim::spinner()))],
                "Esc cancel",
            ),
            Dialog::Confirm {
                title,
                lines,
                blocked,
                yes,
                ..
            } => {
                let mut text: Vec<Line> = lines.iter().map(line).collect();
                if *blocked {
                    (title.as_str(), text, "Esc close")
                } else {
                    text.push(Line::raw(""));
                    text.push(choice_line(*yes));
                    (title.as_str(), text, CHOICE_HINT)
                }
            }
            Dialog::Start { key, summary, yes } => (
                "start a task",
                vec![
                    Line::raw(format!("{key} «{summary}»")),
                    Line::raw("a note, a worktree and an agent for it"),
                    Line::raw(""),
                    choice_line(*yes),
                ],
                CHOICE_HINT,
            ),
            Dialog::Report(report) => {
                let mut text: Vec<Line> = report.lines.iter().map(line).collect();
                let hint = match &report.question {
                    Some(question) => {
                        text.extend(question_lines(question));
                        if question.is_text() {
                            "type   ⏎ ok   Esc cancel"
                        } else if question.items.is_empty() {
                            CHOICE_HINT
                        } else {
                            "type to filter   ↑↓ move   ⏎ pick   Esc cancel"
                        }
                    }
                    None if report.finished => "any key closes",
                    None => {
                        text.push(Line::raw(""));
                        text.push(Line::styled(
                            format!("{} working", super::anim::spinner()),
                            Style::default().fg(Color::Yellow),
                        ));
                        "wait for it to finish"
                    }
                };
                (&report.title, text, hint)
            }
            Dialog::NewTask(form) => (
                "new task",
                super::new_task::lines(form),
                super::new_task::hint(form),
            ),
            Dialog::NewProject(form) => (
                "new project",
                super::project::lines(form),
                super::project::HINT,
            ),
            Dialog::Hide { selected } => (
                "show",
                super::hide::lines(self.hide, *selected),
                "↑↓ move   space toggle   Esc close",
            ),
            Dialog::Settings(form) => (
                "settings",
                super::settings::lines(form),
                super::settings::HINT,
            ),
        };

        let area = frame.area();
        let longest = text.iter().map(Line::width).max().unwrap_or(0);
        let width = (longest.max(title.chars().count()).max(hint.chars().count()) as u16 + 6)
            .clamp(40, 100)
            .min(area.width.saturating_sub(4));
        let inner = width.saturating_sub(2).max(1) as usize;
        let rows = |l: &Line| l.width().div_ceil(inner).max(1);
        // a long report keeps its end in view
        let room = area.height.saturating_sub(6) as usize;
        let mut used = 0;
        let keep = text
            .iter()
            .rev()
            .take_while(|l| {
                used += rows(l);
                used <= room
            })
            .count();
        text.drain(..text.len() - keep);
        text.push(Line::raw(""));
        text.push(Line::from(Span::styled(
            hint,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )));
        let height = text.iter().map(rows).sum::<usize>() as u16 + 2;
        let popup = Rect {
            x: area.x + area.width.saturating_sub(width) / 2,
            y: area.y + area.height.saturating_sub(height) / 2,
            width,
            height: height.min(area.height),
        };
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(text)
                .block(Block::bordered().title(format!(" {title} ")))
                .wrap(Wrap { trim: false }),
            popup,
        );
    }
}

/// A key on a question: the answer goes back to the command, or the choice moves.
fn answer(report: &mut Report, key: KeyEvent) {
    let Some(question) = &mut report.question else {
        return;
    };
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if let Reply::Text(reply) = &question.reply {
        let answer = match key.code {
            KeyCode::Esc => Some(None),
            KeyCode::Enter if !question.filter.trim().is_empty() => {
                Some(Some(question.filter.trim().to_string()))
            }
            KeyCode::Backspace => {
                question.filter.pop();
                None
            }
            KeyCode::Char(c) if !ctrl => {
                question.filter.push(c);
                None
            }
            _ => None,
        };
        if let Some(answer) = answer {
            report.cancelled = answer.is_none();
            let _ = reply.send(answer);
            report.question = None;
        }
        return;
    }
    let reply = if question.items.is_empty() {
        // `selected`: 0 yes, 1 no
        let mut yes = question.selected == 0;
        let picked = match key.code {
            KeyCode::Esc => Some(None),
            code => choose(&mut yes, code).map(|yes| Some(if yes { 0 } else { 1 })),
        };
        question.selected = if yes { 0 } else { 1 };
        picked
    } else {
        let shown = question.shown();
        match key.code {
            KeyCode::Esc => Some(None),
            KeyCode::Enter => shown.get(question.selected).map(|&i| Some(i)),
            KeyCode::Up => {
                question.selected = question.selected.saturating_sub(1);
                None
            }
            KeyCode::Char('p' | 'k') if ctrl => {
                question.selected = question.selected.saturating_sub(1);
                None
            }
            KeyCode::Down | KeyCode::Tab => {
                question.selected = (question.selected + 1).min(shown.len().saturating_sub(1));
                None
            }
            KeyCode::Char('n' | 'j') if ctrl => {
                question.selected = (question.selected + 1).min(shown.len().saturating_sub(1));
                None
            }
            KeyCode::Backspace => {
                question.filter.pop();
                question.selected = 0;
                None
            }
            KeyCode::Char(c) if !ctrl => {
                question.filter.push(c);
                question.selected = 0;
                None
            }
            _ => None,
        }
    };
    if let (Some(answer), Reply::Pick(reply)) = (reply, &question.reply) {
        report.cancelled = key.code == KeyCode::Esc;
        let _ = reply.send(answer);
        report.question = None;
    }
}

/// The hint under a yes/no choice.
const CHOICE_HINT: &str = "←→ choose   ⏎ ok   Esc cancel";

/// A key on a yes/no choice, `yes` the field it is on: the answer once picked.
/// The arrows, Tab and h l move; ⏎ picks the field, y and n pick at once.
fn choose(yes: &mut bool, code: KeyCode) -> Option<bool> {
    match code {
        KeyCode::Char('y') => Some(true),
        KeyCode::Char('n') => Some(false),
        KeyCode::Enter => Some(*yes),
        KeyCode::Left
        | KeyCode::Right
        | KeyCode::Up
        | KeyCode::Down
        | KeyCode::Tab
        | KeyCode::BackTab
        | KeyCode::Char('h' | 'l') => {
            *yes = !*yes;
            None
        }
        _ => None,
    }
}

/// The two fields, the one the choice is on lit.
fn choice_line(yes: bool) -> Line<'static> {
    let field = |label: &str, on: bool| {
        if on {
            Span::styled(
                format!("[ {label} ]"),
                Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
            )
        } else {
            Span::styled(format!("  {label}  "), Style::default().fg(Color::DarkGray))
        }
    };
    Line::from(vec![
        Span::raw("  "),
        field("yes", yes),
        Span::raw("   "),
        field("no", !yes),
    ])
}

/// How many items of a pick show at once.
const PICK_ROWS: usize = 12;

fn question_lines(question: &Question) -> Vec<Line<'static>> {
    let yellow = Style::default().fg(Color::Yellow);
    if question.is_text() {
        return vec![Line::from(vec![
            Span::styled(format!("{}: ", question.prompt), yellow),
            Span::raw(format!("{}▏", question.filter)),
        ])];
    }
    if question.items.is_empty() {
        return vec![
            Line::from(Span::styled(
                format!("{}?", question.prompt.trim_end_matches('?')),
                yellow,
            )),
            Line::raw(""),
            choice_line(question.selected == 0),
        ];
    }
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{}: ", question.prompt), yellow),
        Span::raw(format!("{}▏", question.filter)),
    ])];
    let shown = question.shown();
    if shown.is_empty() {
        lines.push(Line::styled(
            "  nothing matches",
            Style::default().fg(Color::DarkGray),
        ));
    }
    // the window of items scrolls with the selection
    let start = question.selected.saturating_sub(PICK_ROWS - 1);
    for (n, &i) in shown.iter().enumerate().skip(start).take(PICK_ROWS) {
        let item = question.items[i].clone();
        lines.push(if n == question.selected {
            Line::styled(
                format!("› {item}"),
                Style::default().add_modifier(Modifier::BOLD),
            )
        } else {
            Line::raw(format!("  {item}"))
        });
    }
    if shown.len() > start + PICK_ROWS {
        lines.push(Line::styled(
            format!("  … {} more", shown.len() - start - PICK_ROWS),
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyModifiers;

    fn report(reply: Reply) -> Report {
        Report {
            title: String::new(),
            lines: Vec::new(),
            finished: false,
            then: Then::Show,
            question: Some(Question {
                prompt: "Task name".to_string(),
                items: Vec::new(),
                filter: String::new(),
                selected: 0,
                reply,
            }),
            cancelled: false,
        }
    }

    fn press(report: &mut Report, code: KeyCode) {
        answer(report, KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn a_text_question_is_typed_and_sent() {
        let (tx, rx) = channel();
        let mut report = report(Reply::Text(tx));
        // nothing typed: Enter waits
        press(&mut report, KeyCode::Enter);
        assert!(report.question.is_some());
        for c in "ab".chars() {
            press(&mut report, KeyCode::Char(c));
        }
        press(&mut report, KeyCode::Backspace);
        press(&mut report, KeyCode::Char('c'));
        press(&mut report, KeyCode::Enter);
        assert_eq!(rx.recv().unwrap(), Some("ac".to_string()));
        assert!(report.question.is_none() && !report.cancelled);

        let (tx, rx) = channel();
        let mut report = super::tests::report(Reply::Text(tx));
        press(&mut report, KeyCode::Esc);
        assert_eq!(rx.recv().unwrap(), None);
        assert!(report.cancelled);
    }

    #[test]
    fn yes_or_no_is_picked_from_two_fields() {
        let (tx, rx) = channel();
        let mut report = report(Reply::Pick(tx));
        // starts on yes; → moves to no, ⏎ picks it
        press(&mut report, KeyCode::Right);
        assert!(report.question.is_some());
        press(&mut report, KeyCode::Enter);
        assert_eq!(rx.recv().unwrap(), Some(1));

        let (tx, rx) = channel();
        let mut report = super::tests::report(Reply::Pick(tx));
        press(&mut report, KeyCode::Char('y'));
        assert_eq!(rx.recv().unwrap(), Some(0));
    }
}
