use regex::Regex;
use std::path::Path;
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitError {
    #[error("failed to execute git command: {0}")]
    Io(#[from] std::io::Error),

    #[error("git command exited with status {0}")]
    NonZeroExit(String),

    #[error("branch '{0}' not found locally or remotely")]
    BranchNotFound(String),

    #[error("repo path not utf8")]
    RepoPathNotUtf8(),

    #[error("failed to obtain current branch")]
    CurrentBranchNotFound(),
}

pub(crate) type Result<T> = std::result::Result<T, GitError>;

fn run_git(path: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(path).args(args);
    command
}

pub fn local_branch_exists(path: &Path, branch: &str) -> Result<bool> {
    let arg = &["show-ref", "--verify", &format!("refs/heads/{}", branch)];

    let exit_status = run_git(path, arg).status();

    Ok(exit_status?.success())
}

pub fn local_branches(path: &Path) -> Result<Vec<String>> {
    let output = run_git(path, &["branch", "--format=%(refname:short)"]).output()?;

    if !output.status.success() {
        return Err(GitError::NonZeroExit(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    let branches = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();

    Ok(branches)
}

fn remote_branch_exists(path: &Path, branch: &str) -> Result<bool> {
    let arg = &["ls-remote", "--heads", "origin", branch];
    let output = run_git(path, arg).output()?;

    Ok(!output.stdout.is_empty())
}

pub fn checkout_branch(path: &Path, branch: &str) -> Result<()> {
    if local_branch_exists(path, branch)? {
        run_git(path, &["checkout", branch]).status()?;
        return Ok(());
    }

    if remote_branch_exists(path, branch)? {
        run_git(path, &["fetch", "origin", branch]).status()?;
        run_git(
            path,
            &[
                "checkout",
                "-b",
                branch,
                "--track",
                &format!("origin/{}", branch),
            ],
        )
        .status()?;

        return Ok(());
    }

    Err(GitError::BranchNotFound(branch.to_string()))
}

pub fn key_from_branch_name(branch_name: &str) -> Option<&str> {
    let re = Regex::new(r"([A-Z]+-\d+)").unwrap();
    re.captures(branch_name)
        .and_then(|caps| caps.get(1).map(|m| m.as_str()))
}

pub fn current_branch(path: &Path) -> Result<String> {
    let output = run_git(path, &["rev-parse", "--abbrev-ref", "HEAD"]).output()?;
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        return Err(GitError::CurrentBranchNotFound());
    }
    Ok(branch)
}
