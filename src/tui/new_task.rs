//! "+ new task": a form for `wt start` — the name, a Jira ticket (none, a new
//! one, or an existing one, all in one dropdown), whether to make a worktree,
//! and which agent to start (or none).

use super::App;
use super::dialog::{Dialog, Then};
use crate::cli::StartArgs;
use crate::cli::work;
use crate::client::agent::AgentKind;
use crate::client::jira::Category;
use crate::table::Table;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

#[derive(Clone, PartialEq, Eq, Debug)]
enum Jira {
    None,
    New,
    /// The ticket's key.
    Existing(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Jira,
    Worktree,
    Agent,
}

/// A ticket the existing one is picked from.
struct Ticket {
    key: String,
    status: String,
    summary: String,
}

/// Open tickets without a task: the ones assigned to me and the ones worked on
/// without a note; in progress first, then newest.
fn tickets(table: &Table) -> Vec<Ticket> {
    let mut open: Vec<_> = table
        .loose
        .iter()
        .filter_map(|l| table.tickets.get(&l.key))
        .filter(|t| !t.closed())
        .collect();
    open.sort_by(|a, b| {
        (b.category == Category::InProgress)
            .cmp(&(a.category == Category::InProgress))
            .then_with(|| b.created.cmp(&a.created))
    });
    open.into_iter()
        .map(|t| Ticket {
            key: t.key.clone(),
            status: t.status.clone(),
            summary: t.summary.clone(),
        })
        .collect()
}

/// How many options of the dropdown show at once.
const SHOWN: usize = 10;

const AGENTS: [Option<AgentKind>; 3] = [Some(AgentKind::Claude), Some(AgentKind::Codex), None];

fn agent_name(agent: Option<AgentKind>) -> &'static str {
    agent.map_or("none", AgentKind::binary)
}

pub(super) struct Form {
    name: String,
    jira: Jira,
    worktree: bool,
    agent: Option<AgentKind>,
    field: Field,
    /// The current field's dropdown is open, on this option.
    menu: Option<usize>,
    /// What is typed to filter the dropdown.
    filter: String,
    tickets: Vec<Ticket>,
    /// The project has Jira: else no ticket field.
    has_jira: bool,
    /// Why the last ⏎ did not start it.
    error: Option<String>,
}

impl Form {
    fn new(has_jira: bool, tickets: Vec<Ticket>, agent: AgentKind) -> Form {
        Form {
            name: String::new(),
            jira: Jira::None,
            worktree: true,
            agent: Some(agent),
            field: Field::Name,
            menu: None,
            filter: String::new(),
            tickets,
            has_jira,
            error: None,
        }
    }

    /// The fields shown: no ticket without Jira.
    fn fields(&self) -> Vec<Field> {
        let mut fields = vec![Field::Name];
        if self.has_jira {
            fields.push(Field::Jira);
        }
        fields.extend([Field::Worktree, Field::Agent]);
        fields
    }

    fn move_field(&mut self, delta: isize) {
        let fields = self.fields();
        let at = fields.iter().position(|f| *f == self.field).unwrap_or(0) as isize;
        self.field = fields[(at + delta).clamp(0, fields.len() as isize - 1) as usize];
    }

    /// Space or ←→ flips a checkbox or opens a dropdown.
    fn flip(&mut self) {
        match self.field {
            Field::Jira => {
                self.filter.clear();
                let at = self.options().iter().position(|(j, ..)| *j == self.jira);
                self.menu = Some(at.unwrap_or(0));
            }
            Field::Worktree => self.worktree = !self.worktree,
            Field::Agent => {
                self.menu = AGENTS.iter().position(|agent| *agent == self.agent);
            }
            Field::Name => {}
        }
    }

    /// The dropdown's options the filter matches: none, a new ticket, the
    /// tickets by key and summary, and a typed key that is not among them, to
    /// take as it is.
    fn options(&self) -> Vec<(Jira, String, String)> {
        let filter = self.filter.trim().to_lowercase();
        let mut options = vec![
            (Jira::None, "none".to_string(), "no ticket".to_string()),
            (
                Jira::New,
                "new ticket".to_string(),
                "create one in Jira".to_string(),
            ),
        ];
        options.retain(|(_, name, _)| name.contains(&filter));
        options.extend(
            self.tickets
                .iter()
                .filter(|t| {
                    format!("{} {}", t.key, t.summary)
                        .to_lowercase()
                        .contains(&filter)
                })
                .map(|t| {
                    (
                        Jira::Existing(t.key.clone()),
                        t.key.clone(),
                        format!("{}  {}", t.status, t.summary),
                    )
                }),
        );
        let typed = filter.to_uppercase();
        if work::is_ticket_key(&typed) && !self.tickets.iter().any(|t| t.key == typed) {
            options.push((
                Jira::Existing(typed.clone()),
                typed,
                "by its key".to_string(),
            ));
        }
        options
    }

    /// Keys while the dropdown is open: ↑↓ move, ⏎ picks, Esc keeps the old
    /// choice, typing filters.
    fn on_menu_key(&mut self, code: KeyCode, at: usize) {
        if self.field == Field::Agent {
            self.menu = match code {
                KeyCode::Up | KeyCode::BackTab => Some(at.saturating_sub(1)),
                KeyCode::Down | KeyCode::Tab => Some((at + 1).min(AGENTS.len() - 1)),
                KeyCode::Enter => {
                    self.agent = AGENTS[at];
                    None
                }
                KeyCode::Esc => None,
                _ => Some(at),
            };
            return;
        }
        let count = self.options().len();
        self.menu = match code {
            KeyCode::Up | KeyCode::BackTab => Some(at.saturating_sub(1)),
            KeyCode::Down | KeyCode::Tab => Some((at + 1).min(count.saturating_sub(1))),
            KeyCode::Enter => match self.options().into_iter().nth(at) {
                Some((jira, ..)) => {
                    self.jira = jira;
                    None
                }
                None => Some(at),
            },
            KeyCode::Esc => None,
            KeyCode::Backspace => {
                self.filter.pop();
                Some(0)
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                Some(0)
            }
            _ => Some(at),
        };
    }

    fn is_toggle(&self) -> bool {
        self.field != Field::Name
    }

    /// What `wt start` gets. An existing ticket without a name names the task
    /// after its summary.
    fn args(&self) -> Result<StartArgs, String> {
        let name = self.name.trim().to_string();
        let (target, tickets) = match &self.jira {
            Jira::Existing(key) if name.is_empty() => (key.clone(), Vec::new()),
            Jira::Existing(key) => (name, vec![key.clone()]),
            _ if name.is_empty() => return Err("a name for the task".to_string()),
            _ => (name, Vec::new()),
        };
        Ok(StartArgs {
            target: Some(target),
            tickets,
            new_ticket: self.jira == Jira::New,
            no_worktree: !self.worktree,
            no_agent: self.agent.is_none(),
            ..StartArgs::default()
        })
    }
}

impl App {
    pub(super) fn open_new_task(&mut self) {
        let has_jira = self.config.jira.is_some();
        self.dialog = Some(Dialog::NewTask(Form::new(
            has_jira,
            tickets(&self.table),
            self.config.workflow.agent,
        )));
    }

    /// A refresh brings the tickets an open form picks from.
    pub(super) fn refresh_new_task(&mut self) {
        if let Some(Dialog::NewTask(form)) = &mut self.dialog {
            form.tickets = tickets(&self.table);
        }
    }

    /// ↑↓ and Tab move between fields, space flips a checkbox or opens
    /// a dropdown, typing fills the name; ⏎ starts it, Esc closes.
    pub(super) fn on_new_task_key(&mut self, key: KeyEvent, mut form: Form) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(at) = form.menu {
            form.on_menu_key(key.code, at);
            self.dialog = Some(Dialog::NewTask(form));
            return;
        }
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Enter if matches!(form.field, Field::Jira | Field::Agent) => form.flip(),
            KeyCode::Enter => match form.args() {
                Ok(args) => {
                    let target = args.target.clone().unwrap_or_default();
                    let then = if args.no_worktree && args.no_agent {
                        Then::Close
                    } else {
                        Then::Navigate
                    };
                    let agent = form.agent;
                    self.run_command(format!("start {target}"), then, move |config, ui| {
                        let mut config = config.clone();
                        if let Some(agent) = agent {
                            config.workflow.agent = agent;
                        }
                        work::start(&config, args, ui)
                    });
                    return;
                }
                Err(e) => form.error = Some(e),
            },
            KeyCode::Up | KeyCode::BackTab => form.move_field(-1),
            KeyCode::Down | KeyCode::Tab => form.move_field(1),
            KeyCode::Left | KeyCode::Right if form.is_toggle() => form.flip(),
            KeyCode::Char(' ') if form.is_toggle() => form.flip(),
            KeyCode::Backspace if form.field == Field::Name => {
                form.name.pop();
            }
            KeyCode::Char(c) if !ctrl && form.field == Field::Name => form.name.push(c),
            _ => {}
        }
        self.dialog = Some(Dialog::NewTask(form));
    }
}

/// The open dropdown: the filter, then a window of the options around the one
/// the cursor is on.
fn menu(form: &Form, at: usize, lit: Style, dim: Style) -> Vec<Line<'static>> {
    let pad = format!("{:12}", "");
    let mut lines = vec![Line::from(vec![
        Span::raw(format!("{pad}  ")),
        Span::styled("filter ", dim),
        Span::raw(format!("{}▏", form.filter)),
    ])];
    let options = form.options();
    if options.is_empty() {
        lines.push(Line::styled(
            format!("{pad}  nothing matches; type a key"),
            dim,
        ));
        return lines;
    }
    let start = at
        .saturating_sub(SHOWN - 1)
        .min(options.len().saturating_sub(SHOWN));
    for (i, (_, name, what)) in options.iter().enumerate().skip(start).take(SHOWN) {
        let (mark, style) = if i == at {
            ("› ", lit)
        } else {
            ("  ", Style::default())
        };
        lines.push(Line::from(vec![
            Span::raw(format!("{pad}{mark}")),
            Span::styled(format!("{name:<11}"), style),
            Span::styled(format!("  {}", cut(what, 60)), dim),
        ]));
    }
    if options.len() > SHOWN {
        lines.push(Line::styled(
            format!("{pad}  {} of {}", at + 1, options.len()),
            dim,
        ));
    }
    if form.tickets.is_empty() {
        lines.push(Line::styled(
            format!("{pad}  no open tickets without a task yet; type a key"),
            dim,
        ));
    }
    lines
}

fn cut(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let kept: String = text.chars().take(width - 1).collect();
    format!("{kept}…")
}

/// The keys under the form.
pub(super) fn hint(form: &Form) -> &'static str {
    if form.menu.is_some() && form.field == Field::Agent {
        "↑↓ move   ⏎ pick   Esc back"
    } else if form.menu.is_some() {
        "type to filter   ↑↓ move   ⏎ pick   Esc back"
    } else if matches!(form.field, Field::Jira | Field::Agent) {
        "↑↓ field   ⏎ space open   Esc cancel"
    } else if form.is_toggle() {
        "↑↓ field   space check   ⏎ start   Esc cancel"
    } else {
        "↑↓ field   ⏎ start   Esc cancel"
    }
}

/// The form's rows: a label, then the text or the choice; the field the
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
            Field::Name => {
                let cursor = if form.field == field { "▏" } else { "" };
                let mut spans = vec![label(field, "name")];
                if form.name.is_empty() {
                    let hint = if matches!(form.jira, Jira::Existing(_)) {
                        "the ticket's summary"
                    } else {
                        "the task's name"
                    };
                    spans.push(Span::raw(cursor));
                    spans.push(Span::styled(hint, dim));
                } else {
                    spans.push(Span::raw(format!("{}{cursor}", form.name)));
                }
                lines.push(Line::from(spans));
            }
            Field::Jira => {
                // `[ UCP-1 ▾ ] summary`, lit while the cursor is on it
                let style = if form.field == field && form.menu.is_none() {
                    lit
                } else {
                    Style::default()
                };
                let (chosen, summary) = match &form.jira {
                    Jira::None => ("none".to_string(), None),
                    Jira::New => ("new ticket".to_string(), None),
                    Jira::Existing(key) => (
                        key.clone(),
                        form.tickets
                            .iter()
                            .find(|t| t.key == *key)
                            .map(|t| t.summary.clone()),
                    ),
                };
                let mut spans = vec![
                    label(field, "jira"),
                    Span::styled(format!("[ {chosen} ▾ ]"), style),
                ];
                if let Some(summary) = summary {
                    spans.push(Span::styled(format!(" {}", cut(&summary, 40)), dim));
                }
                lines.push(Line::from(spans));
                if let (Some(at), true) = (form.menu, form.field == field) {
                    lines.extend(menu(form, at, lit, dim));
                }
            }
            Field::Worktree => lines.push(checkbox(field, form.worktree, "worktree")),
            Field::Agent => {
                let style = if form.field == field && form.menu.is_none() {
                    lit
                } else {
                    Style::default()
                };
                lines.push(Line::from(vec![
                    label(field, "agent"),
                    Span::styled(format!("[ {} ▾ ]", agent_name(form.agent)), style),
                ]));
                if let (Some(at), true) = (form.menu, form.field == field) {
                    for (i, agent) in AGENTS.iter().enumerate() {
                        lines.push(Line::from(vec![
                            Span::raw(format!("{:12}{}", "", if i == at { "› " } else { "  " })),
                            Span::styled(
                                agent_name(*agent),
                                if i == at { lit } else { Style::default() },
                            ),
                        ]));
                    }
                }
            }
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        match (form.worktree, form.agent.is_some()) {
            (true, true) => "the note, a worktree tab with an agent in the task's workspace",
            (true, false) => "the note, a worktree tab with a shell in the task's workspace",
            (false, true) => "the note, an agent in the main checkout in the task's workspace",
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

    fn ticket(key: &str, summary: &str) -> Ticket {
        Ticket {
            key: key.into(),
            status: "Open".into(),
            summary: summary.into(),
        }
    }

    #[test]
    fn agent_picker_defaults_to_project_agent_and_can_disable_launch() {
        for default in [AgentKind::Claude, AgentKind::Codex] {
            let mut form = Form::new(false, Vec::new(), default);
            form.name = "task".into();
            form.field = Field::Agent;
            form.flip();
            assert_eq!(AGENTS[form.menu.unwrap()], Some(default));
            let text = lines(&form)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            for label in ["claude", "codex", "none"] {
                assert!(text.contains(label));
            }

            for (at, agent) in AGENTS.into_iter().enumerate() {
                form.flip();
                form.on_menu_key(KeyCode::Up, form.menu.unwrap());
                form.on_menu_key(KeyCode::Up, form.menu.unwrap());
                for _ in 0..at {
                    form.on_menu_key(KeyCode::Down, form.menu.unwrap());
                }
                form.on_menu_key(KeyCode::Enter, form.menu.unwrap());
                assert_eq!(form.agent, agent);
                assert!(form.menu.is_none());
                for worktree in [true, false] {
                    form.worktree = worktree;
                    let args = form.args().unwrap();
                    assert_eq!(args.no_agent, agent.is_none());
                    assert_eq!(args.no_worktree, !worktree);
                }
            }

            form.flip();
            form.on_menu_key(KeyCode::Up, form.menu.unwrap());
            form.on_menu_key(KeyCode::Esc, form.menu.unwrap());
            assert!(form.agent.is_none());
            assert!(form.menu.is_none());
        }
    }

    #[test]
    fn an_existing_ticket_without_a_name_names_the_task() {
        let mut form = Form::new(true, Vec::new(), AgentKind::Claude);
        assert!(form.args().is_err());
        form.jira = Jira::Existing("UCP-1".into());
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
    fn none_new_and_the_tickets_are_one_list() {
        let mut form = Form::new(
            true,
            vec![ticket("UCP-1", "video feed"), ticket("UCP-2", "clips")],
            AgentKind::Claude,
        );
        let names: Vec<_> = form.options().into_iter().map(|(_, n, _)| n).collect();
        assert_eq!(names, ["none", "new ticket", "UCP-1", "UCP-2"]);

        form.name = "feed".into();
        form.field = Field::Jira;
        form.flip();
        form.on_menu_key(KeyCode::Down, form.menu.unwrap());
        form.on_menu_key(KeyCode::Enter, form.menu.unwrap());
        assert_eq!(form.jira, Jira::New);
        assert!(form.args().unwrap().new_ticket);
    }

    #[test]
    fn a_ticket_is_picked_by_filter_or_typed() {
        let mut form = Form::new(
            true,
            vec![ticket("UCP-1", "video feed"), ticket("UCP-2", "clips")],
            AgentKind::Claude,
        );
        form.field = Field::Jira;
        form.flip();
        for c in "clip".chars() {
            form.on_menu_key(KeyCode::Char(c), form.menu.unwrap());
        }
        form.on_menu_key(KeyCode::Enter, form.menu.unwrap());
        assert_eq!(form.jira, Jira::Existing("UCP-2".into()));

        form.flip();
        for c in "ucp-7".chars() {
            form.on_menu_key(KeyCode::Char(c), form.menu.unwrap());
        }
        let names: Vec<_> = form.options().into_iter().map(|(_, n, _)| n).collect();
        assert_eq!(names, ["UCP-7"]);
        form.on_menu_key(KeyCode::Enter, form.menu.unwrap());
        assert_eq!(form.jira, Jira::Existing("UCP-7".into()));
    }
}
