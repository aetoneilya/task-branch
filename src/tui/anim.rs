//! Spinners and bars for what is still loading; the event loop redraws often
//! enough for them to move while `App::animating` holds.

pub(super) use crate::table::look::bar;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

/// How often a frame of an animation changes.
pub(super) const FRAME: Duration = Duration::from_millis(80);

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

static START: LazyLock<Instant> = LazyLock::new(Instant::now);

fn frame() -> usize {
    (START.elapsed().as_millis() / FRAME.as_millis()) as usize
}

/// The spinner's current frame.
pub(super) fn spinner() -> &'static str {
    SPINNER[frame() % SPINNER.len()]
}

/// `width` cells with a short block going back and forth: progress unknown.
pub(super) fn bounce(width: usize) -> String {
    const BLOCK: usize = 3;
    let span = width.saturating_sub(BLOCK).max(1);
    let step = frame() % (2 * span);
    let at = if step < span { step } else { 2 * span - step };
    (0..width)
        .map(|i| {
            if (at..at + BLOCK).contains(&i) {
                '▰'
            } else {
                '▱'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_keep_their_width() {
        assert_eq!(bar(0.0, 4), "▱▱▱▱");
        assert_eq!(bar(0.5, 4), "▰▰▱▱");
        assert_eq!(bar(2.0, 4), "▰▰▰▰");
        assert_eq!(bounce(10).chars().count(), 10);
        assert_eq!(bounce(10).chars().filter(|&c| c == '▰').count(), 3);
    }
}
