//! Step lines for slow work: a spinner while it runs, then ✓ or ✗ with the time.

use std::fmt::Display;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The step lines go to stderr: colored when it takes colors.
static COLOR: LazyLock<bool> = LazyLock::new(|| super::colors(&std::io::stderr()));

/// Runs `work` under a spinner: for steps that print nothing themselves.
pub fn step<T, E: Display>(label: &str, work: impl FnOnce() -> Result<T, E>) -> Result<T, String> {
    let started = Instant::now();
    let done = Arc::new(AtomicBool::new(false));
    let spinner = std::io::stderr().is_terminal().then(|| {
        let (done, label) = (done.clone(), label.to_string());
        std::thread::spawn(move || {
            let mut frame = 0;
            while !done.load(Ordering::Relaxed) {
                eprint!(
                    "\r\x1b[2K{} {label}  {:.1}s",
                    FRAMES[frame % FRAMES.len()],
                    started.elapsed().as_secs_f64()
                );
                let _ = std::io::stderr().flush();
                frame += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
        })
    });
    let result = work();
    done.store(true, Ordering::Relaxed);
    if let Some(spinner) = spinner {
        let _ = spinner.join();
        eprint!("\r\x1b[2K");
    }
    finish(label, started, result)
}

/// Runs `work` that draws its own progress (git checkout) below a header line.
pub fn loud_step<T, E: Display>(
    label: &str,
    work: impl FnOnce() -> Result<T, E>,
) -> Result<T, String> {
    let started = Instant::now();
    eprintln!("▸ {label}");
    finish(label, started, work())
}

fn finish<T, E: Display>(label: &str, started: Instant, result: Result<T, E>) -> Result<T, String> {
    let took = started.elapsed().as_secs_f64();
    let (mark, color) = if result.is_ok() {
        ("✓", "32")
    } else {
        ("✗", "31")
    };
    if *COLOR {
        eprintln!("\x1b[{color}m{mark}\x1b[0m {label}  {took:.1}s");
    } else {
        eprintln!("{mark} {label}  {took:.1}s");
    }
    result.map_err(|e| e.to_string())
}
