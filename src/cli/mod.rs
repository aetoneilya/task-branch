mod progress;
mod table;
mod work;

use crate::config::{Config, load_config};
use crate::vault::task::Task;
use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use std::io::{self, IsTerminal};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "wt", author, version, long_about = None)]
/// Tasks, Jira tickets, merge requests and worktrees in one table
pub struct Cli {
    /// Without a command: the TUI
    #[command(subcommand)]
    command: Option<CliCommands>,
}

#[derive(Subcommand, Debug)]
enum CliCommands {
    /// The TUI, as `wt` without a command
    Tui {},

    /// Start new work. `wt start "<task name>"`: a task note, its herdr workspace
    /// and a worktree tab with claude briefed from the note and the first ticket.
    /// `wt start UCP-123`: the same, the task named after the ticket's summary
    Start(StartArgs),
    /// Add a ticket to a task, with its own worktree tab in the task's workspace
    Add(AddArgs),
    /// Put a ticket into a task without starting work on it (no worktree)
    Attach {
        key: String,
        /// The task; a picker when omitted
        task: Option<String>,
    },
    /// Go back to a task: its agent, or claude resuming the last session there
    Select(SelectArgs),
    /// (internal) Run in a new tab by start/add: create the worktree, then claude
    #[command(hide = true)]
    Prepare { spec: PathBuf },
    /// (for agents) Append a finding to the task note's "Agent notes"
    Note {
        #[arg(trailing_var_arg = true, required = true)]
        text: Vec<String>,
    },
    /// Tasks with their tickets, merge requests, worktrees and agents; then work
    /// that belongs to no task. Updates `state` and `sessions` in task notes.
    Status(StatusArgs),
    /// Start a reviewer agent next to the task's agent
    Review { name: Option<String> },
    /// Close a task: `wt done "<task>"` (or a ticket of it) removes its finished
    /// worktrees, closes my tickets in Jira and marks the note done. With a
    /// worktree name, or none inside one: remove that worktree
    Done(DoneArgs),
    /// Shell completion script
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
}

// The arguments of the bigger commands, handed to them whole. No doc comments
// on the structs: clap would take them for the commands' help.

#[derive(Args, Debug)]
struct StartArgs {
    /// Task name or ticket key; a picker when omitted
    target: Option<String>,
    /// Tickets of the new task, the first one gets the worktree
    #[arg(long = "ticket", short = 'k')]
    tickets: Vec<String>,
    #[arg(long = "tag", short = 't')]
    tags: Vec<String>,
    /// The goal: first line of the note and the agent's brief
    #[arg(long, short = 'm')]
    message: Option<String>,
    /// Worktree, tab and agent name; default: from the ticket summary
    #[arg(long, short = 'n')]
    name: Option<String>,
    /// Default: <branch_prefix><KEY>-<name>
    #[arg(long, short = 'b')]
    branch: Option<String>,
    /// Don't switch to the new tab
    #[arg(long)]
    bg: bool,
}

#[derive(Args, Debug)]
struct AddArgs {
    /// [<task>] <KEY>; the task defaults to the current one
    #[arg(num_args = 1..=2, required = true)]
    args: Vec<String>,
    /// Run the agent in this existing worktree of the task (stacked MRs)
    #[arg(long = "in")]
    in_tree: Option<String>,
    #[arg(long, short = 'n')]
    name: Option<String>,
    #[arg(long, short = 'm')]
    message: Option<String>,
    #[arg(long)]
    bg: bool,
}

#[derive(Args, Debug)]
struct SelectArgs {
    /// Task name or ticket key; a picker when omitted
    target: Option<String>,
    /// That worktree of the task
    #[arg(long)]
    tree: Option<String>,
    /// Pick which of the task's sessions to resume
    #[arg(long)]
    session: bool,
    /// Don't switch to it
    #[arg(long)]
    bg: bool,
}

#[derive(Args, Debug)]
struct StatusArgs {
    /// Only tasks with this tag
    #[arg(long, short = 't')]
    tag: Option<String>,
    /// Also done tasks, tickets with their status, merged MRs, finished
    /// worktrees and tickets without activity
    #[arg(long, short = 'a')]
    all: bool,
    /// Don't update task notes
    #[arg(long)]
    no_write: bool,
}

#[derive(Args, Debug)]
struct DoneArgs {
    name: Option<String>,
    /// Remove every clean worktree whose MR is merged or closed
    #[arg(long)]
    merged: bool,
    /// Skip the uncommitted / open MR / unpushed checks
    #[arg(long, short = 'f')]
    force: bool,
    #[arg(long, short = 'y')]
    yes: bool,
}

pub fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let config = load_config().map_err(|err| err.to_string())?;
    let command = match cli.command {
        None | Some(CliCommands::Tui {}) => return crate::tui::run(config),
        Some(command) => command,
    };
    match command {
        CliCommands::Status(args) => table::cli_status(&config, args),
        CliCommands::Start(args) => work::cli_start(&config, args),
        CliCommands::Add(args) => work::cli_add(&config, args),
        CliCommands::Select(args) => work::cli_select(&config, args),
        CliCommands::Note { text } => work::cli_note(&config, text),
        CliCommands::Prepare { spec } => work::cli_prepare(&spec),
        CliCommands::Attach { key, task } => work::cli_attach(&config, &key, task),
        CliCommands::Review { name } => work::cli_review(&config, name),
        CliCommands::Done(args) => work::cli_done(&config, args),
        CliCommands::Completions { shell } => {
            generate(shell, &mut Cli::command(), "wt", &mut io::stdout());
            Ok(())
        }
        CliCommands::Tui {} => unreachable!(),
    }
}

/// The vault's tasks; notes that can't be read are reported and left out.
fn load(config: &Config) -> Result<Vec<Task>, String> {
    let (tasks, skipped) = crate::vault::task::load(config)?;
    for err in skipped {
        eprintln!("skipped {err}");
    }
    Ok(tasks)
}

/// Colors on a terminal only, and never with NO_COLOR set.
fn colors(stream: &impl IsTerminal) -> bool {
    stream.is_terminal() && std::env::var_os("NO_COLOR").is_none()
}
