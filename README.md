# JiraGitlab (`jg`)

`jg` is a CLI utility designed to simplify the workflow between **Jira** and **GitLab**.  
It helps seamlessly switch between issues and branches, and create merge requests linked to Jira tasks.

## Commands

```
jg
|- info [--task-key]
|- new-task
|- mr
|- switch [--task-key]
|- tui
|- start [--key] [--name] [--branch] [--message] [--tab] [--bg]
|- status
|- review [name]
|- done [name] [--merged] [--force] [--yes]
|- list
    |- task
    |- mr
    |- all (default)
```

## jg info [--task-key]

List task details from Jira and related open merge requests from GitLab.

## jg new-task [--task-key]

Select one of your unresolved Jira issues, suggest a branch name based on the issue key and summary,
and create or reuse the local branch.

## jg mr [--task-key] [--title] [--description]

Create a merge request for the current branch. If the branch name contains a Jira key, it is used
automatically; otherwise the command asks you to select an issue.

## jg switch [--task-key]

Fuzzy-pick one of your authored open merge requests and checkout its source branch locally.

## jg tui

Open the interactive launcher for the most common workflows: list, switch, create branch, create MR,
and inspect task details.

Example:

```
>> jg info TASK-123

TASK-123: some example task name from jira

Task description. This is very important task.

branches:
    - author-TASK-123-branch-name [mr: opened] [issues: 2] [pipeline: failed]
        - commit-3
        - commit-2
        - commit-1

```

## Parallel agents: worktree per task

`start`, `status`, `review` and `done` run one task per git worktree, each with its own
claude agent in a [herdr](https://herdr.dev) workspace. They rely on the `wt` script
(worktree + herdr workspace + agent) being on `PATH`.

```toml
# ~/.config/jg/config.toml
[workflow]
branch_prefix = "i.udalov/"   # new branches: <prefix><KEY>-<name>
# review_model = "claude-fable-5-1"
```

### jg start [--key] [--name] [--branch] [--message] [--tab] [--bg]

Pick a Jira issue, create `<repo>/.claude/worktrees/<name>` on a new branch from
`origin/HEAD`, open it in a herdr workspace and start claude briefed with the issue text,
`--message` and a definition of done. With `--name` nothing is asked, so agents can run it.

### jg status

All linked worktrees: branch, uncommitted / unpushed commits, live agents and the latest
MR. Worktrees that are safe to remove are marked `→ jg done`.

### jg review [name]

Start a second claude next to the task's agent that reviews the branch diff and only
reports findings.

### jg done [name] [--merged]

Remove the worktree, its branch and its herdr workspace. Refuses when there are
uncommitted changes, the MR is still open, or commits exist that are neither on origin nor
in the merged MR. `--merged` sweeps every worktree that is safe to remove.
