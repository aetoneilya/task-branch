mod cli;
mod client;
mod config;
mod table;
mod trace;
mod tui;
mod vault;

fn main() {
    let _total = trace::span("total");
    if let Err(err) = cli::run() {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
}
