//! Key handling and the actions behind the keys.

use super::{App, Prompt, Update, rows::Kind};
use crate::client::herdr;
use crate::table::Table;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};
use std::process::Command;

impl App {
    pub(super) fn on_key(&mut self, key: KeyEvent, terminal: &mut DefaultTerminal) {
        if self.dialog.is_some() {
            self.on_dialog_key(key);
            return;
        }
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
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.rebuild();
            }
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('l') if ctrl => {
                let _ = terminal.clear();
            }
            KeyCode::Char('d') if ctrl => {
                self.detail_scroll = self.detail_scroll.saturating_add(10)
            }
            KeyCode::Char('u') if ctrl => {
                self.detail_scroll = self.detail_scroll.saturating_sub(10)
            }
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
                self.say(if self.all {
                    "showing everything"
                } else {
                    "active work only"
                });
                self.rebuild();
            }
            KeyCode::Char('r') => {
                self.refresh();
                self.say("refreshing…");
            }
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('w') => self.next_waiting(),
            KeyCode::Char('N') => match self.note() {
                Some(path) => self.prompt = Some((Prompt::NoteLine(path), String::new())),
                None => self.say("no note here"),
            },
            KeyCode::Char('a') => match self.current().map(|r| r.kind.clone()) {
                Some(Kind::Loose(i)) => {
                    let key = self.table.loose[i].key.clone();
                    self.run_wt(terminal, &["attach", &key]);
                }
                _ => self.say("a works on a ticket without a task"),
            },
            KeyCode::Char('p') => self.preview = Some(!self.preview_shown),
            KeyCode::Enter => self.go(terminal),
            KeyCode::Char('b') => self.open_in_browser(),
            KeyCode::Char('i') => self.open_in_ide(),
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
            KeyCode::Char('d') => match self.current().map(|r| r.kind.clone()) {
                Some(Kind::Task(i)) => self.close_task(self.table.tasks[i].task.name.clone()),
                _ => match self.tree() {
                    Some(t) => self.remove_tree(t),
                    None => self.say("d works on a task or a worktree"),
                },
            },
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
                    Prompt::Filter | Prompt::StartTicket(_) => {}
                    Prompt::AddTicket(task) => {
                        let key = text.trim().to_uppercase();
                        if !key.is_empty() {
                            self.run_wt(terminal, &["add", &task, &key]);
                        }
                    }
                    Prompt::NoteLine(path) => {
                        if !text.trim().is_empty() {
                            let line = format!(
                                "{}: {}",
                                chrono::Local::now().format("%Y-%m-%d"),
                                text.trim()
                            );
                            match add_note_line(&path, &line) {
                                Ok(()) => self.say("noted"),
                                Err(e) => self.say(format!("! {e}")),
                            }
                            self.refresh();
                        }
                    }
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

    /// The task name or loose ticket key `wt select` takes for the selection.
    pub(super) fn target(&self) -> Option<String> {
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

    pub(super) fn url(&self) -> Option<String> {
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
            Kind::Task(_) | Kind::Header(_) => None,
        }
    }

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
                if self.popup {
                    let args: Vec<&str> = args.iter().map(String::as_str).collect();
                    self.run_wt(terminal, &args);
                } else {
                    self.say(format!("opening {}…", tree.wt.name()));
                    self.run_wt_background(args);
                }
            }
            Kind::Mr(_) => self.open_in_browser(),
            Kind::Ticket(key) => self.run_wt(terminal, &["select", &key]),
            Kind::Header(_) => {}
            // nothing `wt select` can get back to yet: no worktree, no open MR
            Kind::Loose(i) if self.table.loose[i].state.is_none() => {
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

    fn focus(&mut self, agent: &str) {
        match herdr::focus_agent(agent) {
            Ok(()) => {
                self.say(format!("→ {agent}"));
                self.quit |= self.popup;
            }
            Err(e) => self.say(format!("! {e}")),
        }
    }

    fn open_in_browser(&mut self) {
        match self.url() {
            Some(url) => open(&url),
            None => self.say("nothing to open in the browser"),
        }
    }

    /// The worktree to open in the IDE: the selected one, or the only live one
    /// of the selected task or ticket.
    pub(super) fn ide_tree(&self) -> Option<usize> {
        if let Some(t) = self.tree() {
            return Some(t);
        }
        let trees = match &self.current()?.root {
            Kind::Task(i) => &self.table.tasks[*i].trees,
            Kind::Loose(i) => &self.table.loose[*i].trees,
            _ => return None,
        };
        let live: Vec<usize> = trees
            .iter()
            .copied()
            .filter(|&t| !self.table.tree(t).finished)
            .collect();
        match live.as_slice() {
            [t] => Some(*t),
            _ => None,
        }
    }

    /// `i`: the worktree in the IDE; an already open project just comes forward.
    fn open_in_ide(&mut self) {
        let Some(t) = self.ide_tree() else {
            self.say("i: select a worktree");
            return;
        };
        let tree = self.table.tree(t);
        let ide = &self.config.workflow.ide;
        match Command::new("open")
            .arg("-a")
            .arg(ide)
            .arg(&tree.wt.path)
            .status()
        {
            Ok(s) if s.success() => {
                self.say(format!("{} → {ide}", tree.wt.name()));
                self.quit |= self.popup;
            }
            _ => self.say(format!("! could not open {} in {ide}", tree.wt.name())),
        }
    }

    fn open_note_in_obsidian(&mut self) {
        match self.note() {
            Some(path) => open(&format!(
                "obsidian://open?path={}",
                percent_encode(&path.to_string_lossy())
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
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        ratatui::restore();
        println!("$ wt {}\n", args.join(" "));
        let ok = Command::new(exe)
            .args(args)
            .status()
            .is_ok_and(|s| s.success());
        // as a popup, work that was opened is where to be now; errors stay readable
        let navigates = matches!(args.first(), Some(&"select" | &"start" | &"add"));
        if ok && navigates && self.popup {
            self.quit = true;
            return;
        }
        println!("\n⏎ back to wt");
        let _ = std::io::stdin().read_line(&mut String::new());
        *terminal = ratatui::init();
        self.say(format!(
            "wt {}: {}",
            args.join(" "),
            if ok { "done" } else { "failed" }
        ));
        self.refresh();
    }

    /// Runs `wt <args>` that asks nothing, reporting its last line.
    fn run_wt_background(&mut self, args: Vec<String>) {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let text = match Command::new(exe).args(&args).output() {
                Ok(out) => {
                    let stream = if out.status.success() {
                        out.stdout
                    } else {
                        out.stderr
                    };
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
}

/// The herdr name of a worktree's agent.
fn agent_name(table: &Table, tree: usize, agent: usize) -> String {
    table.tree(tree).agents[agent].name.clone()
}

/// Appends `- line` to the note's own text under the notes lock, rereading it
/// first: agents may have just written to it.
fn add_note_line(path: &Path, line: &str) -> Result<(), String> {
    let _lock = crate::vault::lock().map_err(|e| e.to_string())?;
    let mut note = crate::vault::note::Note::read(path).map_err(|e| e.to_string())?;
    note.append_line(line);
    note.save().map(|_| ()).map_err(|e| e.to_string())
}

/// `text` for a URL: everything but RFC 3986 unreserved characters as `%XX`.
fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn open(target: &str) {
    let _ = Command::new("open").arg(target).spawn();
}
