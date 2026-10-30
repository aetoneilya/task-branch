pub mod agent;
pub mod gitlab;
pub mod herdr;
pub mod jira;
pub mod sessions;
pub mod worktree;

/// `f` of every item on at most `limit` threads, in the items' order; None for
/// an item whose `f` panicked.
pub fn parallel<T: Sync, R: Send>(
    items: &[T],
    limit: usize,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<Option<R>> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<R>>> = Mutex::new(items.iter().map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..limit.clamp(1, items.len().max(1)) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    let result =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(item))).ok();
                    if let Ok(mut results) = results.lock() {
                        results[i] = result;
                    }
                }
            });
        }
    });
    results.into_inner().unwrap_or_default()
}

/// The value of a scoped thread whose work can fail; a panic in it becomes an
/// error, `<what> panicked`, instead of taking the caller down.
pub fn joined<T>(
    handle: std::thread::ScopedJoinHandle<'_, Result<T, String>>,
    what: &str,
) -> Result<T, String> {
    handle
        .join()
        .unwrap_or_else(|_| Err(format!("{what} panicked")))
}

#[cfg(test)]
mod tests {
    use super::parallel;

    #[test]
    fn parallel_keeps_order_and_survives_a_panic() {
        let items: Vec<u32> = (0..50).collect();
        let out = parallel(&items, 4, |&n| {
            assert!(n != 7, "seven");
            n * 2
        });
        assert_eq!(out.len(), 50);
        assert_eq!(out[7], None);
        assert_eq!(out[49], Some(98));
        assert!(parallel(&Vec::<u32>::new(), 4, |&n| n).is_empty());
    }
}
