use crate::client::git::GitError;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) type Result<T> = std::result::Result<T, GitError>;

#[derive(Debug)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: Option<String>,
}

impl Worktree {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    }
}

fn git_output(path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").arg("-C").arg(path).args(args).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitError::NonZeroExit(format!(
            "(git {}) {}",
            args.first().unwrap_or(&""),
            stderr.lines().next().unwrap_or("").trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Root of the main checkout, also when `path` is inside a linked worktree.
pub fn main_checkout(path: &Path) -> Result<PathBuf> {
    let common_dir = git_output(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common_dir = PathBuf::from(common_dir);
    Ok(common_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or(common_dir))
}

/// Root of the worktree containing `path`.
pub fn worktree_root(path: &Path) -> Result<PathBuf> {
    git_output(path, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

/// Linked worktrees of the repository, without the main checkout.
pub fn linked_worktrees(repo: &Path) -> Result<Vec<Worktree>> {
    let output = git_output(repo, &["worktree", "list", "--porcelain"])?;

    let worktrees = output
        .split("\n\n")
        .filter_map(|block| {
            let mut path = None;
            let mut branch = None;
            for line in block.lines() {
                if let Some(p) = line.strip_prefix("worktree ") {
                    path = Some(PathBuf::from(p));
                } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
                    branch = Some(b.to_string());
                }
            }
            path.map(|path| Worktree { path, branch })
        })
        .skip(1)
        .collect();

    Ok(worktrees)
}

pub fn changed_files(path: &Path) -> Result<usize> {
    let output = git_output(path, &["status", "--porcelain"])?;
    Ok(output.lines().filter(|l| !l.is_empty()).count())
}

/// Commits of `branch` that are not on origin: compared with origin/<branch> when it
/// exists, otherwise with origin/HEAD (the branch was never pushed).
pub fn unpushed_commits(path: &Path, branch: &str) -> Result<usize> {
    let remote_branch = format!("origin/{branch}");
    let base = if git_output(path, &["rev-parse", "--verify", "--quiet", &remote_branch]).is_ok() {
        remote_branch
    } else {
        "origin/HEAD".to_string()
    };
    let range = format!("{base}..refs/heads/{branch}");
    let count = git_output(path, &["rev-list", "--count", &range])?;
    Ok(count.parse().unwrap_or(0))
}

pub fn branch_head(path: &Path, branch: &str) -> Result<String> {
    git_output(path, &["rev-parse", &format!("refs/heads/{branch}")])
}

pub fn remove_worktree(repo: &Path, worktree: &Path, force: bool) -> Result<()> {
    let path = worktree.to_str().ok_or(GitError::RepoPathNotUtf8())?;
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(path);
    git_output(repo, &args).map(|_| ())
}

pub fn delete_branch(repo: &Path, branch: &str, force: bool) -> Result<()> {
    let flag = if force { "-D" } else { "-d" };
    git_output(repo, &["branch", flag, branch]).map(|_| ())
}
