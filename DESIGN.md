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
 ├─ tickets    UCP-61083, UCP-61084        manual: the only link the user sets
 ├─ MRs        !16428 !16266 !16170        auto: ticket key in source branch or title
 ├─ branches   i.udalov/UCP-61083-…        auto: from MRs and local refs with the key
 ├─ worktrees  a2i-config                  auto: worktree whose branch is one of the above
 ├─ agents     a2i-config, …-review        auto: herdr agent whose cwd is inside a worktree
 └─ sessions   a9c0419b (a2i-config, 10-08) auto, append-only: claude sessions run in its worktrees
```

- **Task** — a markdown note in one of the task folders. Its body is the shared
  context: goal, decisions, constraints, plan, findings.
- **Ticket → task** is set explicitly (`wt add`, `wt start <name> -k`, or by
  editing `tickets:`). Everything below it is derived on every sync.
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
| worktree | agent | herdr `agent list`, agent `cwd` under the worktree path |
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
# user-owned (worktable only writes these on an explicit command)
tickets: [UCP-61083, UCP-61084]
tags: [a2i, selectors, cache]
parent: "[[единый образ]]"
done: false
# managed
state: active
sessions: ["a9c0419b-e9d1-… a2i-config 2026-10-08", "6db598b3-… a2i-config-review 2026-10-09"]
# written once by `wt done`, frozen afterwards
mrs: ["!16428 merged", "!16266 merged", "!16170 merged"]
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
  disposable cache in `~/.cache/worktable/`.
- Frontmatter is parsed as real YAML on read (the Obsidian Properties UI rewrites
  flow lists into block lists, requotes, reorders). Writes replace only the
  managed keys' lines — whichever style they are in — and leave every other line,
  comment and key order byte-identical. The body is touched only by `wt note`.
- A file is rewritten only when a managed value changed, via temp + rename,
  skipped if its mtime moved during the sync. One lock file per vault serialises
  sync, TUI refresh and `wt note`.
- If Jira or GitLab is unreachable, sync keeps the previous `state` instead of
  recomputing it from partial data.

### Agent context

The note body is the agent's brief, replacing `.git/worktrees/<name>/task.md`.
`wt start` prompts the agent with: the note path, the Jira summaries/descriptions of
the task's tickets (fetched at start), the definition of done.

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
- **Discovery.** On sync, for every worktree path the task ever had, list
  `<slug>/*.jsonl` and add new ids to `sessions`. The key is append-only: removing
  a worktree drops it from `worktrees`, never from `sessions`, so the path is
  remembered through the session entry. Sessions started elsewhere (main checkout,
  the vault) are attached by hand: `wt attach-session <id> <task>`.
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
wt add [<task>] <KEY>       add a ticket to an existing task, with its own workspace
wt select [<task | KEY>]    go back to existing work
wt status                   the table in plain text
```

### `wt start <task name | KEY> [-k KEY]... [-t tag]... [-m text] [-n name] [--tab] [--bg]`

- **A ticket key** (`UCP-12345`) — work on a ticket without a note. Worktree and
  branch `<prefix><KEY>-<slug from summary>`, herdr workspace, claude briefed from
  the Jira issue. The ticket shows in loose ends until `wt add` puts it into a task.
  If the ticket already belongs to a task, this is `wt add <that task> <KEY>`.
- **A name** — a new task: creates the note `<new_task_dir>/<name>.md` (`-k`
  tickets, `-t` tags, `-m` goal as the first body line), then worktree, workspace
  and claude briefed from the note. If a task with that name exists, it says so
  and suggests `wt select` / `wt add`. Before creating, lists tasks with similar
  tags/title and asks whether one of them was meant.
- **No argument** — fuzzy picker over my open Jira tickets not in any task, plus
  "new task…".

### `wt add [<task>] <KEY> [--in <worktree>] [-n name] [--tab] [--bg]`

Attaches the ticket to the task (writes `tickets:`) and opens a workspace for it:
a new worktree by default, or `--in` an existing worktree of the task (stacked
MRs that share one worktree) — then only a new agent in that worktree. The agent
is briefed with the task note plus the ticket. `<task>` defaults to the task of
the current worktree, otherwise a fuzzy picker.

### `wt select [<task | KEY>] [--session]`

Returns to existing work; without an argument a fuzzy picker over open tasks and
note-less tickets with worktrees, most recently active first. Then, for the
chosen work's worktree (picker if the task has several):

1. a live agent → focus its herdr workspace;
2. worktree exists, no agent → herdr workspace there + `claude --continue`
   (`--session` picks one of the task's sessions → `claude --resume <id>`);
3. worktree removed, branch still exists locally or on origin → recreate the
   worktree at the same path (sessions are keyed by directory), then as 2;
4. branch gone → says so; the task note and session ids remain.

### `wt status [-t tag] [--all]`

Plain-text table, the same rows as the TUI: task → tickets → MRs, worktrees with
local changes / unpushed commits, agents; then loose ends. Also the format agents
and scripts read, so it is kept stable.

### Less frequent

```
wt review [<worktree>] [--model M]   reviewer agent next to the task agent
wt done [<task | worktree>] [--merged] [--force] [-y]
                    remove worktrees, branches, herdr views; on a whole task also
                    write the final mrs/branches and done: true. Same safety
                    checks as jg done. --merged: sweep all finished work
wt note "<text>"    (agents) append to the task's `## Agent notes`
wt open [<task>]    the note in Obsidian
wt attach-session <id> <task>   a claude session held outside a worktree
wt migrate [--apply]            convert old notes; dry run by default
wt ticket new …                 create a Jira ticket — the only command that does
```

There is no separate sync command: every `wt` invocation refreshes the cache if
it is older than a few minutes and writes changed `state`/`sessions`. Nothing
creates tickets or notes implicitly — notes only by `wt start <name>`, tickets
only by `wt ticket new`.

`jg list / mr / switch / info / tui` are folded into the TUI.

## TUI

One screen: task tree on the left, details of the selection on the right.
Cached state renders instantly; refresh runs in background threads and updates
rows in place.

```
┌ worktable ─ Tasks │ Loose ends 3 │ Review 2 ───────────────── synced 10:40 ⟳ ┐
│ / filter   # a2i                                                            │
├────────────────────────────────────────┬───────────────────────────────────┤
│ ● a2i selectors              active    │ a2i selectors                      │
│   ├ UCP-61083  В работе                │ #a2i #selectors  ↑ единый образ    │
│   │  ├ !16428 opened  ✓pipe  1/2 appr  │                                    │
│   │  ├ !16266 opened  ✗pipe            │ ## Цель                            │
│   │  └ !16170 merged                   │ Селекторы по 3-уровневой модели:   │
│   ├ UCP-61084  Новый                   │ фабрика → кеш → привязка к запросу │
│   │  └ !16117 opened  ✓pipe            │ …                                  │
│   └ ⌂ a2i-config  2 changed  1 unpushed│                                    │
│       ◉ a2i-config  working            │ ## Agent notes                     │
│ ○ s2i cache                  review    │ - кеш ключуется по версии конфига  │
│ ✓ fp selectors               finished  │                                    │
├────────────────────────────────────────┴───────────────────────────────────┤
│ ⏎ go  o note  e edit  m MR  j jira  s start  R review  d done  a attach  ?  │
└────────────────────────────────────────────────────────────────────────────┘
```

- **Tabs**: Tasks · Loose ends (count) · Review (MRs where I am a reviewer, phase 4).
- **Rows**: task → tickets → MRs; worktrees and agents under the task.
  `space` folds; done tasks hidden unless `.` toggles them.
- **Filter**: `/` text over title, tickets, branches; `#` tag.
- **Details pane** follows the selected row: note body for a task, ticket
  description for a ticket, MR (pipeline, approvals, discussions count) for an MR,
  `git status` + unpushed log for a worktree, last agent output for an agent.

| key | on | action |
|---|---|---|
| `⏎` | worktree / agent | `herdr agent focus` / `workspace focus` (TUI stays) |
| `o` / `e` | any | note in Obsidian / in `$EDITOR` |
| `m`, `j` | MR, ticket | open in browser |
| `s` | task, ticket | `wt start` (asks name, prefilled) |
| `R` | worktree | `wt review` |
| `c` | task, worktree | `wt select --session` (pick from its sessions, newest first) |
| `d` | task, worktree | `wt done` with confirmation listing what goes |
| `a` | loose end | attach to task (fuzzy picker) or `n` new task from it |
| `t` | task | edit tags |
| `r` | — | refresh now |

Destructive actions always show the list of what will be removed and wait for `y`.

## Config

`~/.config/worktable/config.toml` (jg's config is read as fallback once):

```toml
vault = "~/machrist-vault"
task_dirs = ["30-work/tasks", "30-work/projects"]
new_task_dir = "30-work/tasks"

[jira]
host = "https://jira.vk.team"
mine = "assignee = currentUser() AND statusCategory != Done"

[[repo]]
path = "~/code/vk/ucp"
gitlab_project = "ai/ucp"

[workflow]
branch_prefix = "i.udalov/"
max_agents = 4
review_model = "claude-fable-5-1"
done_definition = "…"
```

Multiple `[[repo]]` from day one: worktrees and MRs are collected per repo.
Tokens stay in `JIRA_TOKEN` / `GITLAB_TOKEN`.

## Migration of existing notes

`wt migrate` (dry run prints a diff per file; `--apply` writes):

- `jira: <url>` → add `tickets: [KEY]`; `jira` stays.
- No `jira` → key from the file name `UCP-\d+`; none → task without tickets.
- `done`, `project`, everything else untouched; files not renamed.
- First sync fills managed keys.

## Phases

Actions come before the TUI: the daily workflow is the five commands, and the
note writer has to prove itself on the real vault before more is built on it.

1. **Model + status.** Rename crate to `worktable`, binary `wt`; config; note
   reader (real YAML) and managed-key writer with tests; matching; cache;
   sessions discovery; `wt status`, `wt migrate`; `tasks.base`.
2. **Workflow.** `wt start`, `wt add`, `wt select`, `wt done`, `wt review`,
   `wt note`, ported from jg onto tasks with note-based briefs; delete
   `~/.local/bin/wt`, uninstall `jg`. Then about two weeks of daily use.
3. **TUI.** Tree, details, filters, background refresh; keys map onto the same
   commands.
4. **Extras.** Review tab; task/ticket in the herdr sidebar via
   `herdr workspace report-metadata`.

## Non-goals

- No database: notes + git + Jira/GitLab are the state; the cache is disposable.
- No implicit creation of tickets or notes, no comments in Jira/GitLab, no
  commits or pushes.
- No moving or renaming vault notes.

## Open questions

- Do markdown links inside list properties render as links in Bases? (Fable says
  no — then the archive view builds MR URLs with a formula from the iid.) Check.
- Should `wt done` on a task also offer to move its tickets in Jira? (Default: no.)
