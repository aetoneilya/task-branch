use std::collections::HashMap;
use crate::client::gitlab::MergeRequest;
use crate::client::jira::{JiraIssue, JiraSubtask};
use crate::state::State;
use CliCommands::{Mr, Tasks};
use clap::{Parser, Subcommand, ValueEnum};
use dialoguer::Select;
use dialoguer::theme::ColorfulTheme;
use std::fmt;
use std::thread::sleep;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<CliCommands>,
}

#[derive(Subcommand, Debug)]
enum CliCommands {
    Mr {
        #[command(subcommand)]
        mr_cmd: Option<MrCommands>,
    },
    Tasks {
        #[command(subcommand)]
        tasks_cmd: Option<TasksCommand>,
    },
}

#[derive(Subcommand, Debug)]
enum MrCommands {
    List {
        #[arg(long, short, action = clap::ArgAction::SetTrue)]
        open: bool,
    },

    Create {
        #[arg(short, long)]
        title: String,

        #[arg(short, long)]
        description: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum TasksCommand {
    List {
        #[arg(long, value_enum, default_value_t = MrOption::None)]
        mr: MrOption,
    },
    Create {
        #[arg(short, long)]
        title: String,

        #[arg(short, long)]
        description: Option<String>,
    },
}

#[derive(ValueEnum, Clone, Debug)]
enum MrOption {
    None,
    My,
    All,
}

pub struct CliCommandProcessor {
    state: State,
}

impl CliCommandProcessor {
    pub fn new(state: State) -> Self {
        Self { state }
    }

    pub fn process(&self, cmd: Cli) {
        let cmd = cmd.command.expect("Command not found");

        match cmd {
            Mr { mr_cmd } => self.process_mr_commands(mr_cmd),
            Tasks { tasks_cmd } => self.process_tasks_commands(tasks_cmd),
        }
    }

    fn process_mr_commands(&self, mr_cmd: Option<MrCommands>) {
        if let None = mr_cmd {
            self.select_mr();
            return;
        }

        match mr_cmd.unwrap() {
            MrCommands::List { open } => self.list_mrs(),
            MrCommands::Create { title, description } => {
                println!("creating mrs {title}");
            }
        }
    }

    fn list_mrs(&self) {
        let mrs = self
            .state
            .list_merge_request()
            .expect("Failed to list merge requests");

        println!("Merge requests:");
        for mr in mrs {
            println!("{:#?}", mr)
        }
    }

    fn create_mr(&self, title: String, description: Option<String>) {}

    fn select_mr(&self) {
        let mrs = self
            .state
            .list_merge_request()
            .expect("Failed to list merge requests");

        let selection = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Merge Requests:")
            .items(&mrs)
            .interact()
            .expect("Failed to select merge requests");

        println!("Checkout to {}", mrs[selection].source_branch());

        let branch = mrs[selection].source_branch();
        // sleep(Duration::from_secs(1));
        self.state
            .checkout_to(branch)
            .expect("Failed to checkout branch");
    }

    fn process_tasks_commands(&self, tasks_cmd: Option<TasksCommand>) {
        if let None = tasks_cmd {
            self.list_issues(MrOption::None);
            return;
        }

        let tasks_cmd = tasks_cmd.unwrap();

        match tasks_cmd {
            TasksCommand::List { mr } => self.list_issues(mr),
            TasksCommand::Create { .. } => println!("not implemented yet"),
        }
    }

    fn list_issues(&self, mr: MrOption) {
        let query = "assignee = currentUser() AND resolution = Unresolved";
        let issues = self.state.search_issues(query);

        if let Err(e) = issues {
            println!("Error while issues search: {}", e);
            return;
        }

        let issues = issues.unwrap();
        println!("Current issues:");
        for issue in issues {
            print!("{}", issue);
        }
    }
}

impl fmt::Display for JiraIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = self
            .fields
            .status
            .as_ref()
            .map(|s| s.name.as_str())
            .unwrap_or("unknown");

        let link = task_hyperlink(self.key.as_ref());
        writeln!(f, "📌 {}  [{}] \t{}", link, status, self.fields.summary)?;

        if let Some(subtasks) = &self.fields.subtasks {
            for subtask in subtasks {
                writeln!(f, "   └─ {}", subtask)?;
            }
        }

        Ok(())
    }
}

impl fmt::Display for JiraSubtask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = self
            .fields
            .status
            .as_ref()
            .map(|s| s.name.as_str())
            .unwrap_or("unknown");

        let link = task_hyperlink(self.key.as_ref());
        write!(f, "🔹 {}  [{}] \t{}", link, status, self.fields.summary)
    }
}

impl fmt::Display for MergeRequest {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "[{}] {}", self.state(), self.title())
    }
}

fn task_hyperlink(text: &str) -> String {
    let url = "https://jira.vk.team/browse/";
    format!("\x1b]8;;{}{}\x1b\\{}\x1b]8;;\x1b\\", url, text, text)
}
