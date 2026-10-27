//! "+ new task": a form for `wt start` — the name, a Jira ticket (none, an
//! existing one, a new one), whether to make a worktree and an agent.

use super::App;
use super::dialog::{Dialog, Then};
use crate::cli::StartArgs;
use crate::cli::work;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Jira {
    None,
    Existing,
    New,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Jira,
    Key,
    Worktree,
    Agent,
}

const JIRA: [(Jira, &str); 3] = [
    (Jira::None, "none"),
    (Jira::Existing, "existing"),
    (Jira::New, "new"),
];

pub(super) struct Form {
    name: String,
    jira: Jira,
    key: String,
    worktree: bool,
    agent: bool,
    field: Field,
    /// The project has Jira: else no ticket fields.
    has_jira: bool,
    /// Why the last ⏎ did not start it.
    error: Option<String>,
}

impl Form {
    fn new(has_jira: bool) -> Form {
        Form {
            name: String::new(),
            jira: Jira::None,
            key: String::new(),
            worktree: true,
            agent: true,
            field: Field::Name,
            has_jira,
            error: None,
        }
    }

    /// The fields shown: the ticket key only for an existing ticket, neither
    /// without Jira.
    fn fields(&self) -> Vec<Field> {
        let mut fields = vec![Field::Name];
        if self.has_jira {
            fields.push(Field::Jira);
        }
        if self.jira == Jira::Existing {
            fields.push(Field::Key);
        }
        fields.extend([Field::Worktree, Field::Agent]);
        fields
    }

    fn move_field(&mut self, delta: isize) {
        let fields = self.fields();
        let at = fields.iter().position(|f| *f == self.field).unwrap_or(0) as isize;
        self.field = fields[(at + delta).clamp(0, fields.len() as isize - 1) as usize];
    }

    /// ←→ on a choice: the next option, around; on a checkbox, it flips.
    fn turn(&mut self, delta: isize) {
        fn next<T: Copy + PartialEq>(options: &[(T, &str)], now: T, delta: isize) -> T {
            let at = options.iter().position(|(o, _)| *o == now).unwrap_or(0) as isize;
            options[(at + delta).rem_euclid(options.len() as isize) as usize].0
        }
        match self.field {
            Field::Jira => self.jira = next(&JIRA, self.jira, delta),
            Field::Worktree | Field::Agent => self.flip(),
            Field::Name | Field::Key => {}
        }
    }

    /// Space or ←→ on a checkbox.
    fn flip(&mut self) {
        match self.field {
            Field::Worktree => self.worktree = !self.worktree,
            Field::Agent => self.agent = !self.agent,
            _ => {}
        }
    }

    fn is_checkbox(&self) -> bool {
        matches!(self.field, Field::Worktree | Field::Agent)
    }

    fn text(&mut self) -> Option<&mut String> {
        match self.field {
            Field::Name => Some(&mut self.name),
            Field::Key => Some(&mut self.key),
            Field::Jira | Field::Worktree | Field::Agent => None,
        }
    }

    /// What `wt start` gets. An existing ticket without a name names the task
    /// after its summary.
    fn args(&self) -> Result<StartArgs, String> {
        let name = self.name.trim().to_string();
        let key = self.key.trim().to_uppercase();
        let (target, tickets) = match self.jira {
            Jira::Existing if !work::is_ticket_key(&key) => {
                return Err("the ticket's key, like UCP-12345".to_string());
            }
            Jira::Existing if name.is_empty() => (key, Vec::new()),
            Jira::Existing => (name, vec![key]),
            _ if name.is_empty() => return Err("a name for the task".to_string()),
            _ => (name, Vec::new()),
        };
        Ok(StartArgs {
            target: Some(target),
            tickets,
            new_ticket: self.jira == Jira::New,
            no_worktree: !self.worktree,
            no_agent: !self.agent,
            ..StartArgs::default()
        })
    }
}

impl App {
    pub(super) fn open_new_task(&mut self) {
        let has_jira = self.config.jira.is_some();
        self.dialog = Some(Dialog::NewTask(Form::new(has_jira)));
    }

    /// ↑↓ and Tab move between fields, ←→ choose, typing fills the text
    /// fields; ⏎ starts it, Esc closes.
    pub(super) fn on_new_task_key(&mut self, key: KeyEvent, mut form: Form) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Enter => match form.args() {
                Ok(args) => {
                    let target = args.target.clone().unwrap_or_default();
                    let then = if args.no_worktree && args.no_agent {
                        Then::Close
                    } else {
                        Then::Navigate
                    };
                    self.run_command(format!("start {target}"), then, move |config, ui| {
                        work::start(config, args, ui)
                    });
                    return;
                }
                Err(e) => form.error = Some(e),
            },
            KeyCode::Up | KeyCode::BackTab => form.move_field(-1),
            KeyCode::Down | KeyCode::Tab => form.move_field(1),
            KeyCode::Left => form.turn(-1),
            KeyCode::Right => form.turn(1),
            KeyCode::Backspace => {
                if let Some(text) = form.text() {
                    text.pop();
                }
            }
            KeyCode::Char(' ') if form.is_checkbox() => form.flip(),
            KeyCode::Char(c) if !ctrl => {
                if let Some(text) = form.text() {
                    text.push(c);
                }
            }
            _ => {}
        }
        self.dialog = Some(Dialog::NewTask(form));
    }
}

/// The form's rows: a label, then the text or the options; the field the
/// cursor is on is marked.
pub(super) fn lines(form: &Form) -> Vec<Line<'static>> {
    let lit = Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);
    let label = |field: Field, text: &str| {
        let on = form.field == field;
        Span::styled(
            format!("{}{text:<10}", if on { "› " } else { "  " }),
            if on {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            },
        )
    };
    let choice = |options: Vec<(&str, bool)>| {
        let mut spans = vec![];
        for (text, chosen) in options {
            spans.push(if chosen {
                Span::styled(format!("[ {text} ]"), lit)
            } else {
                Span::styled(format!("  {text}  "), dim)
            });
            spans.push(Span::raw(" "));
        }
        spans
    };
    let text = |field: Field, value: &str, placeholder: &str| {
        let cursor = if form.field == field { "▏" } else { "" };
        if value.is_empty() {
            vec![
                Span::raw(cursor.to_string()),
                Span::styled(placeholder.to_string(), dim),
            ]
        } else {
            vec![Span::raw(format!("{value}{cursor}"))]
        }
    };

    // `› [✓] worktree`: checked green, unchecked dim
    let checkbox = |field: Field, checked: bool, text: &str| {
        let on = form.field == field;
        let (mark, style) = if checked {
            (
                "[✓]",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            ("[ ]", dim)
        };
        let text_style = match (on, checked) {
            (true, _) => Style::default().fg(Color::Yellow),
            (false, true) => Style::default(),
            (false, false) => dim,
        };
        Line::from(vec![
            Span::raw(if on { "› " } else { "  " }),
            Span::styled(mark, style),
            Span::styled(format!(" {text}"), text_style),
        ])
    };

    let mut lines = Vec::new();
    for field in form.fields() {
        match field {
            Field::Worktree => {
                lines.push(checkbox(field, form.worktree, "worktree"));
                continue;
            }
            Field::Agent => {
                lines.push(checkbox(field, form.agent, "agent"));
                continue;
            }
            _ => {}
        }
        let mut spans = match field {
            Field::Name => vec![label(field, "name")],
            Field::Jira => vec![label(field, "jira")],
            Field::Key => vec![label(field, "ticket")],
            Field::Worktree | Field::Agent => unreachable!("drawn as checkboxes"),
        };
        spans.extend(match field {
            Field::Name => {
                let hint = if form.jira == Jira::Existing {
                    "the ticket's summary"
                } else {
                    "the task's name"
                };
                text(field, &form.name, hint)
            }
            Field::Jira => choice(JIRA.iter().map(|(j, t)| (*t, *j == form.jira)).collect()),
            Field::Key => text(field, &form.key, "UCP-12345"),
            Field::Worktree | Field::Agent => unreachable!("drawn as checkboxes"),
        });
        lines.push(Line::from(spans));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        match (form.worktree, form.agent) {
            (true, true) => "the note, a worktree tab with claude in the task's workspace",
            (true, false) => "the note, a worktree tab with a shell in the task's workspace",
            (false, true) => "the note, claude in the main checkout in the task's workspace",
            (false, false) => "only the task note",
        },
        dim,
    ));
    if let Some(error) = &form.error {
        lines.push(Line::styled(
            format!("! {error}"),
            Style::default().fg(Color::Red),
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_existing_ticket_without_a_name_names_the_task() {
        let mut form = Form::new(true);
        assert!(form.args().is_err());
        form.jira = Jira::Existing;
        form.key = "ucp-1".into();
        form.worktree = false;
        let args = form.args().unwrap();
        assert_eq!(args.target.as_deref(), Some("UCP-1"));
        assert!(args.tickets.is_empty());
        assert!(args.no_worktree && !args.no_agent);

        form.name = "video".into();
        let args = form.args().unwrap();
        assert_eq!(args.target.as_deref(), Some("video"));
        assert_eq!(args.tickets, vec!["UCP-1"]);
    }

    #[test]
    fn the_key_field_shows_only_for_an_existing_ticket() {
        let mut form = Form::new(true);
        form.field = Field::Jira;
        form.move_field(1);
        assert!(form.field == Field::Worktree);
        form.field = Field::Jira;
        form.turn(1);
        form.move_field(1);
        assert!(form.field == Field::Key);
    }
}
