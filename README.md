# worktable (`wt`)

worktable joins Jira tickets, GitLab merge requests, git worktrees,
[herdr](https://herdr.dev) agents and task notes in an Obsidian vault into one
table. A task is a markdown note; its tickets, MRs, worktrees, agents and claude
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
[jira]
host = "https://jira.vk.team"
project = "UCP"            # where wt start --new-ticket creates tickets
issue_type = "Задача"       # their type as the project names it (default Task)

[gitlab]
host = "gitlab.corp.mail.ru"
project = "ai/ucp"

[vault]
path = "~/machrist-vault"
task_dirs = ["30-work/tasks"]
new_task_dir = "30-work/tasks"

[[repo]]
path = "~/code/vk/ucp"
gitlab_project = "ai/ucp"

[workflow]
branch_prefix = "i.udalov/"
# review_model = "claude-fable-5-1"
# ide = "IntelliJ IDEA"
```

Tokens come from `JIRA_TOKEN` and `GITLAB_TOKEN`. `WT_TRACE=1` prints how long
each fetch, the matching and the rendering took to stderr.

## Commands

```
wt                         the TUI (also `wt tui`)
wt start [<task|KEY>]      new task: note, herdr workspace, worktree tab with claude
wt add [<task>] <KEY>      add a ticket to a task, with its own worktree tab
wt attach <KEY> [<task>]   put a ticket into a task without a worktree
wt select [<task|KEY>]     go back to a task: its agent, or claude resuming its session
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

## TUI

`?` shows all keys. The main ones:

- `↑↓`/`jk` move, `←→`/`hl`/space fold, `/` filter, `.` all / active only, `v` hide MRs, worktrees, …
- `⏎` focus the agent, or resume work (`wt select`)
- `+ new task` row after the tasks: ⏎ or a click starts one; `s` start a ticket / add one to a task, `a` attach a ticket
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
