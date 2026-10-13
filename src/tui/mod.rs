//! `wt` without a command: the task tree on the left, details of the selection on
//! the right. The offline table (worktrees, agents, notes) renders at once; Jira
//! and GitLab fill in from a background refresh, repeated every minute.
//! Actions run the same `wt` commands; interactive ones get the terminal for a while.

mod rows;

use crate::client::herdr;
use crate::config::Config;
use crate::table::{self, Table};
use crate::vault::task::load_tasks;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use rows::{Kind, Row, clean};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

const REFRESH_EVERY: Duration = Duration::from_secs(60);
const MESSAGE_FOR: Duration = Duration::from_secs(4);
/// Narrower than this, the details pane is hidden unless `p` asks for it.
const PREVIEW_MIN_WIDTH: u16 = 110;

pub fn run(config: Config) -> Result<(), String> {
    let mut app = App::new(config)?;
    let mut terminal = ratatui::init();
    let result = app.run(&mut terminal);
    ratatui::restore();
    result
}

enum Prompt {
    Filter,
    /// A ticket key to add to the task.
    AddTicket(String),
    NewTask,
    /// y/n before `wt start <key>`: it creates a note, a worktree and an agent.
    StartTicket(String),
}

enum Update {
    Table(Box<Table>, Vec<String>),
    /// Lazily loaded details, by key.
    Detail(String, String),
    Message(String),
}

struct App {
    config: Arc<Config>,
    table: Table,
    online: bool,
    rows: Vec<Row>,
    list: ListState,
    selected: Option<String>,
    folded: HashSet<String>,
    all: bool,
    filter: String,
    prompt: Option<(Prompt, String)>,
    /// Shown in the footer for a few seconds instead of the key hints.
    message: Option<(String, Instant)>,
    help: bool,
    /// `p` overrides the width rule: Some(true) shows the details, Some(false) hides.
    preview: Option<bool>,
    /// Whether the last frame had the details pane: details load only then.
    preview_shown: bool,
    refreshing: bool,
    last_refresh: Instant,
    synced: Option<chrono::DateTime<chrono::Local>>,
    details: HashMap<String, String>,
    pending: HashSet<String>,
    detail_scroll: u16,
    tx: Sender<Update>,
    rx: Receiver<Update>,
    quit: bool,
}

impl App {
    fn new(config: Config) -> Result<App, String> {
        let vault = config
            .vault
            .as_ref()
            .ok_or("no [vault] section in ~/.config/worktable/config.toml")?;
        let (tasks, _) = load_tasks(vault);
        let table = table::build(&config, tasks, false);
        let (tx, rx) = channel();
        let mut app = App {
            config: Arc::new(config),
            table,
            online: false,
            rows: Vec::new(),
            list: ListState::default(),
            selected: None,
            folded: HashSet::new(),
            all: false,
            filter: String::new(),
            prompt: None,
            message: None,
            help: false,
            preview: None,
            preview_shown: true,
            refreshing: false,
            last_refresh: Instant::now(),
            synced: None,
            details: HashMap::new(),
            pending: HashSet::new(),
            detail_scroll: 0,
            tx,
            rx,
            quit: false,
        };
        app.rebuild();
        app.refresh();
        Ok(app)
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<(), String> {
        while !self.quit {
            self.load_details();
            terminal.draw(|f| self.draw(f)).map_err(|e| e.to_string())?;
            if event::poll(Duration::from_millis(200)).map_err(|e| e.to_string())? {
                if let Event::Key(key) = event::read().map_err(|e| e.to_string())? {
                    if key.kind == KeyEventKind::Press {
                        self.on_key(key, terminal);
                    }
                }
            }
            while let Ok(update) = self.rx.try_recv() {
                self.apply(update);
            }
            if self.last_refresh.elapsed() > REFRESH_EVERY {
                self.refresh();
            }
        }
        Ok(())
    }

    // ---- data

    /// Rebuilds the table with Jira and GitLab in the background and writes task
    /// states and sessions back, as `wt status` does.
    fn refresh(&mut self) {
        if self.refreshing {
            return;
        }
        self.refreshing = true;
        self.last_refresh = Instant::now();
        let (config, tx) = (self.config.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let Some(vault) = config.vault.as_ref() else {
                return;
            };
            let (tasks, _) = load_tasks(vault);
            let mut table = table::build(&config, tasks, true);
            let updated = table::write_back(&mut table);
            let _ = tx.send(Update::Table(Box::new(table), updated));
        });
    }

    fn apply(&mut self, update: Update) {
        match update {
            Update::Table(table, updated) => {
                self.table = *table;
                self.online = true;
                self.refreshing = false;
                self.synced = Some(chrono::Local::now());
                self.details.clear();
                if let Some(warning) = self.table.warnings.first() {
                    self.say(format!("! {warning}"));
                } else if !updated.is_empty() {
                    self.say(format!("updated notes: {}", updated.join(", ")));
                }
                self.rebuild();
            }
            Update::Detail(key, text) => {
                self.pending.remove(&key);
                self.details.insert(key, text);
            }
            Update::Message(text) => self.say(text),
        }
    }

    /// Rows from the table, keeping the selection on the same item.
    fn rebuild(&mut self) {
        self.rows = rows::build(&self.table, self.all, &self.filter, &self.folded);
        let index = self
            .selected
            .as_ref()
            .and_then(|id| self.rows.iter().position(|r| &r.id == id))
            .unwrap_or_else(|| {
                self.list
                    .selected()
                    .unwrap_or(0)
                    .min(self.rows.len().saturating_sub(1))
            });
        self.select(index);
    }

    /// Moves by `delta` rows, stepping over section titles.
    fn step(&mut self, delta: isize) {
        let mut index = self.list.selected().unwrap_or(0) as isize;
        loop {
            index += delta;
            if index < 0 || index as usize >= self.rows.len() {
                return;
            }
            if self.rows[index as usize].kind != Kind::Header {
                self.select(index as usize);
                return;
            }
        }
    }

    fn select(&mut self, index: usize) {
        if self.rows.is_empty() {
            self.list.select(None);
            self.selected = None;
            return;
        }
        let mut index = index.min(self.rows.len() - 1);
        // a section title is never selected: the next row, or the previous one
        if let Some(i) = (index..self.rows.len())
            .chain((0..index).rev())
            .find(|&i| self.rows[i].kind != Kind::Header)
        {
            index = i;
        }
        let id = self.rows[index].id.clone();
        if self.selected.as_ref() != Some(&id) {
            self.detail_scroll = 0;
            // agent output is live: read it again every time
            if let Kind::Agent(..) = self.rows[index].kind {
                if let Some(key) = self.detail_key(&self.rows[index].kind) {
                    self.details.remove(&key);
                }
            }
        }
        self.list.select(Some(index));
        self.selected = Some(id);
    }

    fn say(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), Instant::now()));
    }

    fn current(&self) -> Option<&Row> {
        self.list.selected().and_then(|i| self.rows.get(i))
    }

    /// The slow part of a row's details, loaded on selection.
    fn detail_key(&self, kind: &Kind) -> Option<String> {
        match kind {
            Kind::Loose(i) => Some(format!("desc:{}", self.table.loose[*i].key)),
            Kind::Ticket(key) => Some(format!("desc:{key}")),
            Kind::Tree(t) => Some(format!("git:{}", self.table.tree(*t).wt.path.display())),
            Kind::Agent(t, a) => Some(format!("agent:{}", agent_name(&self.table, *t, *a))),
            _ => None,
        }
    }

    fn load_details(&mut self) {
        if !self.preview_shown {
            return;
        }
        let Some(key) = self.current().and_then(|r| self.detail_key(&r.kind)) else {
            return;
        };
        if self.details.contains_key(&key) || self.pending.contains(&key) {
            return;
        }
        self.pending.insert(key.clone());
        let (config, tx) = (self.config.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let text = load_detail(&config, &key);
            let _ = tx.send(Update::Detail(key, text));
        });
    }

    // ---- keys

    fn on_key(&mut self, key: KeyEvent, terminal: &mut DefaultTerminal) {
        if self.prompt.is_some() {
            self.on_prompt_key(key, terminal);
            return;
        }
        if self.help {
            self.help = false;
            return;
        }
        self.message = None;
        let selected = self.list.selected().unwrap_or(0);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.rebuild();
            }
            KeyCode::Esc => self.quit = true,
            KeyCode::Char('l') if ctrl => {
                let _ = terminal.clear();
            }
            KeyCode::Char('d') if ctrl => self.detail_scroll = self.detail_scroll.saturating_add(10),
            KeyCode::Char('u') if ctrl => self.detail_scroll = self.detail_scroll.saturating_sub(10),
            KeyCode::Down | KeyCode::Char('j') => self.step(1),
            KeyCode::Up | KeyCode::Char('k') => self.step(-1),
            KeyCode::PageDown => self.select(selected + 15),
            KeyCode::PageUp => self.select(selected.saturating_sub(15)),
            KeyCode::Home | KeyCode::Char('g') => self.select(0),
            KeyCode::End | KeyCode::Char('G') => self.select(usize::MAX),
            KeyCode::Char(' ') => self.toggle_fold(),
            KeyCode::Left | KeyCode::Char('h') => self.fold_or_parent(),
            KeyCode::Right | KeyCode::Char('l') => self.unfold(),
            KeyCode::Char('/') => self.prompt = Some((Prompt::Filter, self.filter.clone())),
            KeyCode::Char('.') => {
                self.all = !self.all;
                self.say(if self.all { "showing everything" } else { "active work only" });
                self.rebuild();
            }
            KeyCode::Char('r') => {
                self.refresh();
                self.say("refreshing…");
            }
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('p') => self.preview = Some(!self.preview_shown),
            KeyCode::Enter => self.go(terminal),
            KeyCode::Char('b') => self.open_in_browser(),
            KeyCode::Char('o') => self.open_note_in_obsidian(),
            KeyCode::Char('e') => self.edit_note(terminal),
            KeyCode::Char('s') => self.start(terminal),
            KeyCode::Char('n') => self.prompt = Some((Prompt::NewTask, String::new())),
            KeyCode::Char('c') => {
                if let Some(target) = self.target() {
                    self.run_wt(terminal, &["select", &target, "--session"]);
                }
            }
            KeyCode::Char('R') => {
                if let Some(t) = self.tree() {
                    let name = self.table.tree(t).wt.name();
                    self.run_wt(terminal, &["review", &name]);
                }
            }
            KeyCode::Char('d') => {
                if let Some(t) = self.tree() {
                    let name = self.table.tree(t).wt.name();
                    self.run_wt(terminal, &["done", &name]);
                } else {
                    self.say("d works on a worktree");
                }
            }
            _ => {}
        }
    }

    fn on_prompt_key(&mut self, key: KeyEvent, terminal: &mut DefaultTerminal) {
        let Some((prompt, mut text)) = self.prompt.take() else {
            return;
        };
        if let Prompt::StartTicket(ticket) = &prompt {
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Enter) {
                let ticket = ticket.clone();
                self.run_wt(terminal, &["start", &ticket]);
            }
            return;
        }
        match key.code {
            KeyCode::Esc => {
                if let Prompt::Filter = prompt {
                    self.filter.clear();
                    self.rebuild();
                }
                return;
            }
            KeyCode::Enter => {
                match prompt {
                    Prompt::Filter => {}
                    Prompt::AddTicket(task) => {
                        let key = text.trim().to_uppercase();
                        if !key.is_empty() {
                            self.run_wt(terminal, &["add", &task, &key]);
                        }
                    }
                    Prompt::StartTicket(_) => {}
                    Prompt::NewTask => {
                        if !text.trim().is_empty() {
                            self.run_wt(terminal, &["start", text.trim()]);
                        }
                    }
                }
                return;
            }
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Char(c) => text.push(c),
            _ => {}
        }
        if let Prompt::Filter = prompt {
            self.filter = text.clone();
            self.rebuild();
        }
        self.prompt = Some((prompt, text));
    }

    fn toggle_fold(&mut self) {
        let Some(row) = self.current() else { return };
        if !row.has_children && !self.folded.contains(&row.id) {
            return;
        }
        let id = row.id.clone();
        if !self.folded.remove(&id) {
            self.folded.insert(id);
        }
        self.rebuild();
    }

    fn fold_or_parent(&mut self) {
        let Some(row) = self.current() else { return };
        if row.has_children && !self.folded.contains(&row.id) {
            let id = row.id.clone();
            self.folded.insert(id);
            self.rebuild();
        } else if let Some(parent) = row.parent {
            self.select(parent);
        }
    }

    fn unfold(&mut self) {
        let Some(row) = self.current() else { return };
        let id = row.id.clone();
        if self.folded.remove(&id) {
            self.rebuild();
        }
    }

    // ---- what the selection refers to

    /// The task name or loose ticket key `wt select` takes for the selection.
    fn target(&self) -> Option<String> {
        match &self.current()?.root {
            Kind::Task(i) => Some(self.table.tasks[*i].task.name.clone()),
            Kind::Loose(i) => Some(self.table.loose[*i].key.clone()),
            _ => None,
        }
    }

    fn note(&self) -> Option<PathBuf> {
        match &self.current()?.root {
            Kind::Task(i) => Some(self.table.tasks[*i].task.note.path.clone()),
            _ => None,
        }
    }

    fn tree(&self) -> Option<usize> {
        match self.current()?.kind {
            Kind::Tree(t) | Kind::Agent(t, _) => Some(t),
            _ => None,
        }
    }

    fn url(&self) -> Option<String> {
        let table = &self.table;
        match &self.current()?.kind {
            Kind::Ticket(key) => Some(table.ticket_url(key)),
            Kind::Loose(i) => Some(table.ticket_url(&table.loose[*i].key)),
            Kind::Mr(m) => Some(table.mr(*m).mr.web_url().to_string()),
            Kind::Tree(t) | Kind::Agent(t, _) => {
                let tree = table.tree(*t);
                match (tree.mr, tree.keys.first()) {
                    (Some(m), _) => Some(table.mr(m).mr.web_url().to_string()),
                    (None, Some(key)) => Some(table.ticket_url(key)),
                    _ => None,
                }
            }
            Kind::Task(_) | Kind::Header => None,
        }
    }

    // ---- actions

    /// ⏎: to the agent of the selection — focus a live one, otherwise `wt select`
    /// resumes or starts claude there; an MR opens in the browser.
    fn go(&mut self, terminal: &mut DefaultTerminal) {
        let Some(row) = self.current() else { return };
        match row.kind.clone() {
            Kind::Agent(t, a) => self.focus(&agent_name(&self.table, t, a)),
            Kind::Tree(t) => {
                let tree = self.table.tree(t);
                if !tree.agents.is_empty() {
                    self.focus(&agent_name(&self.table, t, 0));
                    return;
                }
                let Some(target) = self.target() else {
                    self.say("worktree without a ticket: wt add to attach it");
                    return;
                };
                let args = vec![
                    "select".to_string(),
                    target,
                    "--tree".to_string(),
                    tree.wt.name(),
                ];
                self.say(format!("opening {}…", tree.wt.name()));
                self.run_wt_background(args);
            }
            Kind::Mr(_) => self.open_in_browser(),
            Kind::Ticket(key) => self.run_wt(terminal, &["select", &key]),
            Kind::Header => {}
            Kind::Loose(i) if !self.loose_has_work(i) => {
                let key = self.table.loose[i].key.clone();
                self.prompt = Some((Prompt::StartTicket(key), String::new()));
            }
            Kind::Task(_) | Kind::Loose(_) => {
                if let Some(target) = self.target() {
                    self.run_wt(terminal, &["select", &target]);
                }
            }
        }
    }

    /// A worktree or an open MR: something `wt select` can get back to.
    fn loose_has_work(&self, i: usize) -> bool {
        let loose = &self.table.loose[i];
        loose.trees.iter().any(|&t| !self.table.tree(t).finished)
            || loose.mrs.iter().any(|&m| self.table.mr(m).opened())
    }

    fn focus(&mut self, agent: &str) {
        let text = match herdr::focus_agent(agent) {
            Ok(()) => format!("→ {agent}"),
            Err(e) => format!("! {e}"),
        };
        self.say(text);
    }

    fn open_in_browser(&mut self) {
        match self.url() {
            Some(url) => open(&url),
            None => self.say("nothing to open in the browser"),
        }
    }

    fn open_note_in_obsidian(&mut self) {
        match self.note() {
            Some(path) => open(&format!(
                "obsidian://open?path={}",
                urlencoding::encode(&path.to_string_lossy())
            )),
            None => self.say("no note here"),
        }
    }

    fn edit_note(&mut self, terminal: &mut DefaultTerminal) {
        let Some(path) = self.note() else {
            self.say("no note here");
            return;
        };
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
        ratatui::restore();
        let _ = Command::new("sh")
            .arg("-c")
            .arg(format!("{editor} \"$0\""))
            .arg(&path)
            .status();
        *terminal = ratatui::init();
        self.refresh();
    }

    /// `s`: a ticket gets its worktree (`wt start KEY` adds it to its task); on a
    /// task, asks which ticket to add.
    fn start(&mut self, terminal: &mut DefaultTerminal) {
        let Some(row) = self.current() else { return };
        match row.kind.clone() {
            Kind::Ticket(key) => self.run_wt(terminal, &["start", &key]),
            Kind::Loose(i) => {
                let key = self.table.loose[i].key.clone();
                self.run_wt(terminal, &["start", &key]);
            }
            _ => match row.root.clone() {
                Kind::Task(i) => {
                    let task = self.table.tasks[i].task.name.clone();
                    self.prompt = Some((Prompt::AddTicket(task), String::new()));
                }
                _ => self.say("s works on a task or a ticket"),
            },
        }
    }

    /// Runs `wt <args>` with the terminal handed over: commands that may ask.
    fn run_wt(&mut self, terminal: &mut DefaultTerminal, args: &[&str]) {
        let Ok(exe) = std::env::current_exe() else { return };
        ratatui::restore();
        println!("$ wt {}\n", args.join(" "));
        let ok = Command::new(exe)
            .args(args)
            .status()
            .is_ok_and(|s| s.success());
        println!("\n⏎ back to wt");
        let _ = std::io::stdin().read_line(&mut String::new());
        *terminal = ratatui::init();
        self.say(format!("wt {}: {}", args.join(" "), if ok { "done" } else { "failed" }));
        self.refresh();
    }

    /// Runs `wt <args>` that asks nothing, reporting its last line.
    fn run_wt_background(&mut self, args: Vec<String>) {
        let Ok(exe) = std::env::current_exe() else { return };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let text = match Command::new(exe).args(&args).output() {
                Ok(out) => {
                    let stream = if out.status.success() { out.stdout } else { out.stderr };
                    String::from_utf8_lossy(&stream)
                        .lines()
                        .last()
                        .unwrap_or("")
                        .to_string()
                }
                Err(e) => format!("! {e}"),
            };
            let _ = tx.send(Update::Message(text));
        });
    }

    // ---- drawing

    fn draw(&mut self, frame: &mut Frame) {
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
                Some((Prompt::AddTicket(task), text)) => (format!("ticket for \"{task}\": "), text.clone()),
                Some((Prompt::NewTask, text)) => ("new task: ".to_string(), text.clone()),
                Some((Prompt::StartTicket(key), _)) => {
                    let summary = self.table.tickets.get(key).map(|t| clean(&t.summary)).unwrap_or_default();
                    (format!("start a task from {key} «{summary}»? "), "y / n".to_string())
                }
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

        let items: Vec<ListItem> = self.rows.iter().map(|r| ListItem::new(r.line.clone())).collect();
        let title = if self.all { " all " } else { " active " };
        let list = List::new(items)
            .block(Block::bordered().title(title).border_style(Style::default().fg(Color::DarkGray)))
            .highlight_style(Style::default().bg(Color::Rgb(50, 54, 62)).add_modifier(Modifier::BOLD));
        frame.render_stateful_widget(list, left, &mut self.list);

        let details = Paragraph::new(self.details_lines())
            .block(Block::bordered().border_style(Style::default().fg(Color::DarkGray)))
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0));
        if self.preview_shown {
            frame.render_widget(details, right);
        }

        let footer_line = match &self.message {
            Some((text, at)) if at.elapsed() < MESSAGE_FOR => {
                Line::from(Span::styled(text.clone(), Style::default().fg(Color::Yellow)))
            }
            _ => self.hints(),
        };
        frame.render_widget(Paragraph::new(footer_line), footer);

        if self.help {
            self.draw_help(frame);
        }
    }

    /// Keys that do something on the selected row, then the ones that always do.
    fn hints(&self) -> Line<'static> {
        let mut keys: Vec<(&str, String)> = Vec::new();
        if let Some(row) = self.current() {
            let under_task = matches!(row.root, Kind::Task(_));
            let has_url = self.url().is_some();
            match &row.kind {
                Kind::Task(_) => {
                    keys.push(("⏎", "open".into()));
                    keys.push(("s", "add ticket".into()));
                    keys.push(("c", "sessions".into()));
                }
                Kind::Loose(i) if !self.loose_has_work(*i) => {
                    keys.push(("⏎", "start task".into()));
                    keys.push(("b", "jira".into()));
                }
                Kind::Loose(_) => {
                    keys.push(("⏎", "open".into()));
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
                Kind::Header => {}
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
                Kind::Agent(..) => {
                    keys.push(("⏎", "focus".into()));
                    if has_url {
                        keys.push(("b", "browser".into()));
                    }
                    keys.push(("R", "review".into()));
                    keys.push(("d", "done".into()));
                }
            }
            if under_task {
                keys.push(("o", "obsidian".into()));
                keys.push(("e", "edit note".into()));
            }
            if row.has_children {
                let folded = self.folded.contains(&row.id);
                keys.push(("space", if folded { "unfold" } else { "fold" }.into()));
            }
        }
        let key_style = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);
        let mut spans = Vec::new();
        for (key, label) in keys {
            spans.push(Span::styled(format!("{key} "), key_style));
            spans.push(Span::raw(format!("{label}  ")));
        }
        spans.push(Span::styled("│ ", Style::default().fg(Color::DarkGray)));
        let all = if self.all { "active" } else { "all" };
        let preview = if self.preview_shown { "hide details" } else { "details" };
        for (key, label) in [("n", "new task"), ("/", "filter"), (".", all), ("p", preview), ("r", "refresh"), ("?", "help"), ("q", "quit")] {
            spans.push(Span::styled(format!("{key} "), Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD)));
            spans.push(Span::styled(format!("{label}  "), Style::default().fg(Color::DarkGray)));
        }
        Line::from(spans)
    }

    fn header(&self) -> Paragraph<'static> {
        // roots per section: tasks until the first title, then each section
        let mut counts = [0usize; 3];
        let mut section = 0;
        for row in self.rows.iter().filter(|r| r.parent.is_none()) {
            match row.id.as_str() {
                "header:no task" => section = 1,
                "header:assigned to me in Jira" => section = 2,
                _ => counts[section] += 1,
            }
        }
        let mut spans = vec![
            Span::styled(" worktable ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(
                format!(
                    " {} task(s) · {} without a task · {} assigned to me",
                    counts[0], counts[1], counts[2]
                ),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("   "),
        ];
        spans.push(if self.refreshing {
            Span::styled("⟳ refreshing…", Style::default().fg(Color::Yellow))
        } else if let Some(synced) = self.synced {
            Span::styled(
                format!("synced {}", synced.format("%H:%M")),
                Style::default().fg(Color::DarkGray),
            )
        } else {
            Span::raw("")
        });
        if !self.online {
            spans.push(Span::styled(
                "  without Jira and GitLab",
                Style::default().fg(Color::DarkGray),
            ));
        }
        Paragraph::new(Line::from(spans))
    }

    fn draw_help(&self, frame: &mut Frame) {
        let text = vec![
            "↑↓ j k     move             ← h  fold / to parent        → l  unfold        space  fold",
            "⏎          agent: focus; worktree: its agent or claude --resume; task: wt select",
            "b          ticket / MR in the browser   o  note in Obsidian       e  note in $EDITOR",
            "s          ticket: wt start KEY; task: add a ticket (wt add)",
            "n          new task (wt start)          c  pick a session (wt select --session)",
            "R          review worktree (wt review)  d  remove worktree (wt done)",
            "/          filter, Esc clears           .  all / active only      r  refresh",
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

    fn details_lines(&self) -> Vec<Line<'static>> {
        let Some(row) = self.current() else {
            return vec![Line::from("no active work — . shows everything")];
        };
        let table = &self.table;
        let bold = |s: String| Span::styled(s, Style::default().add_modifier(Modifier::BOLD));
        let dim = |s: String| Span::styled(s, Style::default().fg(Color::DarkGray));
        let lazy = |key: Option<String>| -> Vec<Line<'static>> {
            match key.and_then(|k| self.details.get(&k).cloned()) {
                Some(text) => text.lines().map(|l| Line::from(clean(l))).collect(),
                None => vec![Line::from(dim("loading…".to_string()))],
            }
        };
        let mut lines: Vec<Line<'static>> = Vec::new();
        match &row.kind {
            Kind::Task(i) => {
                let row = &table.tasks[*i];
                let state = rows::task_state(table, *i);
                lines.push(Line::from(vec![rows::state_mark(&state), bold(row.task.name.clone())]));
                let mut meta = vec![dim(state)];
                if !row.task.tags.is_empty() {
                    meta.push(dim(format!("  #{}", row.task.tags.join(" #"))));
                }
                lines.push(Line::from(meta));
                lines.push(Line::from(dim(display_path(&row.task.note.path))));
                for key in row.task.tickets.iter().chain(&row.found_tickets) {
                    let mut spans = vec![Span::styled(key.clone(), Style::default().fg(Color::Cyan))];
                    if let Some(t) = table.tickets.get(key) {
                        spans.push(Span::styled(
                            format!(" {}", t.status),
                            Style::default().fg(rows::status_color(&t.category)),
                        ));
                        spans.push(Span::raw(format!(" {}", clean(&t.summary))));
                    }
                    lines.push(Line::from(spans));
                }
                lines.push(Line::from(""));
                lines.extend(row.task.note.body().lines().map(|l| Line::from(clean(l))));
            }
            Kind::Loose(_) | Kind::Ticket(_) => {
                let key = match &row.kind {
                    Kind::Loose(i) => table.loose[*i].key.clone(),
                    Kind::Ticket(key) => key.clone(),
                    _ => unreachable!(),
                };
                match table.tickets.get(&key) {
                    Some(t) => {
                        lines.push(Line::from(vec![
                            Span::styled(key.clone(), Style::default().fg(Color::Cyan)),
                            bold(format!(" {}", clean(&t.summary))),
                        ]));
                        lines.push(Line::from(Span::styled(
                            t.status.clone(),
                            Style::default().fg(rows::status_color(&t.category)),
                        )));
                    }
                    None => lines.push(Line::from(Span::styled(key.clone(), Style::default().fg(Color::Cyan)))),
                }
                lines.push(Line::from(dim(table.ticket_url(&key))));
                lines.push(Line::from(""));
                lines.extend(lazy(self.detail_key(&row.kind)));
            }
            Kind::Mr(m) => {
                let mr = &table.mr(*m);
                lines.push(Line::from(vec![
                    Span::styled(format!("!{}", mr.mr.iid()), Style::default().fg(Color::Blue)),
                    bold(format!(" {}", clean(mr.mr.title()))),
                ]));
                lines.push(Line::from(dim(format!("{} · {}", mr.mr.state(), mr.mr.source_branch()))));
                if !mr.keys.is_empty() {
                    lines.push(Line::from(dim(mr.keys.join(" "))));
                }
                lines.push(Line::from(dim(mr.mr.web_url().to_string())));
            }
            Kind::Tree(t) => {
                let tree = table.tree(*t);
                lines.push(Line::from(bold(format!("⌂ {}", tree.wt.name()))));
                lines.push(Line::from(dim(display_path(&tree.wt.path))));
                lines.push(Line::from(dim(tree.wt.branch.clone().unwrap_or_default())));
                let mut state = Vec::new();
                if let Some(c) = tree.changes {
                    state.push(Span::styled(format!("changed {c}"), Style::default().fg(Color::Yellow)));
                }
                if let Some(u) = tree.unpushed {
                    state.push(Span::styled(format!("  unpushed {u}"), Style::default().fg(Color::Magenta)));
                }
                if let Some(m) = tree.mr {
                    state.push(Span::styled(
                        format!("  !{} {}", table.mr(m).mr.iid(), table.mr(m).mr.state()),
                        Style::default().fg(Color::Blue),
                    ));
                }
                lines.push(Line::from(state));
                for label in &tree.agents {
                    let (name, status) = label.rsplit_once(':').unwrap_or((label, ""));
                    lines.push(Line::from(vec![
                        Span::raw(format!("◉ {name} ")),
                        Span::styled(status.to_string(), Style::default().fg(rows::agent_color(status))),
                    ]));
                }
                lines.push(Line::from(""));
                lines.extend(lazy(self.detail_key(&row.kind)));
            }
            Kind::Header => {}
            Kind::Agent(t, a) => {
                let label = &table.tree(*t).agents[*a];
                let (name, status) = label.rsplit_once(':').unwrap_or((label, ""));
                lines.push(Line::from(vec![
                    bold(format!("◉ {name} ")),
                    Span::styled(status.to_string(), Style::default().fg(rows::agent_color(status))),
                ]));
                lines.push(Line::from(dim(format!("in {}", table.tree(*t).wt.name()))));
                lines.push(Line::from(""));
                lines.extend(lazy(self.detail_key(&row.kind)));
            }
        }
        lines
    }
}

/// The herdr name of a worktree's agent: its label without the status.
fn agent_name(table: &Table, tree: usize, agent: usize) -> String {
    let label = &table.tree(tree).agents[agent];
    label.rsplit_once(':').map_or(label.as_str(), |(n, _)| n).to_string()
}

fn display_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    match dirs::home_dir() {
        Some(home) => path.replacen(&*home.to_string_lossy(), "~", 1),
        None => path.to_string(),
    }
}

fn open(target: &str) {
    let _ = Command::new("open").arg(target).spawn();
}

/// Ticket description, worktree's git status and unpushed commits, or the
/// agent's recent screen.
fn load_detail(config: &Config, key: &str) -> String {
    if let Some(ticket) = key.strip_prefix("desc:") {
        return match table::jira_client(config).and_then(|c| {
            c.get_issue_by_key(ticket).map_err(|e| e.to_string())
        }) {
            Ok(issue) => issue
                .fields
                .description
                .unwrap_or_else(|| "(no description)".to_string()),
            Err(e) => format!("! {e}"),
        };
    }
    if let Some(path) = key.strip_prefix("git:") {
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
                .unwrap_or_default()
        };
        let status = git(&["status", "--short"]);
        let branch = git(&["branch", "--show-current"]);
        // branches are created --no-track: unpushed means not in origin/<branch>
        let unpushed = match git(&["rev-parse", "--verify", "-q", &format!("origin/{branch}")]) {
            remote if remote.is_empty() => "branch not pushed".to_string(),
            _ => git(&["log", "--oneline", "-15", &format!("origin/{branch}..HEAD")]),
        };
        let commits = git(&["log", "--oneline", "-15", "origin/HEAD..HEAD"]);
        let mut out = String::new();
        if status.is_empty() {
            out.push_str("nothing uncommitted\n");
        } else {
            out.push_str(&format!("uncommitted:\n{status}\n"));
        }
        if !unpushed.is_empty() {
            out.push_str(&format!("\nunpushed:\n{unpushed}\n"));
        }
        if !commits.is_empty() {
            out.push_str(&format!("\nbranch commits:\n{commits}\n"));
        }
        return out;
    }
    if let Some(agent) = key.strip_prefix("agent:") {
        return Command::new("herdr")
            .args(["agent", "read", agent, "--source", "recent-unwrapped", "--lines", "30"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
            .unwrap_or_else(|e| format!("! {e}"));
    }
    String::new()
}
