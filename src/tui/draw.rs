//! Drawing: the task tree, prompt line, footer hints, help.

use super::{
    App, MESSAGE_FOR, PREVIEW_MIN_WIDTH, Prompt, anim,
    rows::{self, AgentAt, Kind},
};
use crate::table::view::Section;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, Paragraph, Wrap};

impl App {
    pub(super) fn draw(&mut self, frame: &mut Frame) {
        let show_prompt = self.prompt.is_some() || !self.filter.is_empty();
        let [header, prompt, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(show_prompt as u16),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        self.preview_shown = self.preview.unwrap_or(body.width >= PREVIEW_MIN_WIDTH);
        let [left, right] = if self.preview_shown {
            Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(body)
        } else {
            Layout::horizontal([Constraint::Percentage(100), Constraint::Length(0)]).areas(body)
        };

        frame.render_widget(self.header(), header);
        if show_prompt {
            let (label, text) = match &self.prompt {
                Some((Prompt::Filter, text)) => ("/ ".to_string(), text.clone()),
                Some((Prompt::AddTicket(task), text)) => {
                    (format!("ticket for \"{task}\": "), text.clone())
                }
                Some((Prompt::NoteLine(_), text)) => ("note: ".to_string(), text.clone()),
                None => ("/ ".to_string(), self.filter.clone()),
            };
            let cursor = if self.prompt.is_some() { "▏" } else { "" };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(label, Style::default().fg(Color::Yellow)),
                    Span::raw(format!("{text}{cursor}")),
                ])),
                prompt,
            );
        }

        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|r| {
                let mut line = r.line.clone();
                // a working agent's spinner moves: its status is the row's last span
                if let Kind::Agent(at, a) = r.kind
                    && let Some(last) = line.spans.last_mut()
                {
                    *last = rows::agent_status_span(self.table.agent(at, a));
                }
                ListItem::new(line)
            })
            .collect();
        let mut title = if self.all { " all " } else { " active " }.to_string();
        if self.hide.any() {
            title.push_str(&format!("· hidden: {} ", super::hide::summary(self.hide)));
        }
        let list = List::new(items)
            .block(
                Block::bordered()
                    .title(title)
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .highlight_style(if self.projects_focused {
                // the keys are on the dropdown: the selection only stays in sight
                Style::default().bg(Color::Rgb(36, 39, 45))
            } else {
                Style::default()
                    .bg(Color::Rgb(50, 54, 62))
                    .add_modifier(Modifier::BOLD)
            });
        frame.render_stateful_widget(list, left, &mut self.list);
        (self.list_area, self.details_area) = (left, right);

        let details = Paragraph::new(self.details_lines())
            .block(Block::bordered().border_style(Style::default().fg(Color::DarkGray)))
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0));
        if self.preview_shown {
            frame.render_widget(details, right);
        }

        let footer_line = match &self.message {
            Some((text, at)) if at.elapsed() < MESSAGE_FOR => Line::from(Span::styled(
                text.clone(),
                Style::default().fg(Color::Yellow),
            )),
            _ => self.hints(),
        };
        frame.render_widget(Paragraph::new(footer_line), footer);

        self.draw_picker(frame);
        if self.help {
            self.draw_help(frame);
        }
        self.draw_dialog(frame);
    }

    /// Keys that do something on the selected row, then the ones that always do.
    pub(super) fn hints(&self) -> Line<'static> {
        let mut keys: Vec<(&str, String)> = Vec::new();
        if self.picker.is_some() {
            keys.extend([
                ("type", "filter".into()),
                ("↑↓", "move".into()),
                ("⏎", "pick".into()),
                ("Esc", "close".into()),
            ]);
        } else if self.projects_focused {
            keys.extend([
                ("⏎", "projects".into()),
                ("Tab", "next project".into()),
                ("↓", "back to the list".into()),
            ]);
        } else if let Some(row) = self.current() {
            let under_task = matches!(row.root, Kind::Task(_));
            let has_url = self.url().is_some();
            match &row.kind {
                Kind::Task(_) => {
                    keys.push(("⏎", "open".into()));
                    keys.push(("s", "add ticket".into()));
                    keys.push(("c", "sessions".into()));
                    keys.push(("d", "close task".into()));
                }
                Kind::Loose(i) if self.table.loose[*i].state.is_none() => {
                    keys.push(("⏎", "start task".into()));
                    keys.push(("a", "attach to task".into()));
                    keys.push(("b", "jira".into()));
                }
                Kind::Loose(_) => {
                    keys.push(("⏎", "open".into()));
                    keys.push(("a", "attach to task".into()));
                    keys.push(("b", "jira".into()));
                    keys.push(("s", "start".into()));
                    keys.push(("c", "sessions".into()));
                }
                Kind::Ticket(_) => {
                    keys.push(("⏎", "open".into()));
                    keys.push(("b", "jira".into()));
                    keys.push(("s", "start".into()));
                }
                Kind::Mr(_) => keys.push(("⏎", "open MR".into())),
                Kind::Header(_) => {}
                Kind::NewTask => keys.push(("⏎", "new task".into())),
                Kind::Tree(t) => {
                    let tree = self.table.tree(*t);
                    if !tree.agents.is_empty() {
                        keys.push(("⏎", "agent".into()));
                    } else if self.target().is_some() {
                        keys.push(("⏎", "resume claude".into()));
                    }
                    if has_url {
                        keys.push(("b", if tree.mr.is_some() { "MR" } else { "jira" }.into()));
                    }
                    keys.push(("R", "review".into()));
                    keys.push(("d", "done".into()));
                }
                Kind::Agent(at, _) => {
                    keys.push(("⏎", "focus".into()));
                    if has_url {
                        keys.push(("b", "browser".into()));
                    }
                    if let AgentAt::Tree(_) = at {
                        keys.push(("R", "review".into()));
                        keys.push(("d", "done".into()));
                    }
                }
            }
            if self.ide_tree().is_some() {
                keys.push(("i", "idea".into()));
            }
            if under_task {
                keys.push(("N", "note line".into()));
                keys.push(("o", "obsidian".into()));
                keys.push(("e", "edit note".into()));
            }
            if row.has_children {
                let folded = self.folded.contains(&row.id);
                keys.push(("space", if folded { "unfold" } else { "fold" }.into()));
            }
        }
        let key_style = Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD);
        let mut spans = Vec::new();
        for (key, label) in keys {
            spans.push(Span::styled(format!("{key} "), key_style));
            spans.push(Span::raw(format!("{label}  ")));
        }
        // the open dropdown takes every key
        if self.picker.is_some() {
            return Line::from(spans);
        }
        spans.push(Span::styled("│ ", Style::default().fg(Color::DarkGray)));
        let all = if self.all { "active" } else { "all" };
        if !self.waiting().is_empty() {
            spans.push(Span::styled("w ", key_style));
            spans.push(Span::raw("next waiting  "));
        }
        let preview = if self.preview_shown {
            "hide details"
        } else {
            "details"
        };
        for (key, label) in [
            ("/", "filter"),
            ("z", "fold all"),
            (".", all),
            ("v", "show"),
            ("p", preview),
            ("r", "refresh"),
            ("?", "help"),
            ("q", "quit"),
        ] {
            spans.push(Span::styled(
                format!("{key} "),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                format!("{label}  "),
                Style::default().fg(Color::DarkGray),
            ));
        }
        Line::from(spans)
    }

    fn header(&self) -> Paragraph<'static> {
        // roots per section: tasks until the first title, then each section
        let mut counts = [0usize; 3];
        let mut section = 0;
        for row in self.rows.iter().filter(|r| r.parent.is_none()) {
            match row.kind {
                Kind::Header(Section::NoTask) => section = 1,
                Kind::Header(Section::Assigned) => section = 2,
                Kind::NewTask => {}
                _ => counts[section] += 1,
            }
        }
        let mut spans = vec![self.dropdown()];
        spans.extend([
            Span::styled(
                if self.config.jira.is_some() {
                    format!(
                        "  {} task(s) · {} without a task · {} assigned to me",
                        counts[0], counts[1], counts[2]
                    )
                } else {
                    format!("  {} task(s) · {} without a task", counts[0], counts[1])
                },
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("   "),
        ]);
        let waiting = self.waiting().len();
        if waiting > 0 {
            spans.push(Span::styled(
                format!("⚑ {waiting} waiting   "),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ));
        }
        spans.push(if self.refreshing {
            let elapsed = self.last_refresh.elapsed();
            // filled against the last refresh, never quite full; unknown at first
            let bar = match self.refresh_took {
                Some(took) => anim::bar(
                    0.95_f64.min(elapsed.as_secs_f64() / took.as_secs_f64().max(0.1)),
                    10,
                ),
                None => anim::bounce(10),
            };
            Span::styled(
                format!(
                    "{} refreshing {bar} {}s",
                    anim::spinner(),
                    elapsed.as_secs()
                ),
                Style::default().fg(Color::Yellow),
            )
        } else if let Some(synced) = self.synced {
            Span::styled(
                format!("synced {}", synced.format("%H:%M")),
                Style::default().fg(Color::DarkGray),
            )
        } else {
            Span::raw("")
        });
        let remote = self.config.jira.is_some() || self.config.gitlab.is_some();
        if !self.online && remote {
            let cached = !self.table.tickets.is_empty() || !self.table.mrs.is_empty();
            spans.push(Span::styled(
                if cached {
                    "  cached"
                } else {
                    "  without Jira and GitLab"
                },
                Style::default().fg(Color::DarkGray),
            ));
        }
        Paragraph::new(Line::from(spans))
    }

    fn draw_help(&self, frame: &mut Frame) {
        let text = vec![
            "↑↓ j k     move             ← h  fold / to parent        → l  unfold        space  fold",
            "z          fold all / unfold all",
            "↑ at top   project dropdown: ⏎ opens it, ⏎ on \"+ new project\" adds one   Tab ] [  next / previous project",
            "⏎          agent: focus; worktree: its agent or claude --resume; task: wt select",
            "b          ticket / MR in the browser   o  note in Obsidian       e  note in $EDITOR",
            "i          worktree in IntelliJ IDEA ([workflow] ide); on a task: its only worktree",
            "s          ticket: wt start KEY; task: add a ticket (wt add)",
            "c          pick a session (wt select --session); ⏎ on \"+ new task\": wt start",
            "a          ticket without a task: attach it to a task (wt attach)",
            "N          add a dated line to the task note",
            "R          review worktree (wt review)  d  worktree: remove it; task: close it with its Jira tickets",
            "/          filter, Esc clears           .  all / active only      r  refresh",
            "v          show / hide tickets, MRs, worktrees with agents",
            "mouse      wheel scrolls the list or the details, click selects, again folds",
            "w          next agent waiting for you (blocked, or done and unseen)",
            "p          show / hide details (hidden by default under 110 columns)",
            "ctrl-d/u   scroll details               ctrl-l  redraw            q  quit",
        ];
        let area = frame.area();
        let height = text.len() as u16 + 2;
        let popup = ratatui::layout::Rect {
            x: area.x + 2,
            y: area.height.saturating_sub(height) / 2,
            width: area.width.saturating_sub(4),
            height,
        };
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(text.into_iter().map(Line::from).collect::<Vec<_>>())
                .block(Block::bordered().title(" keys ")),
            popup,
        );
    }
}
