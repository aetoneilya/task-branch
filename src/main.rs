mod client;
mod config;
mod state;
mod cli;

use crate::client::git::checkout_branch;
use clap::{Parser, Subcommand};
use client::gitlab::GitLabClient;
use dialoguer::{theme::ColorfulTheme, Select};
use dotenv::dotenv;
use std::env;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;
use crate::cli::{Cli, CliCommandProcessor};
use crate::state::State;

fn main() {
    dotenv().ok();
    env::set_current_dir(Path::new("/Users/i.udalov/code/vk/zen"))
        .expect("Failed to change working directory");
    
    let state = State::new();
    let cli_processor = CliCommandProcessor::new(state);
    
    let args = Cli::parse();
    cli_processor.process(args)
}
//
// fn main_menu() {
//     let items = vec!["User Management", "System Settings", "Quit"];
//
//     loop {
//         let selection = Select::with_theme(&ColorfulTheme::default())
//             .with_prompt("Main Menu")
//             .items(&items)
//             .interact()
//             .unwrap();
//
//         match selection {
//             0 => user_management_menu(),
//             1 => system_settings_menu(),
//             2 => {
//                 println!("Goodbye!");
//                 return;
//             }
//             _ => println!("Invalid selection"),
//         }
//     }
// }
//
// fn user_management_menu() {
//     let items = vec!["List Users", "Add User", "Back"];
//
//     let selection = Select::with_theme(&ColorfulTheme::default())
//         .with_prompt("User Management")
//         .items(&items)
//         .interact()
//         .unwrap();
//
//     match selection {
//         0 => println!("Listing users..."),
//         1 => println!("Adding user..."),
//         _ => (),
//     }
// }
//
// fn system_settings_menu() {
//     let items = vec!["Configure Network", "Update System", "Back"];
//
//     let selection = Select::with_theme(&ColorfulTheme::default())
//         .with_prompt("System Settings")
//         .items(&items)
//         .interact()
//         .unwrap();
//
//     match selection {
//         0 => println!("Configuring network..."),
//         1 => println!("Updating system..."),
//         _ => (),
//     }
// }
