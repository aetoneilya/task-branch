pub mod note;
pub mod task;

use std::fs::{File, OpenOptions};

/// Serialises note writes between concurrent `wt` processes.
pub fn lock() -> std::io::Result<File> {
    let dir = crate::config::cache_dir();
    std::fs::create_dir_all(&dir)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("lock"))?;
    file.lock()?;
    Ok(file)
}
