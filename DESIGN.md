# worktable — design

worktable (binary `wt`) replaces `jg` and the `~/.local/bin/wt` shell script.
It keeps Jira tickets, GitLab MRs, git worktrees, herdr agents and task notes in
the Obsidian vault joined into one table, shown in Obsidian and in a TUI.

## Why

- Task notes in `30-work/tasks/` stopped being maintained (newest is UCP-53289,
  current work is UCP-6108x): linking tickets, MRs and branches by hand is tedious.
- `jg` knows how tickets, MRs and worktrees relate, but only prints it; nothing is
  remembered and there is no place for context.
- Context is per task, not per ticket: one piece of work spans several tickets,
  several stacked MRs and one shared worktree, and agents on neighbouring tickets
  should read the same notes.
- Finished worktrees pile up (10 in ucp at the time of writing).

## Model

```
task  (note: shared context; any size — a "project" is just a big task)
 ├─ tickets    UCP-61083, UCP-61084        manual, plus keys found on the task's worktrees/MRs
 ├─ MRs        !16428 !16266 !16170        auto: ticket key in source branch or title
 ├─ branches   i.udalov/UCP-61083-…        auto: from MRs and local refs with the key
 ├─ worktrees  a2i-config                  auto: worktree whose branch is one of the above
 ├─ agents     a2i-config, …-review        auto: herdr agent whose cwd is inside a worktree
 └─ sessions   a9c0419b (a2i-config, 10-08) auto, append-only: claude sessions run in its worktrees
```

- **Task** — a markdown note in one of the task folders. Its body is the shared
  context: goal, decisions, constraints, plan, findings.
- **One task = one herdr workspace** (decided 2026-10-09), labelled with the
  task name; each worktree of the task is a tab in it, with its agent. A
  worktree belongs to a task by its branch's ticket key, or because an agent in
  the task's workspace works in it — so a branch without a key (a quick fix
  spun off during the task) still lands in the task.
- **Ticket → task** is set explicitly (`wt add`, `wt attach`, `wt start <name> -k`, or by
  editing `tickets:`), and by sync: a key on a worktree or MR that belongs to the
  task (through its herdr workspace or a worktree's branch) and that no note
  lists yet is added to the task's `tickets:`. Everything else is derived.
- **Nesting** is optional: `parent: "[[единый образ]]"`.
- **Tags** — plain Obsidian `tags:`; used for filtering and "similar tasks".
- **Loose ends** — anything not reachable from a task: my in-progress tickets with
  no task, my open MRs whose key belongs to no task, worktrees with no task,
  finished worktrees. They are listed, never auto-filed.

### Matching

| from | to | rule |
|---|---|---|
| ticket | MR | `\b<KEY>\b` in MR source branch or title |
| ticket | branch | `\b<KEY>\b` in a local or remote branch name |
| branch | worktree | `git worktree list --porcelain` |
| worktree | agent | herdr `agent list`: the last `cwd` in the agent's claude transcript (agents started in the main checkout move into worktrees), else the pane cwd |
| worktree | task | an agent working in it runs in the task's herdr workspace (label == task name) |
| MR | MR (no key) | worktree's branch == MR source branch (MRs without a key still land in their worktree's task) |

A ticket listed in two tasks is a conflict: shown in both, flagged in loose ends.

### Derived task state

| state | condition |
|---|---|
| `idea` | no tickets, no worktrees |
| `active` | a worktree exists, or a ticket is in progress |
| `review` | every MR opened or merged, at least one opened, no worktree has local changes or unpushed commits (stacked MRs merge one by one) |
| `finished` | every ticket closed and every MR merged/closed → suggest `wt done` |
| `done` | `done: true` in frontmatter (set by `wt done` or by hand) |

## Note format

Existing conventions are kept: file `UCP-61083 Title.md` or any free name, keys
`done`, `project`, `jira`, `start`, `status`, `aliases` keep working.

Decided 2026-10-09: the TUI is the daily view; Obsidian is archive and search,
task notes are not edited on the phone. So notes carry almost no live data.

```yaml
---
# user-owned (worktable writes these on a command; sync only appends to tickets)
tickets: [UCP-61083, UCP-61084]
tags: [a2i, selectors, cache]
parent: "[[единый образ]]"
done: false
# managed
state: active
sessions: ["a9c0419b-e9d1-… a2i-config 2026-10-08", "6db598b3-… a2i-config-review 2026-10-09"]
# written once by `wt done`, frozen afterwards
mrs: ["https://gitlab.corp.mail.ru/…/-/merge_requests/16428", "https://gitlab.corp.mail.ru/…/-/merge_requests/16266"]
branches: [i.udalov/UCP-61083-a2i-config-folder, i.udalov/UCP-61083-selectors-3-level]
closed: 2026-11-02
---
```

- **While the task is open** sync writes only `state` and appends to `sessions`;
  both change a few times per task, so notes are rewritten rarely. No `synced`
  stamp: it would rewrite every note on every sync.
- **On `wt done`** the final MRs and branches are written once as history (useful
  for search later), together with `closed`. Notes with `done: true` are never
  touched by sync again, so links that depended on a live worktree do not evaporate.
- Everything else (MR states, pipelines, approvals, Jira statuses, worktrees,
  local changes, agents) is live and lives only in the TUI / `wt status`, backed by a
  disposable cache in the user cache dir (`~/Library/Caches/worktable/` on macOS).
- Frontmatter is parsed as real YAML on read (the Obsidian Properties UI rewrites
  flow lists into block lists, requotes, reorders). Writes replace only the
  managed keys' lines — whichever style they are in — and leave every other line,
  comment and key order byte-identical. The body is touched only by `wt note`.
- A file is rewritten only when a managed value changed, via temp + rename,
  skipped if its mtime moved during the sync. One lock file per vault serialises
  sync, TUI refresh and `wt note`.
- If Jira or GitLab is unreachable, or answers only some of the queries, sync
  keeps the previous `state` instead of recomputing it from partial data.
- A key found on the work of several tasks (an MR whose branch and title name
  keys of different tasks) is shown in each but written into none: which task
  it is for is the user's call (`wt attach`).

### Agent context

`wt start` / `wt add` write a brief to `.git/worktrees/<name>/task.md`: the note
path, the Jira description of the ticket, the definition of done. The agent's
initial prompt only points at the note and the brief and tells it to read them
and wait (decided 2026-10-13): the work starts with analysis, on my command.

Agents write their findings into the note, but only under the `## Agent notes`
heading (decided 2026-10-09); everything above it is the user's. They do it via
`wt note "<text>"` rather than editing the file directly:

- the task is resolved from the agent's cwd (worktree → task);
- the entry is appended as `- YYYY-MM-DD <agent>: <text>` at the end of the
  section, which is created at the end of the note on first write;
- a lock file serialises concurrent agents; the write is temp + rename and is
  retried if the note's mtime changed meanwhile (Obsidian, phone sync);
- the brief tells agents to use it for decisions and non-obvious findings only,
  not progress logs.

### Agent sessions

Every claude conversation held in a task's worktrees stays attached to the task
and can be resumed later, also after the worktree was removed.

- **Where they are.** Claude Code keeps transcripts in
  `~/.claude/projects/<cwd with / and . → ->/<session-id>.jsonl`, keyed by the
  directory claude ran in; they survive `git worktree remove`. herdr also reports
  the live session per agent (`agent list` → `agent_session.value`).
- **Discovery.** Transcripts live under the directory claude was *launched*
  in, often the main checkout, while the work happens in a worktree. So the
  transcripts of the main checkouts and of every worktree are parsed (cached by
  size + mtime in `<cache dir>/sessions-v2.json`), and a session belongs to a
  task when its records' `cwd` fall into one of the task's worktrees; new ids are
  added to `sessions`. The key is append-only: removing
  a worktree drops it from `worktrees`, never from `sessions`, so the path is
  remembered through the session entry. Sessions that never enter a worktree
  (the vault, say) are attached only by adding them to `sessions:` by hand.
- **Stored in the note:** id, worktree name, date only. Transcripts are never
  copied into the vault: they are large (hundreds of MB in total) and can contain
  tokens read from config files; the vault is git-backed and synced to the phone.
  The TUI shows each session's first prompt and last activity from the jsonl.
- **Retention.** Claude Code deletes transcripts older than `cleanupPeriodDays`
  (30 by default); 30 days is enough (decided 2026-10-09), nothing is copied.
  A session whose transcript is gone stays in `sessions` as history; `wt select`
  skips it and starts a fresh claude briefed from the note.
- **Resume** is `wt select` (TUI `⏎`/`c`): it restores the worktree at the same
  path when needed, because `--resume`/`--continue` look for sessions of the
  current directory.

## Obsidian

`tasks.base` next to the notes (the existing `work-tasks.base` is replaced after review):

- **active** — `done != true`, grouped by `state`: name, tags, tickets, parent.
- **by tag** — grouped by tags.
- **archive** — `done == true`: name, tags, tickets, mrs, closed; sorted by `closed`.
- **all** — sorted by mtime.

Task folders are read in place (`30-work/tasks`, `30-work/projects`); notes are
never moved — moves resurrect old copies through the phone sync.

## CLI

The daily workflow is five commands (decided 2026-10-09):

```
wt                          TUI
wt start <task name | KEY>  start new work
wt add [<task>] <KEY>       add a ticket to an existing task, with its own worktree tab
wt select [<task | KEY>]    go back to existing work
wt status                   the table in plain text
```

### `wt start <task name | KEY> [-k KEY]... [-t tag]... [-m text] [-n name] [-b branch] [--bg]`

- **A ticket key** (`UCP-12345`) — a new task named after the ticket's summary
  (characters a file name can't hold dropped), with that ticket: the same as
  `wt start "<summary>" -k UCP-12345`. Work never runs without a task — the note
  is where the context and agents' findings go. If the ticket already belongs to
  a task, this is `wt add <that task> <KEY>`.
- **A name** — a new task: creates the note `<new_task_dir>/<name>.md` (`-k`
  tickets, `-t` tags, `-m` goal as the first body line), the herdr workspace
  labelled `<name>`, then a worktree as a tab in it with claude briefed from the
  note. Never `herdr worktree open`: it files the workspace under the repo group. If a task with that name exists, it says so
  and suggests `wt select` / `wt add`. Before creating, lists tasks with similar
  tags/title and asks whether one of them was meant.
- **No argument** — fuzzy picker over my open Jira tickets not in any task, plus
  "new task…".

`wt start` and `wt add` return at once: fetch and checkout of ucp take ~30 s, so
the new tab runs the hidden `wt prepare <spec>` — step lines with a spinner and
git's own checkout progress, then the brief; the tab's shell then cds into the
worktree and starts `command claude` — herdr shows the branch of the shell's
cwd. The agent is named in the background once herdr detects it.

### `wt add [<task>] <KEY> [--in <worktree>] [-n name] [-m text] [--bg]`

Attaches the ticket to the task (writes `tickets:`) and opens a tab for it in
the task's herdr workspace (creating the workspace if it is gone): a new worktree
by default, or `--in` an existing worktree of the task (stacked
MRs that share one worktree) — then only a new agent in that worktree. The agent
is briefed with the task note plus the ticket. `<task>` defaults to the task of
the current worktree, otherwise a fuzzy picker.

### `wt select [<task | KEY>] [--tree <worktree>] [--session] [--bg]`

Returns to existing work; without an argument a fuzzy picker over open tasks and
note-less tickets with worktrees, those with live agents first. Then, for the
chosen work's worktree (picker if the task has several):

1. a live agent → focus its tab in the task's herdr workspace;
2. worktree exists, no agent → a tab in the task's workspace (created if gone)
   + `claude --resume` of the last session held in that worktree, or a fresh
   claude briefed from the note
   (`--session` picks one of the task's sessions → `claude --resume <id>`);
3. worktree removed, branch still exists locally or on origin → recreate the
   worktree at the same path (sessions are keyed by directory), then as 2;
4. branch gone → says so; the task note and session ids remain.

### `wt status [-t tag] [--all] [--no-write]`

Plain-text table, the same rows and marks as the TUI (both walk one view model,
`src/table/view.rs`, and take glyphs, labels and colors from `src/table/look.rs`):
task → worktrees with local changes / unpushed commits,
their MR and agents, then MRs without a worktree; then the `no task` and
`assigned to me in Jira` sections. Ticket rows show only with `--all`, which also
adds what the TUI's `.` adds. Also the format agents and scripts read, so it is
kept stable.

### Less frequent

```
wt review [<worktree>]   reviewer agent next to the task agent
                         (model: `[workflow] review_model`)
wt done [<task | worktree>] [--merged] [--force] [-y]
                    remove worktrees, branches, herdr views. On a task: refuses
                    while anything is open (uncommitted, open MR, unpushed),
                    closes my open tickets of it in Jira (a done-category
                    transition like Closed, never Cancelled; others' tickets are
                    left open), writes done: true, closed, mrs, branches.
                    --merged: sweep all finished worktrees
wt attach <KEY> [<task>]   put a ticket into a task, no worktree or agent
wt note "<text>"    (agents) append to the task's `## Agent notes`
wt completions <shell>     shell completion script
```

There is no separate sync command: `wt status` and the TUI's refresh write
changed `state`, `sessions` and found tickets. Nothing creates notes implicitly —
only `wt start`; worktable never creates tickets.

## TUI

`wt` (or `wt tui`): the task tree on the left, details of the selection on the
right. The first frame comes from notes, worktrees, agents and the Jira/GitLab
data cached by the last online build; a background refresh replaces it, writes
task states, sessions and found tickets back like `wt status`, and repeats every
minute. A service that does not answer keeps its cached data, with a warning.
Agent states are polled from herdr every 2 s; the header counts agents waiting
for me (blocked, or done and unseen) and `w` jumps to the next one.

In herdr, `prefix+t` opens it as a popup (`[[keys.command]]`, `type = "popup"`,
`WT_POPUP=1`): going to some work — focusing an agent, select, start — closes it.

Rows flag what waits too long: an open MR older than 7 days (`12d`), a worktree
with no agent, nothing uncommitted and no commit for 7 days (`quiet 12d`). Open
MRs show draft, pipeline (`ci✓` `ci✗` `ci…`), approvals (`✔2`), unresolved
threads and conflicts. Ticket descriptions are Jira markup rendered as text.

- **Rows**: task → its tickets → MRs without a worktree; the task's worktrees →
  the MR of their branch, agents. Tasks go by state (active, review, idea,
  finished), newest first by when the note entered the vault's git history.
  Below a `no task` title: tickets without a task that have a worktree or an open
  MR, and worktrees without a ticket. Below `assigned to me in Jira`: my open
  tickets nothing is done on yet, in progress first. `.` adds done tasks, merged
  MRs, finished worktrees.
- **Filter**: `/` over task names, tags, tickets and summaries, branches.
- **Details** follow the selection: the note body for a task, the Jira
  description for a ticket, the MR, `git status` + unpushed and branch commits for
  a worktree, the agent's last screen for an agent — slow parts load in threads.

| key | on | action |
|---|---|---|
| `⏎` | agent / worktree | focus the agent; a worktree without one: `wt select <task> --tree <name>` (resume or fresh claude) |
| `⏎` | task, ticket | `wt select` |
| `b` | ticket, MR, worktree | open in the browser |
| `i` | worktree, or a task/ticket with one worktree | open it in the IDE (`[workflow] ide`, default "IntelliJ IDEA", via `open -a`) |
| `o` / `e` | anything under a task | note in Obsidian / in `$EDITOR` |
| `⏎` | ticket nothing is done on | asks y/n, then `wt start KEY` |
| `s` | ticket / task | `wt start KEY` / asks a key for `wt add` |
| `a` | ticket without a task | `wt attach KEY`: pick a task, the key joins its `tickets:` |
| `N` | anything under a task | a dated line at the end of the note's own text |
| `w` | — | next agent waiting for me |
| `p` | — | details pane on/off (hidden under 110 columns) |
| `n` | — | asks a name for `wt start` |
| `c` | task, ticket | `wt select --session` |
| `R`, `d` | worktree | `wt review`, `wt done` |
| `d` | task | `wt done "<task>"`: close it with its Jira tickets |
| `space` `h` `l` | — | fold, fold / to parent, unfold |
| `r`, `?`, `q` | — | refresh, keys, quit |

Commands run in the TUI process, on a background thread, behind a dialog: their
lines show there, and their pickers and confirmations are asked there (a `Ui`
trait: dialoguer and stdout in the CLI, the update channel in the TUI). `d`
checks first and lists what goes before `y`; a blocked task or worktree only
shows its blockers. Work that was opened (`select`, `start`, `add`) closes the
dialog with its last line in the footer, and a herdr popup with it. Only
`wt prepare` runs as a process: in the new tab, before claude.

Not yet: Review tab, editing tags.

## Config

`~/.config/worktable/config.toml` (`$XDG_CONFIG_HOME/worktable/config.toml`, or
the path in `WORKTABLE_CONFIG`):

```toml
[jira]
host = "https://jira.vk.team"

[gitlab]
host = "gitlab.corp.mail.ru"
project = "ai/ucp"          # used when no [[repo]] names a project

[vault]
path = "~/machrist-vault"
task_dirs = ["30-work/tasks", "30-work/projects"]
new_task_dir = "30-work/tasks"

[[repo]]
path = "~/code/vk/ucp"
gitlab_project = "ai/ucp"

[workflow]
branch_prefix = "i.udalov/"
review_model = "claude-fable-5-1"   # optional: claude's default when unset
ide = "IntelliJ IDEA"               # optional, the default
```

Multiple `[[repo]]` from day one: worktrees and MRs are collected per repo.
Tokens stay in `JIRA_TOKEN` / `GITLAB_TOKEN`. `WT_TRACE=1` prints how long each
fetch, the matching and the rendering took to stderr.

## Non-goals

- No database: notes + git + Jira/GitLab are the state; the cache is disposable.
- No implicit creation of tickets or notes, no comments in Jira/GitLab, no
  commits or pushes.
- No moving or renaming vault notes.

## Open questions

- Do markdown links inside list properties render as links in Bases? (Fable says
  no — then the archive view builds MR URLs with a formula from the iid.) Check.
- Should `wt done` on a task also offer to move its tickets in Jira? (Default: no.)
