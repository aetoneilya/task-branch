mod cli;
mod client;
mod config;
mod core;

fn main() {
    if let Err(err) = cli::run() {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
}
