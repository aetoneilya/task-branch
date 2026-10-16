//! `wt` without a command: the task tree on the left, details of the selection on
//! the right. The offline table (worktrees, agents, notes) renders at once; Jira
//! and GitLab fill in from a background refresh, repeated every minute.
//! Actions run the same `wt` commands; interactive ones get the terminal for a while.

mod details;
mod draw;
mod keys;
mod rows;

use crate::client::herdr;
use crate::config::Config;
use crate::table::{self, Table};
use crate::vault::{self, note::NoteError};
use details::Detail;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::widgets::ListState;
use rows::{Kind, Row};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

const REFRESH_EVERY: Duration = Duration::from_secs(60);
/// herdr is local and quick: agent states are polled this often.
const AGENTS_EVERY: Duration = Duration::from_secs(2);
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
    /// A line for the task note.
    NoteLine(PathBuf),
}

enum Update {
    Table(Box<Table>, Vec<String>),
    /// A lazily loaded detail.
    Detail(Detail, String),
    Message(String),
    Agents(Vec<herdr::Agent>),
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
    details: HashMap<Detail, String>,
    pending: HashSet<Detail>,
    detail_scroll: u16,
    tx: Sender<Update>,
    rx: Receiver<Update>,
    quit: bool,
    /// Run as a herdr popup (`WT_POPUP=1`): going to some work closes it.
    popup: bool,
    agents_polled: Instant,
    agents_polling: bool,
}

impl App {
    fn new(config: Config) -> Result<App, String> {
        let (tasks, skipped) = vault::task::load(&config)?;
        let mut table = table::build_cached(&config, tasks);
        report_skipped(&mut table, &skipped);
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
            popup: std::env::var_os("WT_POPUP").is_some(),
            agents_polled: Instant::now(),
            agents_polling: false,
        };
        app.rebuild();
        if let Some(warning) = app.table.warnings.first() {
            app.say(format!("! {warning}"));
        }
        app.refresh();
        Ok(app)
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<(), String> {
        while !self.quit {
            self.load_details();
            terminal.draw(|f| self.draw(f)).map_err(|e| e.to_string())?;
            if event::poll(Duration::from_millis(200)).map_err(|e| e.to_string())?
                && let Event::Key(key) = event::read().map_err(|e| e.to_string())?
                && key.kind == KeyEventKind::Press
            {
                self.on_key(key, terminal);
            }
            while let Ok(update) = self.rx.try_recv() {
                self.apply(update);
            }
            if self.last_refresh.elapsed() > REFRESH_EVERY {
                self.refresh();
            }
            if !self.agents_polling && self.agents_polled.elapsed() > AGENTS_EVERY {
                self.poll_agents();
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
            // the config is the one `new` loaded the vault with: it does not fail here
            let Ok((tasks, skipped)) = vault::task::load(&config) else {
                return;
            };
            let mut table = table::build(&config, tasks, true);
            report_skipped(&mut table, &skipped);
            let updated = table::write_back(&mut table);
            let _ = tx.send(Update::Table(Box::new(table), updated));
        });
    }

    fn poll_agents(&mut self) {
        if !herdr::available() {
            return;
        }
        self.agents_polling = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Update::Agents(herdr::list_agents().unwrap_or_default()));
        });
    }

    /// Agent rows whose agent waits for me: asking (blocked) or finished unseen (done).
    fn waiting(&self) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| match r.kind {
                Kind::Agent(t, a) => self.table.tree(t).agents[a].status.waiting(),
                _ => false,
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// `w`: the next waiting agent after the selection, round the list.
    fn next_waiting(&mut self) {
        let waiting = self.waiting();
        let current = self.list.selected().unwrap_or(0);
        match waiting.iter().find(|&&i| i > current).or(waiting.first()) {
            Some(&i) => self.select(i),
            None => self.say("no agent is waiting"),
        }
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
            Update::Detail(detail, text) => {
                self.pending.remove(&detail);
                self.details.insert(detail, text);
            }
            Update::Message(text) => self.say(text),
            Update::Agents(agents) => {
                self.agents_polling = false;
                self.agents_polled = Instant::now();
                self.table.update_agents(&agents);
                self.rebuild();
            }
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
            if !self.rows[index as usize].kind.is_header() {
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
            .find(|&i| !self.rows[i].kind.is_header())
        {
            index = i;
        }
        let id = self.rows[index].id.clone();
        if self.selected.as_ref() != Some(&id) {
            self.detail_scroll = 0;
            // agent output is live: read it again every time
            if let Kind::Agent(..) = self.rows[index].kind
                && let Some(detail) = self.detail(&self.rows[index].kind)
            {
                self.details.remove(&detail);
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
}

/// Notes that could not be read are left out of the table, with a warning as
/// `wt status` prints it.
fn report_skipped(table: &mut Table, skipped: &[NoteError]) {
    table
        .warnings
        .extend(skipped.iter().map(|err| format!("skipped {err}")));
}
