pub mod gitlab;
pub mod herdr;
pub mod jira;
pub mod sessions;
pub mod worktree;

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
