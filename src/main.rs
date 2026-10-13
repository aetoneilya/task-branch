mod cli;
mod client;
mod config;
mod core;
mod table;
mod tui;
mod vault;

fn main() {
    if let Err(err) = cli::run() {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
}
