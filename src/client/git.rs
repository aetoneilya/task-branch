use std::{fmt, process::Command};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitError {
    #[error("failed to execute git command: {0}")]
    Io(#[from] std::io::Error),

    #[error("git command exited with status {0}")]
    NonZeroExit(String),

    #[error("branch '{0}' not found locally or remotely")]
    BranchNotFound(String),
}

pub(crate) type Result<T> = std::result::Result<T, GitError>;

fn run_git(args: &[&str]) -> Result<()> {
    let status = Command::new("git").args(args).status()?;

    if status.success() {
        Ok(())
    } else {
        Err(GitError::NonZeroExit(format!("{:?}", args)))
    }
}

fn local_branch_exists(branch: &str) -> Result<bool> {
    let status = Command::new("git")
        .args(&["show-ref", "--verify", &format!("refs/heads/{}", branch)])
        .status()?;

    Ok(status.success())
}

fn remote_branch_exists(branch: &str) -> Result<bool> {
    let output = Command::new("git")
        .args(&["ls-remote", "--heads", "origin", branch])
        .output()?;

    Ok(!output.stdout.is_empty())
}

pub fn checkout_branch(branch: &str) -> Result<()> {
    if local_branch_exists(branch)? {
        run_git(&["checkout", branch])?;
        return Ok(());
    }

    if remote_branch_exists(branch)? {
        run_git(&["fetch", "origin", branch])?;
        run_git(&["checkout", "-b", branch, "--track", &format!("origin/{}", branch)])?;
        return Ok(());
    }

    Err(GitError::BranchNotFound(branch.to_string()))
}