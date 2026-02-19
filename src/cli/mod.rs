use crate::client::gitlab::MergeRequest;
use crate::client::jira::{JiraIssue, JiraSubtask};
use crate::config::load_config;
use crate::core::JiraGitlabState;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use dialoguer::theme::ColorfulTheme;
use dialoguer::FuzzySelect;
use std::collections::HashMap;
use std::path::PathBuf;
use std::{fmt, io};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    command: CliCommands,

    #[arg(long = "repo", short = 'r', default_value = ".", global = true)]
    repo_path: PathBuf,
}

#[derive(Subcommand, Debug)]
enum CliCommands {
    List {
        #[command(subcommand)]
        list_option: Option<ListOption>,
    },
    Switch {
        #[arg(long, short = 'k')]
        key: Option<String>,
    },
    Info {
        #[arg(long, short = 'k')]
        key: String,
    },
    Tui {},

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

    let state = JiraGitlabState::new(
        cli.repo_path,
        config.jira,
        config.gitlab,
    )
    .map_err(|err| err.to_string())?;

    match cli.command {
        CliCommands::List { list_option } => cli_list(&state, list_option)?,
        CliCommands::Switch { key } => select_mr(&state, key)?,
        CliCommands::Info { key } => cli_info(&state, key)?,
        CliCommands::Tui {} => todo!(),
        CliCommands::Completions { shell } => cli_completion(shell)?,
    }

    Ok(())
}

//TODO fix returns completion for jira-gitlab not jg
fn cli_completion(shell: Shell) -> Result<(), String> {
    let mut cmd = Cli::command(); // get clap Command
    let bin_name = cmd.get_name().to_string();
    generate(shell, &mut cmd, bin_name, &mut io::stdout());
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
