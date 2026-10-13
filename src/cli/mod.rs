mod progress;
mod table;
mod tasks;
mod work;

use crate::client::git::key_from_branch_name;
use crate::client::gitlab::MergeRequest;
use crate::client::jira::{JiraIssue, JiraSubtask};
use crate::config::load_config;
use crate::core::JiraGitlabState;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, FuzzySelect, Input};
use std::collections::HashMap;
use std::path::PathBuf;
use std::{fmt, io};

#[derive(Parser, Debug)]
#[command(name = "wt", author, version, long_about = None)]
/// Tasks, Jira tickets, merge requests and worktrees in one table
pub struct Cli {
    /// Without a command: the TUI
    #[command(subcommand)]
    command: Option<CliCommands>,

    #[arg(long = "repo", short = 'r', default_value = ".", global = true)]
    repo_path: PathBuf,
}

#[derive(Subcommand, Debug)]
enum CliCommands {
    List {
        #[command(subcommand)]
        list_option: Option<ListOption>,
    },
    NewTask {
        #[arg(long, short = 'k')]
        key: Option<String>,
    },
    Mr {
        #[arg(long, short = 'k')]
        key: Option<String>,
        #[arg(long, short = 't')]
        title: Option<String>,
        #[arg(long, short = 'd')]
        description: Option<String>,
    },
    Switch {
        #[arg(long, short = 'k')]
        key: Option<String>,
    },
    Info {
        #[arg(long, short = 'k')]
        key: String,
    },
    /// The TUI, as `wt` without a command
    Tui {},

    /// Start new work. `wt start "<task name>"`: a task note, its herdr workspace
    /// and a worktree tab with claude briefed from the note and the first ticket.
    /// `wt start UCP-123`: the same, the task named after the ticket's summary
    Start {
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
    },
    /// Add a ticket to a task, with its own worktree tab in the task's workspace
    Add {
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
    },
    /// Go back to a task: its agent, or claude resuming the last session there
    Select {
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
    },
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
    Status {
        /// Only tasks with this tag
        #[arg(long, short = 't')]
        tag: Option<String>,
        /// Also done tasks
        #[arg(long, short = 'a')]
        all: bool,
        /// Don't update task notes
        #[arg(long)]
        no_write: bool,
    },
    /// Add `tickets:` to task notes made before worktable (dry run by default)
    Migrate {
        #[arg(long)]
        apply: bool,
    },
    /// Start a reviewer agent next to the task's agent
    Review {
        name: Option<String>,
    },
    /// Remove a finished task: worktree, branch and herdr workspace
    Done {
        name: Option<String>,
        /// Remove every clean worktree whose MR is merged or closed
        #[arg(long)]
        merged: bool,
        /// Skip the uncommitted / open MR / unpushed checks
        #[arg(long, short = 'f')]
        force: bool,
        #[arg(long, short = 'y')]
        yes: bool,
    },

    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(Subcommand, Debug)]
enum ListOption {
    Mr,
    Task,
    All,
}

pub fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let config = load_config().map_err(|err| err.to_string())?;
    let command = match cli.command {
        None | Some(CliCommands::Tui {}) => return crate::tui::run(config),
        Some(command) => command,
    };
    match command {
        CliCommands::Status { tag, all, no_write } => {
            return table::cli_status(&config, tag, all, no_write);
        }
        CliCommands::Migrate { apply } => return table::cli_migrate(&config, apply),
        CliCommands::Start {
            target,
            tickets,
            tags,
            message,
            name,
            branch,
            bg,
        } => return work::cli_start(&config, target, tickets, tags, message, name, branch, bg),
        CliCommands::Add {
            args,
            in_tree,
            name,
            message,
            bg,
        } => return work::cli_add(&config, args, in_tree, name, message, bg),
        CliCommands::Select {
            target,
            tree,
            session,
            bg,
        } => {
            return work::cli_select(&config, target, tree, session, bg);
        }
        CliCommands::Note { text } => return work::cli_note(&config, text),
        CliCommands::Prepare { spec } => return work::cli_prepare(&spec),
        _ => {}
    }
    let (repo_path, project) = work::legacy_repo(&config, &cli.repo_path);
    let mut gitlab = config.gitlab;
    if let Some(project) = project {
        gitlab.project = project;
    }
    let workflow = config.workflow;

    let state = JiraGitlabState::new(repo_path, config.jira, gitlab)
        .map_err(|err| err.to_string())?;

    match command {
        CliCommands::List { list_option } => cli_list(&state, list_option)?,
        CliCommands::NewTask { key } => cli_new_task(&state, key)?,
        CliCommands::Mr {
            key,
            title,
            description,
        } => cli_create_mr(&state, key, title, description)?,
        CliCommands::Switch { key } => select_mr(&state, key)?,
        CliCommands::Info { key } => cli_info(&state, key)?,
        CliCommands::Tui {} => unreachable!(),
        CliCommands::Status { .. }
        | CliCommands::Migrate { .. }
        | CliCommands::Start { .. }
        | CliCommands::Add { .. }
        | CliCommands::Select { .. }
        | CliCommands::Note { .. }
        | CliCommands::Prepare { .. } => unreachable!(),
        CliCommands::Review { name } => tasks::cli_review(&state, &workflow, name)?,
        CliCommands::Done {
            name,
            merged,
            force,
            yes,
        } => tasks::cli_done(&state, name, merged, force, yes)?,
        CliCommands::Completions { shell } => cli_completion(shell)?,
    }

    Ok(())
}

//TODO fix returns completion for jira-gitlab not jg
fn cli_completion(shell: Shell) -> Result<(), String> {
    let mut cmd = Cli::command(); // get clap Command
    generate(shell, &mut cmd, "jg", &mut io::stdout());
    Ok(())
}

fn cli_info(state: &JiraGitlabState, key: String) -> Result<(), String> {
    let issue = state
        .find_issue_by_key(key.as_ref())
        .map_err(|err| err.to_string())?;

    let mrs = state
        .find_merge_request_by_key(key.as_ref())
        .map_err(|err| err.to_string())?;

    if let None = issue {
        return Err(format!("No issue found with key: {}", key));
    }

    let issue = issue.unwrap();

    let link = task_hyperlink(issue.key.as_ref());
    let fields = &issue.fields;

    println!("{} {}", link, fields.summary);
    println!("status: {}", issue.status());

    if let Some(description) = &fields.description {
        println!("\ndescription:");
        println!("{}", description);
    }

    if let Some(assignee) = &fields.assignee {
        println!("\nassignee: {}", assignee.display_name);
    }

    if let Some(subtasks) = &fields.subtasks {
        println!("\nsubtasks:");
        for subtask in subtasks {
            println!("{}", subtask);
        }
    }

    if !mrs.is_empty() {
        println!("\nmerge requests:");
        for mr in mrs {
            println!("{}", display_mr(&mr, true));
        }
    }

    Ok(())
}

fn cli_new_task(state: &JiraGitlabState, key: Option<String>) -> Result<(), String> {
    let issue = select_issue(state, key)?;
    let suggested_branch = branch_name_for_issue(&issue);

    let branch: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Branch name")
        .default(suggested_branch)
        .interact_text()
        .map_err(|err| err.to_string())?;

    if state
        .local_branch_exists(&branch)
        .map_err(|err| err.to_string())?
    {
        state.checkout_to(&branch).map_err(|err| err.to_string())?;
        println!("Checked out existing branch {branch}");
        return Ok(());
    }

    state
        .create_branch(&branch)
        .map_err(|err| err.to_string())?;
    println!("Created and checked out branch {branch}");

    Ok(())
}

fn cli_create_mr(
    state: &JiraGitlabState,
    key: Option<String>,
    title: Option<String>,
    description: Option<String>,
) -> Result<(), String> {
    let branch = current_git_branch(state)?;
    let issue = match key.or_else(|| key_from_branch_name(&branch).map(str::to_string)) {
        Some(key) => state
            .find_issue_by_key(&key)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("No issue found with key: {key}"))?,
        None => select_issue(state, None)?,
    };

    let default_title = format!("{} {}", issue.key, issue.fields.summary);
    let mr_title = match title {
        Some(title) => title,
        None => Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Merge request title")
            .default(default_title)
            .interact_text()
            .map_err(|err| err.to_string())?,
    };

    let default_description = issue.fields.description.clone().unwrap_or_default();

    let mr_description = match description {
        Some(description) => Some(description),
        None => {
            let use_issue_description = !default_description.is_empty()
                && Confirm::with_theme(&ColorfulTheme::default())
                    .with_prompt("Use Jira issue description in merge request?")
                    .default(true)
                    .interact()
                    .map_err(|err| err.to_string())?;

            if use_issue_description {
                Some(default_description)
            } else {
                let manual_description: String = Input::with_theme(&ColorfulTheme::default())
                    .with_prompt("Merge request description")
                    .allow_empty(true)
                    .interact_text()
                    .map_err(|err| err.to_string())?;
                Some(manual_description)
            }
        }
    };

    let mr = state
        .create_merge_request(mr_title, mr_description)
        .map_err(|err| err.to_string())?;

    println!("Created merge request: {}", display_mr(&mr, true));
    Ok(())
}

fn cli_list(state: &JiraGitlabState, list_option: Option<ListOption>) -> Result<(), String> {
    match list_option.unwrap_or(ListOption::All) {
        ListOption::Mr => list_mrs(&state),
        ListOption::Task => list_jira_tasks(&state),
        ListOption::All => list_jira_tasks_with_mr(&state),
    }

    Ok(())
}

fn list_mrs(state: &JiraGitlabState) {
    let mrs = state
        .list_merge_request()
        .expect("Failed to list merge requests");

    println!("Merge requests:");
    for mr in mrs {
        println!("{}", display_mr(&mr, true))
    }
}

fn list_jira_tasks_with_mr(state: &JiraGitlabState) {
    let issues = state.unresolved_current_user_issues();

    if let Err(e) = issues {
        println!("Error while issues search: {}", e);
        return;
    }

    let mrs: HashMap<String, Vec<MergeRequest>> = state
        .list_merge_request()
        .expect("Failed to get merge request")
        .into_iter()
        .filter_map(|mr| mr.extract_task().map(|task| (task, mr)))
        .fold(HashMap::new(), |mut acc, (task, mr)| {
            acc.entry(task).or_insert_with(Vec::new).push(mr);
            acc
        });

    display_tasks_mr(issues.unwrap(), mrs);
}

fn list_jira_tasks(state: &JiraGitlabState) {
    let issues = state.unresolved_current_user_issues();

    if let Err(e) = issues {
        println!("Error while issues search: {}", e);
        return;
    }

    display_tasks_mr(issues.unwrap(), HashMap::new());
}

fn display_tasks_mr(issues: Vec<JiraIssue>, mrs: HashMap<String, Vec<MergeRequest>>) {
    let issues = issues;
    println!("Current issues:");
    for issue in issues {
        print!("{}", issue);

        if let Some(mrs_for_issue) = mrs.get(&issue.key) {
            for mr in mrs_for_issue {
                println!("\t   {}", display_mr(mr, true));
            }
        }

        if let Some(subtasks) = &issue.fields.subtasks {
            for subtask in subtasks {
                println!("   └─ {}", subtask);
            }
        }
    }
}

fn select_mr(state: &JiraGitlabState, key: Option<String>) -> Result<(), String> {
    let mut mrs = state.list_merge_request().map_err(|e| e.to_string())?;

    if let Some(key) = key {
        mrs = mrs
            .into_iter()
            .filter(|it| it.title().contains(&key))
            .collect();
    }

    let mr_strings: Vec<String> = mrs.iter().map(|it| display_mr(it, false)).collect();

    let selection = FuzzySelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Merge Requests:")
        .items(&mr_strings)
        .interact_opt()
        .expect("Failed to select merge requests")
        .expect("Merge requests wasn't selected");

    println!("Checkout to {}", mrs[selection].source_branch());

    let branch = mrs[selection].source_branch();

    state
        .checkout_to(branch)
        .expect("Failed to checkout branch");

    Ok(())
}

fn select_issue(state: &JiraGitlabState, key: Option<String>) -> Result<JiraIssue, String> {
    if let Some(key) = key {
        return state
            .find_issue_by_key(&key)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("No issue found with key: {key}"));
    }

    let issues = state
        .unresolved_current_user_issues()
        .map_err(|err| err.to_string())?;

    if issues.is_empty() {
        return Err("No unresolved issues assigned to the current user".to_string());
    }

    let issue_strings: Vec<String> = issues
        .iter()
        .map(|issue| {
            format!(
                "{} [{}] {}",
                issue.key,
                issue.status(),
                issue.fields.summary
            )
        })
        .collect();

    let selection = FuzzySelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Jira issues:")
        .items(&issue_strings)
        .interact_opt()
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "Issue wasn't selected".to_string())?;

    Ok(issues
        .into_iter()
        .nth(selection)
        .expect("Selected issue index should exist"))
}

fn branch_name_for_issue(issue: &JiraIssue) -> String {
    format!("{}-{}", issue.key, slugify(&issue.fields.summary))
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

fn current_git_branch(state: &JiraGitlabState) -> Result<String, String> {
    state.current_branch().map_err(|err| err.to_string())
}

impl JiraGitlabState {
    // fn create_mr(&self, task_key: String, title: String) {
    //     let jira_task = self
    //         .find_issue_by_key(task_key.as_ref())
    //         .expect("Failed to find issue by key")
    //         .expect("Can't find issue by key");
    //
    //     let branch_name = format!("iudalov-{}-{}", jira_task.key, title);
    //
    //     // self.create_merge_request()()
    // }
}

impl fmt::Display for JiraIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let link = task_hyperlink(self.key.as_ref());
        let summary = &self.fields.summary;
        writeln!(f, "📌 {}  [{}] \t{}", link, self.status(), summary)?;

        Ok(())
    }
}

impl fmt::Display for JiraSubtask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let link = task_hyperlink(self.key.as_ref());
        let summary = &self.fields.summary;
        write!(f, "🔹 {}  [{}] \t{}", link, self.status(), summary)?;

        Ok(())
    }
}

fn display_mr(mr: &MergeRequest, use_hyperlink: bool) -> String {
    let format_text = format!("[{}] {} - {}", mr.state(), mr.title(), mr.source_branch());
    if use_hyperlink {
        return hyperlink(mr.web_url(), format_text.as_str());
    }
    format_text
}

fn task_hyperlink(text: &str) -> String {
    let url = "https://jira.vk.team/browse/";
    hyperlink(format!("{}{}", url, text).as_str(), text)
}

fn hyperlink(url: &str, text: &str) -> String {
    format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", url, text)
}
