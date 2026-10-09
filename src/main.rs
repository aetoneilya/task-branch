mod cli;
mod client;
mod config;
mod core;
mod tui;

fn main() {
    if let Err(err) = cli::run() {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
}
