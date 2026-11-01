//! Global preferences. Add new settings to ITEMS and handle their value here.

use super::App;
use super::dialog::Dialog;
use crate::client::agent::AgentKind;
use crate::config::{self, Config, Settings};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::sync::Arc;

#[derive(Clone, Copy)]
enum Setting {
    DefaultAgent,
}

const ITEMS: &[Setting] = &[Setting::DefaultAgent];
pub(super) const BUTTON: &str = "[ Settings ]";
pub(super) const HINT: &str = "↑↓ move   ←→ change   ⏎ save   Esc cancel";

pub(super) struct Form {
    selected: usize,
    settings: Settings,
    error: Option<String>,
}

impl Form {
    fn new(settings: Settings) -> Self {
        Self {
            selected: 0,
            settings,
            error: None,
        }
    }

    fn change(&mut self) {
        match ITEMS[self.selected] {
            Setting::DefaultAgent => {
                self.settings.default_agent = match self.settings.default_agent {
                    AgentKind::Claude => AgentKind::Codex,
                    AgentKind::Codex => AgentKind::Claude,
                };
            }
        }
        self.error = None;
    }
}

/// Keep command-line overrides while applying the freshly saved project defaults.
fn apply_defaults(projects: &mut [Arc<Config>], saved: &[Config]) {
    for project in projects {
        if project.workflow.agent_override.is_none()
            && let Some(config) = saved.iter().find(|c| c.name == project.name)
        {
            Arc::make_mut(project).workflow.agent = config.workflow.agent;
        }
    }
}

impl App {
    pub(super) fn open_settings(&mut self) {
        match config::load_settings() {
            Ok(settings) => self.dialog = Some(Dialog::Settings(Form::new(settings))),
            Err(error) => self.say(format!("! {error}")),
        }
    }

    pub(super) fn on_settings_key(&mut self, key: KeyEvent, mut form: Form) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q' | ',') => return,
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                form.selected = form.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                form.selected = (form.selected + 1).min(ITEMS.len() - 1);
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h' | 'l' | ' ') => form.change(),
            KeyCode::Enter => match config::save_settings(form.settings) {
                Ok(saved) => {
                    apply_defaults(&mut self.projects, &saved);
                    self.config = self.projects[self.project].clone();
                    self.say(format!(
                        "settings saved · default agent: {}",
                        form.settings.default_agent
                    ));
                    return;
                }
                Err(error) => form.error = Some(error),
            },
            _ => {}
        }
        self.dialog = Some(Dialog::Settings(form));
    }
}

pub(super) fn lines(form: &Form) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (i, setting) in ITEMS.iter().enumerate() {
        let (label, value) = match setting {
            Setting::DefaultAgent => ("Default agent", form.settings.default_agent.binary()),
        };
        lines.push(Line::from(vec![
            Span::raw(if form.selected == i { "› " } else { "  " }),
            Span::raw(format!("{label}   ‹ ")),
            Span::styled(value, Style::default().fg(Color::Yellow)),
            Span::raw(" ›"),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::raw("Used for new conversations in all projects."));
    lines.push(Line::raw("Project overrides and --agent take precedence."));
    lines.push(Line::raw("Existing sessions keep their own agent."));
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

    fn project(name: &str, agent: AgentKind, cli: Option<AgentKind>) -> Config {
        Config {
            name: name.into(),
            jira: None,
            gitlab: None,
            workflow: WorkflowConfig {
                agent,
                agent_override: cli,
                ..WorkflowConfig::default()
            },
            vault: VaultConfig {
                path: "/v".into(),
                task_dirs: vec![],
                new_task_dir: "tasks".into(),
            },
            repos: vec![],
        }
    }

    #[test]
    fn saving_defaults_updates_all_projects_but_keeps_project_and_cli_overrides() {
        let mut projects: Vec<_> = [
            project("shared", AgentKind::Claude, None),
            project("own", AgentKind::Claude, None),
            project("cli", AgentKind::Claude, Some(AgentKind::Claude)),
        ]
        .into_iter()
        .map(Arc::new)
        .collect();
        let in_flight = projects[0].clone();
        let saved = [
            project("shared", AgentKind::Codex, None),
            project("own", AgentKind::Claude, None),
            project("cli", AgentKind::Codex, None),
        ];
        apply_defaults(&mut projects, &saved);
        assert_eq!(projects[0].workflow.agent, AgentKind::Codex);
        assert_eq!(projects[1].workflow.agent, AgentKind::Claude);
        assert_eq!(projects[2].workflow.agent, AgentKind::Claude);
        assert_eq!(in_flight.workflow.agent, AgentKind::Claude);
    }
}
