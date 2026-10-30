#![cfg(unix)]

use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    config: PathBuf,
    capture: PathBuf,
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

impl Fixture {
    fn new(workflow: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "wt-agent-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("vault/tasks")).unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        let root = root.canonicalize().unwrap();
        let repo = root.join("repo with space");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(
            &repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-qm",
                "fixture",
            ],
        );
        let config = root.join("config.toml");
        let quote = |p: &Path| toml::Value::String(p.to_string_lossy().into_owned());
        fs::write(&config, format!("[vault]\npath = {}\n[workflow]\n{workflow}\n[[project]]\nname = \"test\"\ntask_dirs = [\"tasks\"]\n[[project.repo]]\npath = {}\n", quote(&root.join("vault")), quote(&repo))).unwrap();
        let capture = root.join("commands.jsonl");
        let fake = r#"#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
with open(os.environ['WT_TEST_CAPTURE'], 'a') as f:
    f.write(json.dumps(args) + '\n')
result = {'agents': [], 'workspaces': []}
if args[:2] == ['agent', 'list'] and os.path.exists(os.environ['WT_TEST_AGENTS']):
    with open(os.environ['WT_TEST_AGENTS']) as f:
        result['agents'] = json.load(f)
if args[:2] == ['workspace', 'create']:
    result = {'tab': {'tab_id': 'test-tab'}, 'root_pane': {'pane_id': 'test-pane'}}
elif args[:2] == ['tab', 'create']:
    result = {'tab': {'tab_id': 'test-tab'}, 'root_pane': {'pane_id': 'test-pane'}}
elif args[:2] == ['pane', 'split']:
    result = {'pane': {'pane_id': 'test-pane'}}
print(json.dumps({'result': result}))
"#;
        for (name, text) in [("herdr", fake), ("sh", "#!/bin/sh\nexit 0\n")] {
            let path = root.join("bin").join(name);
            fs::write(&path, text).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self {
            root,
            repo,
            config,
            capture,
        }
    }

    fn command(&self, program: &str) -> Command {
        let path = format!(
            "{}:{}",
            self.root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut command = Command::new(program);
        command
            .current_dir(&self.repo)
            .env("PATH", path)
            .env("HOME", self.root.join("home"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("WORKTABLE_CONFIG", &self.config)
            .env("CLAUDE_CONFIG_DIR", self.root.join("claude"))
            .env("CODEX_HOME", self.root.join("codex"))
            .env("HERDR_ENV", "1")
            .env("HERDR_WORKSPACE_ID", "test-space")
            .env("WT_TEST_CAPTURE", &self.capture)
            .env("WT_TEST_AGENTS", self.root.join("agents.json"));
        command
    }

    fn run(&self, args: &[&str]) -> Vec<Vec<String>> {
        let _ = fs::remove_file(&self.capture);
        let output = self
            .command(env!("CARGO_BIN_EXE_wt"))
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "args={args:?}\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        self.commands()
    }

    fn commands(&self) -> Vec<Vec<String>> {
        fs::read_to_string(&self.capture)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Exercise the real TUI in a terminal, with only herdr replaced by the fixture.
    fn tui(&self, navigation: &str, answer: &str) -> Vec<Vec<String>> {
        self.tui_with_tree(navigation, answer, "")
    }

    fn tui_with_tree(&self, navigation: &str, answer: &str, tree_answer: &str) -> Vec<Vec<String>> {
        let _ = fs::remove_file(&self.capture);
        let driver = r#"
import fcntl, os, pty, select, struct, subprocess, sys, termios, time
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 180, 0, 0))
process = subprocess.Popen([sys.argv[1]], stdin=slave, stdout=slave, stderr=slave)
os.close(slave)
screen = b''
def wait_for(text):
    global screen
    deadline = time.monotonic() + 10
    while text not in screen:
        if time.monotonic() >= deadline or process.poll() is not None:
            raise AssertionError('waiting for %r: %r' % (text, screen[-6000:]))
        if select.select([master], [], [], 0.1)[0]:
            screen += os.read(master, 65536)
try:
    wait_for(b'new task')
    os.write(master, sys.argv[2].encode() + b'n')
    wait_for(b'New agent')
    screen = b''
    # Crossterm decodes Esc+Esc as Escape without waiting for another byte.
    # A bare Esc followed by the cleanup q could otherwise become Alt-q.
    os.write(master, b'\x1b\x1b' if sys.argv[3] == '\x1b' else sys.argv[3].encode())
    if sys.argv[4]:
        wait_for(b'Worktree')
        screen = b''
        os.write(master, sys.argv[4].encode())
    if sys.argv[3] != '\x1b':
        wait_for(b'tab task')
    # Drain the PTY while closing; refresh output can fill its buffer.
    # Cancellation may finish between frames, replacing its transient footer.
    deadline, next_quit = time.monotonic() + 10, 0
    while process.poll() is None:
        assert time.monotonic() < deadline, screen[-6000:]
        if time.monotonic() >= next_quit:
            os.write(master, b'q')
            next_quit = time.monotonic() + 0.25
        if select.select([master], [], [], 0.1)[0]:
            try:
                screen += os.read(master, 65536)
            except OSError:
                process.wait(timeout=1)
    assert process.returncode == 0, screen
finally:
    if process.poll() is None:
        process.kill()
        process.wait()
    os.close(master)
"#;
        let output = self
            .command("python3")
            .env("TERM", "xterm-256color")
            .env_remove("WT_POPUP")
            .args([
                "-c",
                driver,
                env!("CARGO_BIN_EXE_wt"),
                navigation,
                answer,
                tree_answer,
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.commands()
    }

    fn tree(&self) -> PathBuf {
        let tree = self.repo.join(".claude/worktrees/task");
        git(
            &self.repo,
            &[
                "worktree",
                "add",
                "-qb",
                "UCP-1-task",
                tree.to_str().unwrap(),
            ],
        );
        tree
    }

    fn live_agent(&self, kind: &str, tree: &Path) {
        fs::write(
            self.root.join("agents.json"),
            json!([{
                "name":"already-open", "agent_status":"idle", "cwd":tree,
                "pane_id":"old-pane", "tab_id":"old-tab", "workspace_id":"old-space",
                "agent":kind, "agent_session":{"value":"shared"}
            }])
            .to_string(),
        )
        .unwrap();
    }

    fn sessions(&self, tree: &Path, both: bool) {
        let note = if both {
            "---\ntickets: [UCP-1]\nsessions: [\"shared task 2026-10-29\", \"codex:shared task 2026-10-29\"]\n---\nContext\n"
        } else {
            "---\ntickets: [UCP-1]\nsessions: [\"codex:shared task 2026-10-29\"]\n---\nContext\n"
        };
        fs::write(self.root.join("vault/tasks/task.md"), note).unwrap();
        let slug: String = tree
            .to_string_lossy()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let claude = self.root.join("claude/projects").join(slug);
        fs::create_dir_all(&claude).unwrap();
        if both {
            fs::write(
                claude.join("shared.jsonl"),
                json!({"cwd":tree,"timestamp":"2026-10-29T10:00:00Z"}).to_string(),
            )
            .unwrap();
        }
        let codex = self.root.join("codex/sessions/2026/10/29");
        fs::create_dir_all(&codex).unwrap();
        fs::write(codex.join("rollout-fixture.jsonl"), json!({"type":"session_meta","timestamp":"2026-10-29T10:00:00Z","payload":{"id":"shared","cwd":tree,"source":"cli"}}).to_string()).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn pane_command(commands: &[Vec<String>]) -> &str {
    &commands
        .iter()
        .find(|c| {
            c.first().map(String::as_str) == Some("pane")
                && c.get(1).map(String::as_str) == Some("run")
        })
        .expect("agent launch command")[3]
}

#[test]
fn starts_each_cli_with_the_same_task_brief() {
    for kind in ["claude", "codex"] {
        let fixture = Fixture::new("");
        let commands = fixture.run(&["start", "task", "--no-worktree", "--bg", "--agent", kind]);
        let command = pane_command(&commands);
        assert!(
            command.starts_with(&format!("command {kind} ")),
            "{command}"
        );
        assert!(
            command.contains("Read it and wait for my command."),
            "{command}"
        );
        assert!(command.contains("task note"));
    }
}

#[test]
fn prepared_worktree_command_keeps_the_requested_cli() {
    for kind in ["claude", "codex"] {
        let fixture = Fixture::new("");
        let commands = fixture.run(&["start", "task", "-n", "task", "--bg", "--agent", kind]);
        let command = pane_command(&commands);
        assert!(
            command.contains(&format!("&& command {kind} ")),
            "{command}"
        );
        assert!(command.contains(" prepare "));
    }
}

#[test]
fn resumes_the_owning_cli_even_when_native_ids_collide() {
    for (kind, expected) in [
        ("claude", "command claude '--resume' 'shared'"),
        ("codex", "command codex 'resume' 'shared'"),
    ] {
        let fixture = Fixture::new("");
        let tree = fixture.tree();
        fixture.sessions(&tree, true);
        let commands = fixture.run(&["select", "task", "--bg", "--agent", kind]);
        assert_eq!(pane_command(&commands), expected);
    }
}

#[test]
fn select_automatically_resumes_codex_despite_claude_default() {
    let fixture = Fixture::new("");
    let tree = fixture.tree();
    fixture.sessions(&tree, false);
    let commands = fixture.run(&["select", "task", "--bg"]);
    assert_eq!(pane_command(&commands), "command codex 'resume' 'shared'");
}

#[test]
fn review_uses_the_chosen_cli_and_its_own_model_setting() {
    let fixture = Fixture::new(
        "agent = \"codex\"\nreview_agent = \"claude\"\nreview_model = \"claude-model\"\ncodex_review_model = \"codex-model\"",
    );
    fixture.tree();
    let configured = fixture.run(&["review", "task"]);
    assert!(pane_command(&configured).starts_with("command claude '--model' 'claude-model'"));
    let overridden = fixture.run(&["review", "task", "--agent", "codex"]);
    assert!(pane_command(&overridden).starts_with("command codex '--model' 'codex-model'"));
}

#[test]
fn selecting_codex_does_not_focus_claude_with_the_same_session_id() {
    let fixture = Fixture::new("");
    let tree = fixture.tree();
    fixture.sessions(&tree, true);
    fs::write(
        fixture.root.join("agents.json"),
        json!([{
            "name":"claude-live", "agent_status":"idle", "cwd":tree,
            "pane_id":"old-pane", "tab_id":"old-tab", "workspace_id":"old-space",
            "agent":"claude", "agent_session":{"value":"shared"}
        }])
        .to_string(),
    )
    .unwrap();
    let commands = fixture.run(&["select", "task", "--bg", "--agent", "codex"]);
    assert_eq!(pane_command(&commands), "command codex 'resume' 'shared'");
}

#[test]
fn saved_codex_ids_can_resume_without_a_plain_rollout_file() {
    let fixture = Fixture::new("");
    let tree = fixture.tree();
    fixture.sessions(&tree, false);
    fs::remove_file(
        fixture
            .root
            .join("codex/sessions/2026/10/29/rollout-fixture.jsonl"),
    )
    .unwrap();
    let commands = fixture.run(&["select", "task", "--bg"]);
    assert_eq!(pane_command(&commands), "command codex 'resume' 'shared'");
}

#[test]
fn new_session_bypasses_live_agents_and_saved_conversations() {
    for kind in ["codex", "claude"] {
        let fixture = Fixture::new("");
        let tree = fixture.tree();
        fixture.sessions(&tree, true);
        fixture.live_agent(kind, &tree);
        let brief = fixture.repo.join(".git/worktrees/task/task.md");
        fs::write(&brief, "Task brief").unwrap();
        let commands = fixture.run(&["select", "task", "--new", "--agent", kind]);
        let command = pane_command(&commands);
        assert!(
            command.starts_with(&format!("command {kind} 'Context:")),
            "{command}"
        );
        assert!(command.contains(fixture.root.join("vault/tasks/task.md").to_str().unwrap()));
        assert!(command.contains(brief.to_str().unwrap()));
        assert!(
            !commands
                .iter()
                .any(|c| c.starts_with(&["agent".into(), "focus".into()]))
        );
        assert!(
            commands
                .iter()
                .any(|c| c.starts_with(&["tab".into(), "focus".into()]))
        );
    }
}

#[test]
fn new_session_and_session_picker_are_mutually_exclusive() {
    let fixture = Fixture::new("");
    let output = fixture
        .command(env!("CARGO_BIN_EXE_wt"))
        .args(["select", "task", "--new", "--session"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
    assert!(fixture.commands().is_empty());
}

#[test]
fn tui_n_can_start_either_cli_despite_the_project_default_and_live_agents() {
    for (default, kind, answer) in [("claude", "codex", "\r"), ("codex", "claude", "\x1b[B\r")] {
        let fixture = Fixture::new(&format!("agent = \"{default}\""));
        let tree = fixture.tree();
        fixture.sessions(&tree, true);
        fixture.live_agent(kind, &tree);
        let commands = fixture.tui("", answer);
        let command = pane_command(&commands);
        assert!(
            command.starts_with(&format!("command {kind} 'Context:")),
            "{command}"
        );
        assert!(command.contains("Read it and wait for my command."));
    }
}

#[test]
fn tui_n_on_a_worktree_uses_that_worktree_without_asking() {
    let fixture = Fixture::new("");
    let tree = fixture.tree();
    fixture.sessions(&tree, true);
    let other = fixture.repo.join(".claude/worktrees/other");
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-qb",
            "UCP-1-other",
            other.to_str().unwrap(),
        ],
    );
    // Task → ticket → other → task: choose the second worktree, not the first.
    let commands = fixture.tui("jjj", "\r");
    let create = commands
        .iter()
        .find(|c| c.starts_with(&["workspace".into(), "create".into()]))
        .unwrap();
    assert!(
        create.contains(&tree.to_string_lossy().into_owned()),
        "{create:?}"
    );
    assert!(pane_command(&commands).starts_with("command codex 'Context:"));
}

#[test]
fn tui_new_agent_picker_can_be_cancelled_without_opening_a_tab() {
    let fixture = Fixture::new("");
    let tree = fixture.tree();
    fixture.sessions(&tree, true);
    let commands = fixture.tui("", "\x1b");
    assert!(!commands.iter().any(|c| matches!(
        c.first().map(String::as_str),
        Some("pane" | "workspace" | "tab")
    ) && c.get(1).is_some_and(|s| s == "create" || s == "run")));
}

#[test]
fn tui_n_on_a_task_asks_which_worktree_to_use() {
    let fixture = Fixture::new("");
    let tree = fixture.tree();
    fixture.sessions(&tree, true);
    let other = fixture.repo.join(".claude/worktrees/other");
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-qb",
            "UCP-1-other",
            other.to_str().unwrap(),
        ],
    );
    let commands = fixture.tui_with_tree("", "\r", "task\r");
    let create = commands
        .iter()
        .find(|c| c.starts_with(&["workspace".into(), "create".into()]))
        .unwrap();
    assert!(
        create.contains(&tree.to_string_lossy().into_owned()),
        "{create:?}"
    );
    assert!(pane_command(&commands).starts_with("command codex 'Context:"));
}

#[test]
fn fresh_codex_session_restores_a_removed_worktree_from_claude_history() {
    let fixture = Fixture::new("");
    let tree = fixture.tree();
    fixture.sessions(&tree, true);
    fs::write(
        fixture.root.join("vault/tasks/task.md"),
        "---\ntickets: [UCP-1]\nsessions: [\"shared task 2026-10-29\"]\n---\nContext\n",
    )
    .unwrap();
    fs::remove_file(
        fixture
            .root
            .join("codex/sessions/2026/10/29/rollout-fixture.jsonl"),
    )
    .unwrap();
    git(
        &fixture.repo,
        &["worktree", "remove", tree.to_str().unwrap()],
    );
    let commands = fixture.run(&["select", "task", "--new", "--agent", "codex", "--bg"]);
    assert!(tree.exists());
    assert!(pane_command(&commands).starts_with("command codex 'Context:"));
}
