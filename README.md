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
|- list
    |- task
    |- mr
    |- task-mr (default)
```

## jg info [--task-key]

List all the info about the task

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