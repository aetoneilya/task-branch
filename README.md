# worktable (`wt`)

worktable joins Jira tickets, GitLab merge requests, git worktrees,
[herdr](https://herdr.dev) agents and task notes in an Obsidian vault into one
table. A task is a markdown note; its tickets, MRs, worktrees, agents and Claude/Codex
sessions are found from branch names, herdr workspaces and transcripts. `wt`
shows the table as a TUI or as plain text and runs the daily workflow: start a
task, add a ticket to it, go back to it, review, close. `DESIGN.md` has the
model and the decisions.

## Install

```
cargo install --path . --locked
```

## Config

`~/.config/worktable/config.toml` (`$XDG_CONFIG_HOME/worktable/config.toml`, or
the path in `WORKTABLE_CONFIG`):

```toml
[vault]
path = "~/machrist-vault"

[workflow]                  # defaults for every project
branch_prefix = "i.udalov/"
# agent = "codex"             # claude (default) or codex, for new conversations
# review_agent = "claude"     # optional, defaults to agent
# codex_review_model = "your-codex-model"
# review_model = "claude-fable-5-1"
# ide = "IntelliJ IDEA"

[[project]]                 # in the TUI's project dropdown, `wt -p ucp`
name = "ucp"
task_dirs = ["30-work/tasks"]
# new_task_dir = "30-work/tasks"   # default: the first of task_dirs
jira = { host = "https://jira.vk.team", project = "UCP", issue_type = "Задача" }
gitlab = { host = "gitlab.corp.mail.ru", project = "ai/ucp" }

[[project.repo]]
path = "~/code/vk/ucp"
gitlab_project = "ai/ucp"

[[project]]                 # no Jira, no GitLab: notes, worktrees, agents
name = "vpn"
task_dirs = ["10-ah-fuck-life/projects/vpn"]
branch_prefix = ""
# agent = "claude"            # optional per-project override

[[project.repo]]
path = "~/code/pet-projects/vpn-bot"
```

Commands work on one project: `-p <name>`, else the one whose repo the current
directory is in, else the first. The TUI opens on the last one shown; ↑ from the
top of the list gets to the project dropdown, ⏎ opens it, and its "+ new project"
appends a `[[project]]` here. `[` `]` and Tab switch at once.

Tokens come from `JIRA_TOKEN` and `GITLAB_TOKEN`. `WT_TRACE=1` prints how long
each fetch, the matching and the rendering took to stderr.

## Commands

```
wt                         the TUI (also `wt tui`)
wt start [<task|KEY>]      new task: note, herdr workspace, worktree tab with an agent
wt add [<task>] <KEY>      add a ticket to a task, with its own worktree tab
wt attach <KEY> [<task>]   put a ticket into a task without a worktree
wt select [<task|KEY>]     go back to a task: its agent, or the owning CLI resuming its session; a task with nothing yet gets its first worktree
wt note "<text>"           (for agents) append a finding to the note's "Agent notes"
wt status                  the table in plain text; updates `state` and `sessions` in notes
wt review [<worktree>]     start a reviewer agent next to the worktree's agent
wt done [<task|worktree>] [--merged] [--force] [-y]
                           close a task (worktrees, my Jira tickets, note marked
                           done, its herdr workspace),
                           or remove one worktree; --merged removes every finished one
wt completions <shell>     shell completion script
```

`wt <command> --help` lists the options.

## Claude and Codex

```sh
wt start "new task" --agent codex
wt add "existing task" UCP-123 --agent claude
wt select "existing task" --agent codex
wt select "existing task" --new --agent codex
wt review --agent claude
wt --agent codex              # TUI: use Codex for new work in this invocation
```

`--agent` is accepted before or after the subcommand. Without it, new work uses
`[[project]] agent`, then `[workflow] agent`, then Claude for compatibility.
Reviews use `review_agent` when configured; `--agent` overrides it too.
`review_model` applies to Claude reviews and `codex_review_model` to Codex reviews;
when omitted, each CLI keeps its own configured model.

`wt select` without an override focuses an existing agent or resumes the most
recently active conversation using its original CLI. With `--agent`, it focuses
or resumes that CLI only, starting a new conversation when none exists.
Switching CLIs starts a separate conversation with the task note and brief as
shared context; it does not translate the other CLI's transcript.
`--new` always opens a fresh conversation, even when that CLI already has a live
agent or saved sessions. It cannot be combined with `--session`.

The session picker labels each entry with its CLI. Historical bare session ids
in task notes remain Claude ids; Codex entries use `codex:<id>`. Both stores are
discovered (`CLAUDE_CONFIG_DIR/projects` and `CODEX_HOME/sessions`, defaulting to
`~/.claude` and `~/.codex`). Codex subagents are excluded. The parsed cache is
`sessions-v3.json`; the previous cache can be left in place. Worktree paths stay
under `<repo>/.claude/worktrees/` for both tools.

## TUI

`?` shows all keys. The main ones:

- `↑↓`/`jk` move, `←→`/`hl`/space fold, `z` fold / unfold all, `/` filter, `.` all / active only, `v` hide MRs, worktrees, …
- `⏎` focus the agent, or resume work (`wt select`)
- `n` on a task or worktree: choose Codex or Claude and open a fresh agent session in a new tab with the task context; tasks with multiple worktrees ask which one to use
- `c` pick an existing session to resume
- `+ new task` row above the tasks: ⏎ or a click opens a form (name, one Jira dropdown: none, new ticket and the open tickets without a task, filtered by typing; a worktree checkbox and an agent dropdown: claude, codex or none); `s` start a ticket / add one to a task, `a` attach a ticket
- `b` browser, `o` note in Obsidian, `e` note in `$EDITOR`, `i` worktree in the IDE
- `R` review, `d` remove a worktree / close a task
- `w` next agent waiting for you, `p` details pane, `r` refresh, `q` quit

## herdr popup

In herdr's config, `prefix+t` opens the TUI as a popup; going to some work
closes it:

```toml
[[keys.command]]
key = "prefix+t"
type = "popup"
command = "zsh -ic 'WT_POPUP=1 exec wt'"
```
