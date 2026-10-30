//! The project dropdown on the header's first row: ↑ from the top of the list
//! gets to it, ⏎ opens it over the list. Its last option, "+ new project",
//! opens a form that adds a `[[project]]` to the config.

use super::App;
use super::dialog::Dialog;
use crate::config::{self, Config};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use std::path::Path;
use std::sync::Arc;

/// The open dropdown.
pub(super) struct Picker {
    /// The option the cursor is on.
    at: usize,
    /// What is typed to filter the projects.
    filter: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Choice {
    /// Its index in `projects`.
    Project(usize),
    New,
}

/// The projects whose name has `filter` in it, then "+ new project".
fn options(projects: &[Arc<Config>], filter: &str) -> Vec<Choice> {
    let filter = filter.trim().to_lowercase();
    let mut options: Vec<Choice> = projects
        .iter()
        .enumerate()
        .filter(|(_, p)| p.name.to_lowercase().contains(&filter))
        .map(|(i, _)| Choice::Project(i))
        .collect();
    options.push(Choice::New);
    options
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    TaskDir,
    Repo,
}

const FIELDS: [Field; 3] = [Field::Name, Field::TaskDir, Field::Repo];

/// "+ new project": its name, the vault folder of its task notes, its repo.
pub(super) struct Form {
    name: String,
    task_dir: String,
    repo: String,
    field: Field,
    /// The folder the shown project's notes are in: an empty task dir is the
    /// new project's name next to it.
    beside: Option<String>,
    /// Why the last ⏎ did not add it.
    error: Option<String>,
}

impl Form {
    fn new(shown: &Config) -> Form {
        let beside = shown
            .vault
            .new_task_dir
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_string_lossy().into_owned());
        Form {
            name: String::new(),
            task_dir: String::new(),
            repo: String::new(),
            field: Field::Name,
            beside,
            error: None,
        }
    }

    fn move_field(&mut self, delta: isize) {
        let at = FIELDS.iter().position(|f| *f == self.field).unwrap_or(0) as isize;
        self.field = FIELDS[(at + delta).clamp(0, FIELDS.len() as isize - 1) as usize];
    }

    fn text(&mut self) -> &mut String {
        match self.field {
            Field::Name => &mut self.name,
            Field::TaskDir => &mut self.task_dir,
            Field::Repo => &mut self.repo,
        }
    }

    /// The task dir when the field is left empty.
    fn default_task_dir(&self) -> Option<String> {
        let name = self.name.trim();
        let beside = self.beside.as_ref()?;
        (!name.is_empty()).then(|| format!("{beside}/{name}"))
    }

    /// The name, the task dir and the repo the config gets; why not, if not.
    fn check(&self, projects: &[Arc<Config>]) -> Result<(String, String, Option<String>), String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("a name for the project".into());
        }
        if projects.iter().any(|p| p.name.eq_ignore_ascii_case(name)) {
            return Err(format!("there is a project {name} already"));
        }
        let task_dir = match self.task_dir.trim() {
            "" => self
                .default_task_dir()
                .ok_or("a folder in the vault for the task notes")?,
            dir => dir.to_string(),
        };
        if Path::new(&task_dir).is_absolute() {
            return Err("the task dir is a folder inside the vault: a relative path".into());
        }
        let repo = match self.repo.trim() {
            "" => None,
            repo if !config::expand_home(Path::new(repo)).is_dir() => {
                return Err(format!("no directory {repo}"));
            }
            repo => Some(repo.to_string()),
        };
        Ok((name.to_string(), task_dir, repo))
    }
}

impl App {
    /// ↑ on the list's top row: the dropdown takes the keys.
    pub(super) fn focus_projects(&mut self) {
        self.projects_focused = true;
    }

    fn open_picker(&mut self) {
        self.projects_focused = true;
        self.picker = Some(Picker {
            at: self.project,
            filter: String::new(),
        });
    }

    /// Keys while the dropdown has the focus, closed: ⏎ space → open it, ↓ Esc
    /// go back to the list, Tab and [ ] switch at once. Any other key goes back
    /// to the list too, and does there what it does; false then.
    pub(super) fn on_projects_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Right | KeyCode::Char('l') => {
                self.open_picker()
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Esc => self.projects_focused = false,
            KeyCode::Up | KeyCode::Char('k') => {}
            KeyCode::Char(']') | KeyCode::Tab => self.step_project(1),
            KeyCode::Char('[') | KeyCode::BackTab => self.step_project(-1),
            _ => {
                self.projects_focused = false;
                return false;
            }
        }
        true
    }

    /// Keys while the dropdown is open: ↑↓ move, ⏎ picks, Esc closes it,
    /// typing filters.
    pub(super) fn on_picker_key(&mut self, key: KeyEvent, mut picker: Picker) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let count = options(&self.projects, &picker.filter).len();
        match key.code {
            KeyCode::Up | KeyCode::BackTab => picker.at = picker.at.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => picker.at = (picker.at + 1).min(count - 1),
            KeyCode::Enter => {
                if let Some(choice) = options(&self.projects, &picker.filter).get(picker.at) {
                    self.pick(*choice);
                }
                return;
            }
            KeyCode::Esc => return,
            KeyCode::Backspace => {
                picker.filter.pop();
                picker.at = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                picker.filter.push(c);
                picker.at = 0;
            }
            _ => {}
        }
        self.picker = Some(picker);
    }

    fn pick(&mut self, choice: Choice) {
        self.picker = None;
        match choice {
            Choice::Project(i) => {
                self.projects_focused = false;
                self.switch_project(i);
            }
            Choice::New => self.dialog = Some(Dialog::NewProject(Form::new(&self.config))),
        }
    }

    /// The dropdown's closed face: `[ ucp ▾ ]`, lit while it has the focus.
    pub(super) fn dropdown(&self) -> Span<'static> {
        let style = if self.projects_focused {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };
        Span::styled(format!("[ {} ▾ ]", self.config.name), style)
    }

    /// A click on the header: on the dropdown, it opens or closes.
    pub(super) fn click_header(&mut self, column: u16) {
        if column >= self.dropdown().width() as u16 {
            return;
        }
        if self.picker.take().is_none() {
            self.open_picker();
        }
    }

    /// A click while the dropdown is open: an option picks it, anywhere else
    /// closes it.
    pub(super) fn click_picker(&mut self, column: u16, row: u16) {
        let Some(picker) = self.picker.take() else {
            return;
        };
        let area = self.picker_area;
        let first = area.y + 1 + !picker.filter.is_empty() as u16;
        if column <= area.x || column + 1 >= area.x + area.width || row < first {
            return;
        }
        if let Some(choice) = options(&self.projects, &picker.filter).get((row - first) as usize) {
            self.pick(*choice);
        }
    }

    /// The open dropdown, over the list under the header.
    pub(super) fn draw_picker(&mut self, frame: &mut Frame) {
        let Some(picker) = &self.picker else {
            return;
        };
        let lit = Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD);
        let dim = Style::default().fg(Color::DarkGray);
        let mut lines = Vec::new();
        if !picker.filter.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("filter ", dim),
                Span::raw(format!("{}▏", picker.filter)),
            ]));
        }
        for (i, choice) in options(&self.projects, &picker.filter).iter().enumerate() {
            let on = i == picker.at;
            let (text, style) = match choice {
                Choice::Project(p) => (self.projects[*p].name.clone(), Style::default()),
                Choice::New => (
                    "+ new project".to_string(),
                    Style::default().fg(Color::Green),
                ),
            };
            let current = *choice == Choice::Project(self.project);
            lines.push(Line::from(vec![
                Span::raw(if on { "› " } else { "  " }),
                Span::styled(text, if on { lit } else { style }),
                Span::styled(if current { " ✓" } else { "" }, dim),
            ]));
        }
        let area = frame.area();
        let width = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4;
        let popup = Rect {
            x: area.x,
            y: area.y + 1,
            width: width.max(20).min(area.width),
            height: (lines.len() as u16 + 2).min(area.height.saturating_sub(1)),
        };
        self.picker_area = popup;
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .title(" projects ")
                    .border_style(Style::default().fg(Color::Yellow)),
            ),
            popup,
        );
    }

    /// ↑↓ and Tab move between fields, typing fills the one on; ⏎ adds the
    /// project and shows it, Esc closes.
    pub(super) fn on_new_project_key(&mut self, key: KeyEvent, mut form: Form) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Enter => {
                match form
                    .check(&self.projects)
                    .and_then(|(name, dir, repo)| config::add_project(&name, &dir, repo.as_deref()))
                {
                    Ok(mut config) => {
                        if let Some(agent) = self.config.workflow.agent_override {
                            config.workflow.agent = agent;
                            config.workflow.agent_override = Some(agent);
                        }
                        let name = config.name.clone();
                        self.projects.push(Arc::new(config));
                        self.projects_focused = false;
                        self.switch_project(self.projects.len() - 1);
                        self.say(format!("added project {name} to the config"));
                        return;
                    }
                    Err(e) => form.error = Some(e),
                }
            }
            KeyCode::Up | KeyCode::BackTab => form.move_field(-1),
            KeyCode::Down | KeyCode::Tab => form.move_field(1),
            KeyCode::Backspace => {
                form.text().pop();
            }
            KeyCode::Char(c) if !ctrl => form.text().push(c),
            _ => {}
        }
        self.dialog = Some(Dialog::NewProject(form));
    }
}

pub(super) const HINT: &str = "↑↓ field   ⏎ add   Esc cancel";

/// The form's rows: a label, then the text or what an empty field means.
pub(super) fn lines(form: &Form) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let default_dir = form.default_task_dir();
    let mut lines = Vec::new();
    for field in FIELDS {
        let on = form.field == field;
        let (label, text, empty) = match field {
            Field::Name => ("name", &form.name, "the project's name"),
            Field::TaskDir => (
                "task dir",
                &form.task_dir,
                default_dir.as_deref().unwrap_or("a folder in the vault"),
            ),
            Field::Repo => ("repo", &form.repo, "none; e.g. ~/code/pet-projects/foo"),
        };
        let mut spans = vec![Span::styled(
            format!("{}{label:<10}", if on { "› " } else { "  " }),
            if on {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            },
        )];
        let cursor = if on { "▏" } else { "" };
        if text.is_empty() {
            spans.push(Span::raw(cursor));
            spans.push(Span::styled(empty.to_string(), dim));
        } else {
            spans.push(Span::raw(format!("{text}{cursor}")));
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "goes at the end of the config; Jira and GitLab are added there by hand",
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
    use crate::config::{VaultConfig, WorkflowConfig};
    use std::path::PathBuf;

    fn project(name: &str, notes: &str) -> Arc<Config> {
        Arc::new(Config {
            name: name.into(),
            jira: None,
            gitlab: None,
            workflow: WorkflowConfig::default(),
            vault: VaultConfig {
                path: PathBuf::from("/v"),
                task_dirs: vec![PathBuf::from(notes)],
                new_task_dir: PathBuf::from(notes),
            },
            repos: Vec::new(),
        })
    }

    #[test]
    fn the_filter_keeps_new_project_last() {
        let projects = [project("ucp", "30/tasks"), project("vpn", "10/vpn")];
        assert_eq!(
            options(&projects, ""),
            [Choice::Project(0), Choice::Project(1), Choice::New]
        );
        assert_eq!(options(&projects, "VP"), [Choice::Project(1), Choice::New]);
        assert_eq!(options(&projects, "zzz"), [Choice::New]);
    }

    #[test]
    fn an_empty_task_dir_is_the_name_next_to_the_shown_project() {
        let projects = [project("vpn", "10/projects/vpn")];
        let mut form = Form::new(&projects[0]);
        assert!(form.check(&projects).is_err());
        form.name = "VPN".into();
        assert!(form.check(&projects).unwrap_err().contains("already"));
        form.name = "kcal".into();
        assert_eq!(
            form.check(&projects),
            Ok(("kcal".into(), "10/projects/kcal".into(), None))
        );
        form.task_dir = "/abs".into();
        assert!(form.check(&projects).is_err());
        form.task_dir = "20/kcal".into();
        form.repo = "/no/such/dir".into();
        assert!(form.check(&projects).unwrap_err().contains("no directory"));
    }
}
