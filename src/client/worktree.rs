use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitError {
    #[error("failed to execute git command: {0}")]
    Io(#[from] std::io::Error),

    #[error("git command exited with status {0}")]
    NonZeroExit(String),

    #[error("repo path not utf8")]
    RepoPathNotUtf8,

    #[error("failed to obtain current branch")]
    CurrentBranchNotFound,
}

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
    git_stdout(path, args).map(|out| out.trim_start().to_string())
}

/// git's stdout without its trailing newline: leading spaces mean something in
/// `git status --short`.
fn git_stdout(path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitError::NonZeroExit(format!(
            "(git {}) {}",
            args.first().unwrap_or(&""),
            stderr.lines().next().unwrap_or("").trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
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

pub fn current_branch(path: &Path) -> Result<String> {
    let branch = git_output(path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch.is_empty() {
        return Err(GitError::CurrentBranchNotFound);
    }
    Ok(branch)
}

pub fn changed_files(path: &Path) -> Result<usize> {
    let output = git_output(path, &["status", "--porcelain"])?;
    Ok(output.lines().filter(|l| !l.is_empty()).count())
}

/// `git status --short`: one line per changed file.
pub fn short_status(path: &Path) -> Result<String> {
    git_stdout(path, &["status", "--short"])
}

/// Whether origin has `branch`. Branches are created --no-track: origin/<branch>
/// is what unpushed commits are counted against.
pub fn on_origin(path: &Path, branch: &str) -> bool {
    let remote_branch = format!("origin/{branch}");
    git_output(path, &["rev-parse", "--verify", "--quiet", &remote_branch]).is_ok()
}

/// `git log --oneline` of `range`, newest first, at most `max` commits.
pub fn log_oneline(path: &Path, range: &str, max: usize) -> Result<String> {
    git_output(path, &["log", "--oneline", &format!("-{max}"), range])
}

/// Commits of `branch` that are not on origin: compared with origin/<branch> when it
/// exists, otherwise with origin/HEAD (the branch was never pushed).
pub fn unpushed_commits(path: &Path, branch: &str) -> Result<usize> {
    let base = if on_origin(path, branch) {
        format!("origin/{branch}")
    } else {
        "origin/HEAD".to_string()
    };
    let range = format!("{base}..refs/heads/{branch}");
    let count = git_output(path, &["rev-list", "--count", &range])?;
    Ok(count.parse().unwrap_or(0))
}

pub fn last_commit_time(path: &Path) -> Result<i64> {
    let out = git_output(path, &["log", "-1", "--format=%ct"])?;
    Ok(out.parse().unwrap_or(0))
}

pub fn branch_head(path: &Path, branch: &str) -> Result<String> {
    git_output(path, &["rev-parse", &format!("refs/heads/{branch}")])
}

pub fn remove_worktree(repo: &Path, worktree: &Path, force: bool) -> Result<()> {
    let path = worktree.to_str().ok_or(GitError::RepoPathNotUtf8)?;
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

pub fn branch_exists(repo: &Path, branch: &str) -> bool {
    ["refs/heads/", "refs/remotes/origin/"]
        .iter()
        .any(|prefix| {
            git_output(
                repo,
                &[
                    "show-ref",
                    "--verify",
                    "--quiet",
                    &format!("{prefix}{branch}"),
                ],
            )
            .is_ok()
        })
}

pub fn worktree_dir(main: &Path, name: &str) -> PathBuf {
    main.join(".claude/worktrees").join(name)
}

/// How to create `<main checkout>/.claude/worktrees/<name>` on `branch`: an
/// existing local or remote branch is checked out, otherwise the branch is
/// created from a freshly fetched origin/HEAD.
pub struct WorktreePlan {
    /// The origin branch to fetch first.
    pub fetch: Option<String>,
    /// Arguments of `git worktree add`.
    pub add: Vec<String>,
}

pub fn plan_worktree(main: &Path, name: &str, branch: &str) -> Result<WorktreePlan> {
    let dir = worktree_dir(main, name);
    let path = dir.to_str().ok_or(GitError::RepoPathNotUtf8)?.to_string();
    if branch_exists(main, branch) {
        return Ok(WorktreePlan {
            add: vec!["worktree".into(), "add".into(), path, branch.into()],
            fetch: None,
        });
    }
    let base = git_output(
        main,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .unwrap_or_else(|_| "HEAD".to_string());
    Ok(WorktreePlan {
        fetch: base.strip_prefix("origin/").map(str::to_string),
        add: ["worktree", "add", "--no-track", "-b", branch, &path, &base]
            .map(String::from)
            .to_vec(),
    })
}

pub fn fetch(main: &Path, remote_branch: &str) -> Result<()> {
    git_output(main, &["fetch", "-q", "origin", remote_branch]).map(|_| ())
}

/// The plan's `git worktree add`; `progress` leaves git's own checkout progress
/// on the terminal.
pub fn add_worktree(main: &Path, plan: &WorktreePlan, progress: bool) -> Result<()> {
    if !progress {
        let args: Vec<&str> = plan.add.iter().map(String::as_str).collect();
        return git_output(main, &args).map(|_| ());
    }
    let status = Command::new("git")
        .arg("-C")
        .arg(main)
        .args(&plan.add)
        .stdout(std::process::Stdio::null())
        .status()?;
    if !status.success() {
        return Err(GitError::NonZeroExit(
            "(git worktree add) failed".to_string(),
        ));
    }
    Ok(())
}

/// The worktree `plan_worktree` describes; an existing directory is reused as is.
pub fn create_worktree(main: &Path, name: &str, branch: &str) -> Result<PathBuf> {
    let dir = worktree_dir(main, name);
    if dir.exists() {
        return Ok(dir);
    }
    let plan = plan_worktree(main, name, branch)?;
    if let Some(remote_branch) = &plan.fetch {
        fetch(main, remote_branch)?;
    }
    add_worktree(main, &plan, false)?;
    copy_worktree_includes(main, &dir);
    Ok(dir)
}

/// Untracked local files listed in `.worktreeinclude` (e.g. `.claude/settings.local.json`
/// with the memory mapping), copied the way Claude Code does for its own worktrees.
pub fn copy_worktree_includes(main: &Path, dir: &Path) {
    let Ok(list) = std::fs::read_to_string(main.join(".worktreeinclude")) else {
        return;
    };
    for entry in list.lines().map(str::trim) {
        if entry.is_empty() || entry.starts_with('#') {
            continue;
        }
        let (from, to) = (main.join(entry), dir.join(entry));
        if from.exists() && !to.exists() {
            let _ = copy_recursive(&from, &to);
        }
    }
}

fn copy_recursive(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

/// The worktree's git dir, `.git/worktrees/<name>`: never committed, removed
/// with the worktree.
pub fn git_dir(path: &Path) -> Result<PathBuf> {
    git_output(path, &["rev-parse", "--absolute-git-dir"]).map(PathBuf::from)
}

/// Local and remote branch names (without `origin/`), deduplicated.
pub fn all_branches(repo: &Path) -> Result<Vec<String>> {
    let output = git_output(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads",
            "refs/remotes/origin",
        ],
    )?;
    let mut branches: Vec<String> = output
        .lines()
        .filter_map(|r| {
            r.strip_prefix("refs/heads/")
                .or_else(|| r.strip_prefix("refs/remotes/origin/"))
        })
        .filter(|b| *b != "HEAD")
        .map(str::to_string)
        .collect();
    branches.sort();
    branches.dedup();
    Ok(branches)
}
