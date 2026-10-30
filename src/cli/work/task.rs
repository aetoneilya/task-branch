//! `wt note`: an agent's finding appended to its task note.

use super::common::{current_task, err};
use crate::cli::load;
use crate::client::herdr;
use crate::config::Config;
use crate::table;
use crate::vault::note::NoteError;

/// `wt note "<text>"`: an agent's finding, appended to `## Agent notes` of the
/// task it works on (by its worktree, or by its herdr workspace).
pub(in crate::cli) fn cli_note(config: &Config, text: Vec<String>) -> Result<(), String> {
    let text = text.join(" ").trim().to_string();
    if text.is_empty() {
        return Err("wt note \"<text>\"".to_string());
    }
    let mut table = table::build(config, load(config)?, false);
    let index = current_task(&table)
        .ok_or("no task here: run it from the task's worktree or herdr workspace")?;
    let mut task = table.tasks.swap_remove(index).task;

    let agent = std::env::var("HERDR_PANE_ID")
        .ok()
        .and_then(|pane| {
            herdr::list_agents()
                .ok()?
                .into_iter()
                .find(|a| a.pane_id == pane)?
                .name
        })
        .unwrap_or_else(|| "agent".to_string());
    let line = format!(
        "{} {agent}: {text}",
        chrono::Local::now().format("%Y-%m-%d")
    );

    let _lock = crate::vault::lock().map_err(err)?;
    for _ in 0..3 {
        task.note.append_agent_note(&line);
        match task.note.save() {
            Ok(_) => {
                println!("written to \"{}\"", task.name);
                return Ok(());
            }
            Err(NoteError::Changed(_)) => task.note.reload().map_err(err)?,
            Err(e) => return Err(err(e)),
        }
    }
    Err(format!("\"{}\" keeps changing, not written", task.name))
}
