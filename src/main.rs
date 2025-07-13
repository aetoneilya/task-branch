mod client;
mod config;

use config::Config;
use client::jira::JiraClient;
use clap::{ArgAction, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand, Debug, Clone)]
enum Commands {
    Create {
        task: String,
        name: String,
    },
    Set {
        #[arg(long, action = ArgAction::SetTrue)]
        piska: bool,
    },
}

fn search_jira() {
    let config = Config::from_env();
    println!("Token: {}", config.token);

    let client = JiraClient::new(config.base_url, config.token);

    match client.search_issues("assignee = currentUser() AND resolution = Unresolved") {
        Ok(issues) => {
            for issue in issues {
                println!("{:#?}", issue);
            }
        }
        Err(e) => eprintln!("Error fetching issues: {}", e),
    }
}

fn main() {
    let args = Args::parse();

    println!("{:?}", args);

    search_jira();
}
