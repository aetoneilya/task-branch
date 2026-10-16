//! Opt-in timing of the slow phases: `WT_TRACE=1 wt status` prints how long
//! each fetch, the matching and the rendering took to stderr.

use std::sync::LazyLock;
use std::time::Instant;

static ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("WT_TRACE").is_some_and(|v| !v.is_empty() && v != "0"));
static START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Prints `name` and its duration when dropped, with WT_TRACE set.
pub struct Span {
    name: String,
    start: Instant,
}

pub fn span(name: impl Into<String>) -> Option<Span> {
    if !*ENABLED {
        return None;
    }
    LazyLock::force(&START);
    Some(Span {
        name: name.into(),
        start: Instant::now(),
    })
}

/// `f`, timed as `name`.
pub fn time<T>(name: &str, f: impl FnOnce() -> T) -> T {
    let _span = span(name);
    f()
}

impl Drop for Span {
    fn drop(&mut self) {
        let end = Instant::now();
        eprintln!(
            "[wt] {:>6}ms  {:>6}ms  {}",
            end.duration_since(self.start).as_millis(),
            end.duration_since(*START).as_millis(),
            self.name
        );
    }
}
