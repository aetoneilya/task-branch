use crate::client::gitlab::MergeRequest;
use crate::client::jira::{JiraIssue, JiraSubtask};
use crate::core::JiraGitlabState;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap,
};
use ratatui::{DefaultTerminal, Frame};
use std::io::stdout;
use std::time::Duration;

pub fn run(state: &JiraGitlabState) -> Result<(), String> {
    let mut app = App::new(state);
    app.refresh();

    enable_raw_mode().map_err(|err| err.to_string())?;
    execute!(stdout(), EnterAlternateScreen).map_err(|err| err.to_string())?;
    let mut terminal = ratatui::init();

    let result = run_app(&mut terminal, &mut app);

    ratatui::restore();
    disable_raw_mode().map_err(|err| err.to_string())?;
    execute!(stdout(), LeaveAlternateScreen).map_err(|err| err.to_string())?;

    result
}

fn run_app(terminal: &mut DefaultTerminal, app: &mut App<'_>) -> Result<(), String> {
    loop {
        terminal
            .draw(|frame| render(frame, app))
            .map_err(|err| err.to_string())?;

        if !event::poll(Duration::from_millis(100)).map_err(|err| err.to_string())? {
            continue;
        }

        let Event::Key(key) = event::read().map_err(|err| err.to_string())? else {
            continue;
        };

        if key.kind != KeyEventKind::Press {
            continue;
        }

        if app.handle_key(key.code)? {
            return Ok(());
        }
    }
}

struct App<'a> {
    state: &'a JiraGitlabState,
    tab: Tab,
    issues: Vec<JiraIssue>,
    mrs: Vec<MergeRequest>,
    issue_state: ListState,
    mr_state: ListState,
    status: String,
    popup: Option<Popup>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Dashboard,
    Issues,
    MergeRequests,
}

struct Popup {
    title: String,
    body: String,
}

impl<'a> App<'a> {
    fn new(state: &'a JiraGitlabState) -> Self {
        let mut issue_state = ListState::default();
        issue_state.select(Some(0));
        let mut mr_state = ListState::default();
        mr_state.select(Some(0));

        Self {
            state,
            tab: Tab::Dashboard,
            issues: Vec::new(),
            mrs: Vec::new(),
            issue_state,
            mr_state,
            status: "Loading Jira and GitLab data…".to_string(),
            popup: None,
        }
    }

    fn refresh(&mut self) {
        let issues_result = self.state.unresolved_current_user_issues();
        let mrs_result = self.state.list_merge_request();

        match issues_result {
            Ok(issues) => {
                self.issues = issues;
                if self.issues.is_empty() {
                    self.issue_state.select(None);
                } else {
                    let idx = self
                        .issue_state
                        .selected()
                        .unwrap_or(0)
                        .min(self.issues.len().saturating_sub(1));
                    self.issue_state.select(Some(idx));
                }
            }
            Err(err) => {
                self.issues.clear();
                self.issue_state.select(None);
                self.status = format!("Failed to load Jira issues: {err}");
            }
        }

        match mrs_result {
            Ok(mrs) => {
                self.mrs = mrs;
                if self.mrs.is_empty() {
                    self.mr_state.select(None);
                } else {
                    let idx = self
                        .mr_state
                        .selected()
                        .unwrap_or(0)
                        .min(self.mrs.len().saturating_sub(1));
                    self.mr_state.select(Some(idx));
                }
            }
            Err(err) => {
                self.mrs.clear();
                self.mr_state.select(None);
                self.status = format!("Failed to load merge requests: {err}");
            }
        }

        if self.popup.is_none() && !self.status.starts_with("Failed to load") {
            self.status = format!(
                "{} issues, {} open merge requests. `r` refreshes.",
                self.issues.len(),
                self.mrs.len()
            );
        }
    }

    fn handle_key(&mut self, code: KeyCode) -> Result<bool, String> {
        if self.popup.is_some() {
            match code {
                KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') => self.popup = None,
                _ => {}
            }
            return Ok(false);
        }

        match code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => self.next_tab(),
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => self.prev_tab(),
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Down | KeyCode::Char('j') => self.next_item(),
            KeyCode::Up | KeyCode::Char('k') => self.prev_item(),
            KeyCode::Char('b') => self.create_branch_for_selected_issue()?,
            KeyCode::Char('m') => self.create_mr_for_selected_issue()?,
            KeyCode::Char('s') => self.switch_to_selected_mr()?,
            KeyCode::Enter => self.handle_enter()?,
            _ => {}
        }

        Ok(false)
    }

    fn next_tab(&mut self) {
        self.tab = match self.tab {
            Tab::Dashboard => Tab::Issues,
            Tab::Issues => Tab::MergeRequests,
            Tab::MergeRequests => Tab::Dashboard,
        };
    }

    fn prev_tab(&mut self) {
        self.tab = match self.tab {
            Tab::Dashboard => Tab::MergeRequests,
            Tab::Issues => Tab::Dashboard,
            Tab::MergeRequests => Tab::Issues,
        };
    }

    fn next_item(&mut self) {
        match self.tab {
            Tab::Issues => move_selection(&mut self.issue_state, self.issues.len(), 1),
            Tab::MergeRequests => move_selection(&mut self.mr_state, self.mrs.len(), 1),
            Tab::Dashboard => {}
        }
    }

    fn prev_item(&mut self) {
        match self.tab {
            Tab::Issues => move_selection(&mut self.issue_state, self.issues.len(), -1),
            Tab::MergeRequests => move_selection(&mut self.mr_state, self.mrs.len(), -1),
            Tab::Dashboard => {}
        }
    }

    fn handle_enter(&mut self) -> Result<(), String> {
        match self.tab {
            Tab::Dashboard => {
                self.tab = if !self.issues.is_empty() {
                    Tab::Issues
                } else {
                    Tab::MergeRequests
                };
            }
            Tab::Issues => self.show_selected_issue(),
            Tab::MergeRequests => self.switch_to_selected_mr()?,
        }

        Ok(())
    }

    fn selected_issue(&self) -> Option<&JiraIssue> {
        self.issue_state
            .selected()
            .and_then(|idx| self.issues.get(idx))
    }

    fn selected_mr(&self) -> Option<&MergeRequest> {
        self.mr_state.selected().and_then(|idx| self.mrs.get(idx))
    }

    fn show_selected_issue(&mut self) {
        if let Some(issue) = self.selected_issue() {
            let mut lines = vec![
                format!("Status: {}", issue.status()),
                String::new(),
                issue.fields.summary.clone(),
            ];

            if let Some(description) = &issue.fields.description {
                lines.push(String::new());
                lines.push(description.clone());
            }

            if let Some(subtasks) = &issue.fields.subtasks {
                lines.push(String::new());
                lines.push("Subtasks:".to_string());
                for subtask in subtasks {
                    lines.push(format!(
                        "- {} [{}] {}",
                        subtask.key,
                        subtask.status(),
                        subtask.fields.summary
                    ));
                }
            }

            self.popup = Some(Popup {
                title: issue.key.clone(),
                body: lines.join("\n"),
            });
        }
    }

    fn create_branch_for_selected_issue(&mut self) -> Result<(), String> {
        let issue = self
            .selected_issue()
            .ok_or_else(|| "No Jira issue selected".to_string())?;
        let branch = format!("{}-{}", issue.key, slugify(&issue.fields.summary));

        if self
            .state
            .local_branch_exists(&branch)
            .map_err(|err| err.to_string())?
        {
            self.state
                .checkout_to(&branch)
                .map_err(|err| err.to_string())?;
            self.popup = Some(Popup {
                title: "Branch Ready".to_string(),
                body: format!("Checked out existing branch:\n\n{branch}"),
            });
            return Ok(());
        }

        self.state
            .create_branch(&branch)
            .map_err(|err| err.to_string())?;
        self.popup = Some(Popup {
            title: "Branch Created".to_string(),
            body: format!("Created and checked out:\n\n{branch}"),
        });
        Ok(())
    }

    fn create_mr_for_selected_issue(&mut self) -> Result<(), String> {
        let issue = self
            .selected_issue()
            .ok_or_else(|| "No Jira issue selected".to_string())?;
        let title = format!("{} {}", issue.key, issue.fields.summary);
        let description = issue.fields.description.clone();

        let mr = self
            .state
            .create_merge_request(title, description)
            .map_err(|err| err.to_string())?;

        self.refresh();
        self.popup = Some(Popup {
            title: "Merge Request Created".to_string(),
            body: format!(
                "{}\n\nSource branch: {}\nURL: {}",
                mr.title(),
                mr.source_branch(),
                mr.web_url()
            ),
        });
        Ok(())
    }

    fn switch_to_selected_mr(&mut self) -> Result<(), String> {
        let mr = self
            .selected_mr()
            .ok_or_else(|| "No merge request selected".to_string())?;
        self.state
            .checkout_to(mr.source_branch())
            .map_err(|err| err.to_string())?;
        self.popup = Some(Popup {
            title: "Branch Switched".to_string(),
            body: format!("Checked out:\n\n{}", mr.source_branch()),
        });
        Ok(())
    }
}

fn move_selection(state: &mut ListState, len: usize, delta: isize) {
    if len == 0 {
        state.select(None);
        return;
    }

    let current = state.selected().unwrap_or(0) as isize;
    let next = (current + delta).clamp(0, len.saturating_sub(1) as isize) as usize;
    state.select(Some(next));
}

fn render(frame: &mut Frame, app: &mut App<'_>) {
    let area = frame.area();
    let shell = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(area);

    render_header(frame, shell[0], app);
    match app.tab {
        Tab::Dashboard => render_dashboard(frame, shell[1], app),
        Tab::Issues => render_issues(frame, shell[1], app),
        Tab::MergeRequests => render_mrs(frame, shell[1], app),
    }
    render_footer(frame, shell[2], app);

    if let Some(popup) = &app.popup {
        render_popup(frame, popup);
    }
}

fn render_header(frame: &mut Frame, area: Rect, app: &App<'_>) {
    let titles = ["Dashboard", "Issues", "Merge Requests"];
    let selected = match app.tab {
        Tab::Dashboard => 0,
        Tab::Issues => 1,
        Tab::MergeRequests => 2,
    };

    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" jg ")
                .border_type(BorderType::Rounded),
        )
        .select(selected)
        .style(Style::default().fg(Color::Gray))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .divider("  ");

    frame.render_widget(tabs, area);
}

fn render_dashboard(frame: &mut Frame, area: Rect, app: &App<'_>) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Length(8),
            Constraint::Min(0),
        ])
        .split(area);

    let stats = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[0]);

    frame.render_widget(
        stat_card("Open Issues", app.issues.len(), Color::Yellow),
        stats[0],
    );
    frame.render_widget(stat_card("Open MRs", app.mrs.len(), Color::Green), stats[1]);

    let shortcuts = Paragraph::new(Text::from(vec![
        Line::from(vec![
            Span::styled(
                "Enter",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" open current section   "),
            Span::styled(
                "Tab",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" switch section"),
        ]),
        Line::from(vec![
            Span::styled(
                "r",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" refresh data   "),
            Span::styled(
                "q",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" quit"),
        ]),
        Line::from(vec![
            Span::styled(
                "b",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" create branch from selected issue   "),
            Span::styled(
                "m",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" create MR"),
        ]),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Shortcuts "),
    )
    .wrap(Wrap { trim: true });
    frame.render_widget(shortcuts, chunks[1]);

    let preview = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(chunks[2]);

    let issue_items: Vec<ListItem> = app
        .issues
        .iter()
        .take(8)
        .map(|issue| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    issue.key.as_str(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("  {}", issue.fields.summary)),
            ]))
        })
        .collect();
    let issues = List::new(issue_items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Current Issues "),
    );
    frame.render_widget(issues, preview[0]);

    let mr_items: Vec<ListItem> = app
        .mrs
        .iter()
        .take(8)
        .map(|mr| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    mr.state(),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("  {}", mr.title())),
            ]))
        })
        .collect();
    let mrs = List::new(mr_items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Merge Requests "),
    );
    frame.render_widget(mrs, preview[1]);
}

fn render_issues(frame: &mut Frame, area: Rect, app: &mut App<'_>) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(area);

    let items: Vec<ListItem> = app
        .issues
        .iter()
        .map(|issue| {
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        issue.key.as_str(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("  [{}]", issue.status())),
                ]),
                Line::from(Span::styled(
                    issue.fields.summary.as_str(),
                    Style::default().fg(Color::White),
                )),
            ])
        })
        .collect();

    let issue_list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(" Jira Issues "),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(28, 44, 64))
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    frame.render_stateful_widget(issue_list, columns[0], &mut app.issue_state);

    let detail = app
        .selected_issue()
        .map(issue_detail_text)
        .unwrap_or_else(|| "No unresolved Jira issues found.".to_string());
    let detail_widget = Paragraph::new(detail)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(" Details "),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(detail_widget, columns[1]);
}

fn render_mrs(frame: &mut Frame, area: Rect, app: &mut App<'_>) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(area);

    let items: Vec<ListItem> = app
        .mrs
        .iter()
        .map(|mr| {
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        mr.state(),
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::styled(mr.source_branch(), Style::default().fg(Color::Cyan)),
                ]),
                Line::from(Span::raw(mr.title())),
            ])
        })
        .collect();

    let mr_list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(" Open Merge Requests "),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(25, 52, 40))
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    frame.render_stateful_widget(mr_list, columns[0], &mut app.mr_state);

    let detail = app
        .selected_mr()
        .map(mr_detail_text)
        .unwrap_or_else(|| "No open merge requests for the current author.".to_string());
    let detail_widget = Paragraph::new(detail)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(" Details "),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(detail_widget, columns[1]);
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App<'_>) {
    let help = match app.tab {
        Tab::Dashboard => "Tab switch section  Enter open  r refresh  q quit",
        Tab::Issues => "j/k move  Enter inspect  b branch  m MR  r refresh  q quit",
        Tab::MergeRequests => "j/k move  Enter or s checkout  r refresh  q quit",
    };

    let footer = Paragraph::new(Line::from(vec![
        Span::styled("Status: ", Style::default().fg(Color::DarkGray)),
        Span::styled(app.status.as_str(), Style::default().fg(Color::White)),
        Span::raw("    "),
        Span::styled(help, Style::default().fg(Color::Cyan)),
    ]))
    .alignment(Alignment::Left)
    .block(Block::default().borders(Borders::TOP));

    frame.render_widget(footer, area);
}

fn render_popup(frame: &mut Frame, popup: &Popup) {
    let area = centered_rect(68, 62, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(format!(" {} ", popup.title))
        .style(Style::default().bg(Color::Rgb(16, 20, 28)).fg(Color::White));
    let body = Paragraph::new(popup.body.as_str())
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(body, area);
}

fn stat_card<'a>(title: &'a str, value: usize, color: Color) -> Paragraph<'a> {
    Paragraph::new(Text::from(vec![
        Line::from(Span::styled(
            value.to_string(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(title, Style::default().fg(Color::Gray))),
    ]))
    .alignment(Alignment::Center)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded),
    )
}

fn issue_detail_text(issue: &JiraIssue) -> String {
    let mut lines = vec![
        format!("Key: {}", issue.key),
        format!("Status: {}", issue.status()),
        String::new(),
        issue.fields.summary.clone(),
    ];

    if let Some(assignee) = &issue.fields.assignee {
        lines.push(String::new());
        lines.push(format!("Assignee: {}", assignee.display_name));
    }

    if let Some(description) = &issue.fields.description {
        lines.push(String::new());
        lines.push("Description:".to_string());
        lines.push(description.clone());
    }

    if let Some(subtasks) = &issue.fields.subtasks {
        if !subtasks.is_empty() {
            lines.push(String::new());
            lines.push("Subtasks:".to_string());
            for subtask in subtasks {
                lines.push(format_subtask(subtask));
            }
        }
    }

    lines.join("\n")
}

fn mr_detail_text(mr: &MergeRequest) -> String {
    let mut lines = vec![
        format!("State: {}", mr.state()),
        format!("Branch: {}", mr.source_branch()),
        String::new(),
        mr.title().to_string(),
        String::new(),
        "Open in browser:".to_string(),
        mr.web_url().to_string(),
    ];

    if let Some(task) = mr.extract_task() {
        lines.push(String::new());
        lines.push(format!("Detected Jira key: {task}"));
    }

    lines.join("\n")
}

fn format_subtask(subtask: &JiraSubtask) -> String {
    format!(
        "- {} [{}] {}",
        subtask.key,
        subtask.status(),
        subtask.fields.summary
    )
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn slugify(text: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;

    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }

    slug.trim_matches('-').to_string()
}
