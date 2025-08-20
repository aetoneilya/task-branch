mod cli;
mod client;
mod config;
mod state;

use crate::cli::{Cli, CliCommandProcessor};
use crate::state::State;
use clap::Parser;
use dotenv::dotenv;

fn main() {
    dotenv().ok();

    let state = State::new();
    let cli_processor = CliCommandProcessor::new(state);

    let args = Cli::parse();
    cli_processor.process(args)
}
