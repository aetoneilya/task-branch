//! `wt` without a command: the task tree on the left, details of the selection on
//! the right. The offline table (worktrees, agents, notes) renders at once; Jira
//! and GitLab fill in from a background refresh, repeated every minute.
//! Actions run the same commands as `wt` does, in the background: they report
//! and ask in a dialog.

mod anim;
mod details;
mod dialog;
mod draw;
mod hide;
mod keys;
mod rows;

use crate::client::herdr;
use crate::config::Config;
use crate::table::view::Hide;
use crate::table::{self, Table};
use crate::vault::{self, note::NoteError};
use details::Detail;
use dialog::{Action, Dialog};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Position, Rect};
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
    let mut terminal = init_terminal();
    let result = app.run(&mut terminal);
    restore_terminal();
    result
}

/// The full-screen terminal, with the mouse reported to the TUI.
fn init_terminal() -> DefaultTerminal {
    let terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    terminal
}

fn restore_terminal() {
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
}

enum Prompt {
    Filter,
    /// A ticket key to add to the task.
    AddTicket(String),
    NewTask,
    /// A line for the task note.
    NoteLine(PathBuf),
}

enum Update {
    Table(Box<Table>, Vec<String>),
    /// A lazily loaded detail.
    Detail(Detail, String),
    /// herdr's agents and tabs.
    Agents(Vec<herdr::Agent>, Vec<herdr::Tab>),
    /// A dialog's action checked, or why it can't be.
    Plan(Result<Action, String>),
    /// A line of a running command's report.
    Said(String),
    /// A running command's question: a pick of `items`, yes or no without them.
    Ask {
        prompt: String,
        items: Vec<String>,
        reply: dialog::Reply,
    },
    Finished(Result<(), String>),
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
    /// Kinds of rows left out, `v`; kept between runs.
    hide: Hide,
    filter: String,
    prompt: Option<(Prompt, String)>,
    dialog: Option<Dialog>,
    /// Shown in the footer for a few seconds instead of the key hints.
    message: Option<(String, Instant)>,
    help: bool,
    /// `p` overrides the width rule: Some(true) shows the details, Some(false) hides.
    preview: Option<bool>,
    /// Whether the last frame had the details pane: details load only then.
    preview_shown: bool,
    refreshing: bool,
    last_refresh: Instant,
    /// How long the last refresh took: the header's bar fills against it.
    refresh_took: Option<Duration>,
    synced: Option<chrono::DateTime<chrono::Local>>,
    details: HashMap<Detail, String>,
    pending: HashSet<Detail>,
    /// Loaded details to read again, shown as they were meanwhile.
    stale: HashSet<Detail>,
    detail_scroll: u16,
    tx: Sender<Update>,
    rx: Receiver<Update>,
    quit: bool,
    /// Run as a herdr popup (`WT_POPUP=1`): going to some work closes it.
    popup: bool,
    agents_polled: Instant,
    agents_polling: bool,
    /// Where the last frame drew the list and the details: mouse targets.
    list_area: Rect,
    details_area: Rect,
    /// Panes whose unnamed agent was given a name: asked once while unnamed.
    named: HashSet<String>,
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
            hide: hide::load(),
            filter: String::new(),
            prompt: None,
            dialog: None,
            message: None,
            help: false,
            preview: None,
            preview_shown: true,
            refreshing: false,
            last_refresh: Instant::now(),
            refresh_took: None,
            synced: None,
            details: HashMap::new(),
            pending: HashSet::new(),
            stale: HashSet::new(),
            detail_scroll: 0,
            tx,
            rx,
            quit: false,
            popup: std::env::var_os("WT_POPUP").is_some(),
            agents_polled: Instant::now(),
            agents_polling: false,
            list_area: Rect::default(),
            details_area: Rect::default(),
            named: HashSet::new(),
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
            let wait = if self.animating() {
                anim::FRAME
            } else {
                Duration::from_millis(200)
            };
            if event::poll(wait).map_err(|e| e.to_string())? {
                match event::read().map_err(|e| e.to_string())? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        self.on_key(key, terminal)
                    }
                    Event::Mouse(mouse) => self.on_mouse(mouse),
                    _ => {}
                }
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

    /// Something on screen moves: a refresh, a command, details loading, or an
    /// agent at work.
    fn animating(&self) -> bool {
        self.refreshing
            || self.dialog.as_ref().is_some_and(Dialog::busy)
            || (self.preview_shown && !self.pending.is_empty())
            || self.rows.iter().any(|r| match r.kind {
                Kind::Agent(at, a) => self.table.agent(at, a).status == table::AgentStatus::Working,
                _ => false,
            })
    }

    fn poll_agents(&mut self) {
        if !herdr::available() {
            return;
        }
        self.agents_polling = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let agents = herdr::list_agents().unwrap_or_default();
            let tabs = herdr::list_tabs().unwrap_or_default();
            let _ = tx.send(Update::Agents(agents, tabs));
        });
    }

    /// Names the agents herdr knows by pane id only, so herdr and the tree show
    /// what they work on; the next poll brings the names in.
    fn name_agents(&mut self, tabs: &HashMap<String, String>) {
        if !herdr::available() {
            return;
        }
        let names = self.table.agent_names(tabs);
        // named since: should it lose the name again, it gets one again
        self.named
            .retain(|pane| names.iter().any(|(p, _)| p == pane));
        let names: Vec<(String, String)> = names
            .into_iter()
            .filter(|(pane, _)| self.named.insert(pane.clone()))
            .collect();
        if !names.is_empty() {
            std::thread::spawn(move || {
                for (pane, name) in names {
                    let _ = herdr::rename_agent(&pane, &name);
                }
            });
        }
    }

    /// Agent rows whose agent waits for me: asking (blocked) or finished unseen
    /// (done).
    fn waiting(&self) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| match r.kind {
                Kind::Agent(at, a) => self.table.agent(at, a).status.waiting(),
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
                self.refresh_took = Some(self.last_refresh.elapsed());
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
            Update::Plan(plan) => self.on_plan(plan),
            Update::Said(line) => self.on_said(line),
            Update::Ask {
                prompt,
                items,
                reply,
            } => self.on_ask(prompt, items, reply),
            Update::Finished(result) => self.on_finished(result),
            Update::Agents(agents, tabs) => {
                self.agents_polling = false;
                self.agents_polled = Instant::now();
                self.table.update_agents(&agents);
                // their screens moved on too: read again after each poll
                let screens = self
                    .details
                    .keys()
                    .filter(|d| matches!(d, Detail::Screen(_)));
                self.stale.extend(screens.cloned());
                self.rebuild();
                let tabs = tabs.into_iter().map(|t| (t.tab_id, t.label)).collect();
                self.name_agents(&tabs);
            }
        }
    }

    /// Rows from the table, keeping the selection on the same item.
    fn rebuild(&mut self) {
        self.rows = rows::build(&self.table, self.all, self.hide, &self.filter, &self.folded);
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
    /// The wheel scrolls the list or the details under it; a click selects a
    /// row, a click on the selected one folds or unfolds it. Not while a dialog,
    /// the help or a prompt is up.
    fn on_mouse(&mut self, mouse: MouseEvent) {
        if self.dialog.is_some() || self.help || self.prompt.is_some() {
            return;
        }
        let at = Position::new(mouse.column, mouse.row);
        let in_details = self.preview_shown && self.details_area.contains(at);
        match mouse.kind {
            MouseEventKind::ScrollDown if in_details => {
                self.detail_scroll = self.detail_scroll.saturating_add(3)
            }
            MouseEventKind::ScrollUp if in_details => {
                self.detail_scroll = self.detail_scroll.saturating_sub(3)
            }
            MouseEventKind::ScrollDown if self.list_area.contains(at) => self.step(1),
            MouseEventKind::ScrollUp if self.list_area.contains(at) => self.step(-1),
            MouseEventKind::Down(MouseButton::Left) if self.list_area.contains(at) => {
                // inside the border, below the title
                let Some(line) = mouse.row.checked_sub(self.list_area.y + 1) else {
                    return;
                };
                let index = self.list.offset() + line as usize;
                if index >= self.rows.len() || self.rows[index].kind.is_header() {
                    return;
                }
                if self.list.selected() == Some(index) {
                    self.toggle_fold();
                } else {
                    self.select(index);
                }
            }
            _ => {}
        }
    }

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
