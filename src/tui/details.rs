//! The details pane: its lines and the lazily loaded parts.

use super::{
    App, Update,
    rows::{self, Kind, clean},
};
use crate::config::Config;
use crate::table;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::path::Path;
use std::process::Command;

impl App {
    /// The slow part of a row's details, loaded on selection.
    pub(super) fn detail_key(&self, kind: &Kind) -> Option<String> {
        match kind {
            Kind::Loose(i) => Some(format!("desc:{}", self.table.loose[*i].key)),
            Kind::Ticket(key) => Some(format!("desc:{key}")),
            Kind::Tree(t) => Some(format!("git:{}", self.table.tree(*t).wt.path.display())),
            // by pane: unnamed agents and duplicate names read the right screen
            Kind::Agent(t, a) => Some(format!("agent:{}", self.table.tree(*t).agents[*a].pane)),
            _ => None,
        }
    }

    pub(super) fn load_details(&mut self) {
        if !self.preview_shown {
            return;
        }
        let Some(key) = self.current().and_then(|r| self.detail_key(&r.kind)) else {
            return;
        };
        if self.details.contains_key(&key) || self.pending.contains(&key) {
            return;
        }
        self.pending.insert(key.clone());
        let (config, tx) = (self.config.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let text = load_detail(&config, &key);
            let _ = tx.send(Update::Detail(key, text));
        });
    }

    pub(super) fn details_lines(&self) -> Vec<Line<'static>> {
        let Some(row) = self.current() else {
            return vec![Line::from("no active work — . shows everything")];
        };
        let table = &self.table;
        let bold = |s: String| Span::styled(s, Style::default().add_modifier(Modifier::BOLD));
        let dim = |s: String| Span::styled(s, Style::default().fg(Color::DarkGray));
        let lazy = |key: Option<String>| -> Vec<Line<'static>> {
            match key.and_then(|k| self.details.get(&k).cloned()) {
                Some(text) => text.lines().map(|l| Line::from(clean(l))).collect(),
                None => vec![Line::from(dim("loading…".to_string()))],
            }
        };
        let mut lines: Vec<Line<'static>> = Vec::new();
        match &row.kind {
            Kind::Task(i) => {
                let row = &table.tasks[*i];
                let state = row.state_label();
                lines.push(Line::from(vec![
                    rows::state_mark(state),
                    bold(row.task.name.clone()),
                ]));
                let mut meta = vec![dim(state.to_string())];
                if !row.task.tags.is_empty() {
                    meta.push(dim(format!("  #{}", row.task.tags.join(" #"))));
                }
                lines.push(Line::from(meta));
                lines.push(Line::from(dim(display_path(&row.task.note.path))));
                for key in row.tickets() {
                    let mut spans =
                        vec![Span::styled(key.clone(), Style::default().fg(Color::Cyan))];
                    if let Some(t) = table.tickets.get(key) {
                        spans.push(Span::styled(
                            format!(" {}", t.status),
                            Style::default().fg(rows::status_color(t.category)),
                        ));
                        spans.push(Span::raw(format!(" {}", clean(&t.summary))));
                    }
                    lines.push(Line::from(spans));
                }
                lines.push(Line::from(""));
                lines.extend(row.task.note.body().lines().map(|l| Line::from(clean(l))));
            }
            Kind::Loose(_) | Kind::Ticket(_) => {
                let key = match &row.kind {
                    Kind::Loose(i) => table.loose[*i].key.clone(),
                    Kind::Ticket(key) => key.clone(),
                    _ => unreachable!(),
                };
                match table.tickets.get(&key) {
                    Some(t) => {
                        lines.push(Line::from(vec![
                            Span::styled(key.clone(), Style::default().fg(Color::Cyan)),
                            bold(format!(" {}", clean(&t.summary))),
                        ]));
                        lines.push(Line::from(Span::styled(
                            t.status.clone(),
                            Style::default().fg(rows::status_color(t.category)),
                        )));
                    }
                    None => lines.push(Line::from(Span::styled(
                        key.clone(),
                        Style::default().fg(Color::Cyan),
                    ))),
                }
                lines.push(Line::from(dim(table.ticket_url(&key))));
                lines.push(Line::from(""));
                lines.extend(lazy(self.detail_key(&row.kind)));
            }
            Kind::Mr(m) => {
                let mr = &table.mr(*m);
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("!{}", mr.mr.iid()),
                        Style::default().fg(Color::Blue),
                    ),
                    bold(format!(" {}", clean(mr.mr.title()))),
                ]));
                lines.push(Line::from(dim(format!(
                    "{} · {}",
                    mr.mr.state().as_str(),
                    mr.mr.source_branch()
                ))));
                if let Some(review) = &mr.review {
                    let mut status = vec![Span::raw("pipeline ")];
                    status.extend(
                        rows::review_spans(mr)
                            .into_iter()
                            .filter(|s| s.content.contains("ci")),
                    );
                    if review.pipeline.is_none() {
                        status.push(dim("none".to_string()));
                    }
                    lines.push(Line::from(status));
                    lines.push(Line::from(if review.approved_by.is_empty() {
                        dim("no approvals".to_string())
                    } else {
                        Span::styled(
                            format!("approved by {}", review.approved_by.join(", ")),
                            Style::default().fg(Color::Green),
                        )
                    }));
                    if review.unresolved > 0 {
                        lines.push(Line::from(Span::styled(
                            format!("{} unresolved thread(s)", review.unresolved),
                            Style::default().fg(Color::Yellow),
                        )));
                    }
                }
                if let Some(status) = &mr.mr.detailed_merge_status {
                    lines.push(Line::from(dim(format!(
                        "merge: {}",
                        status.replace('_', " ")
                    ))));
                }
                if !mr.keys.is_empty() {
                    lines.push(Line::from(dim(mr.keys.join(" "))));
                }
                lines.push(Line::from(dim(mr.mr.web_url().to_string())));
            }
            Kind::Tree(t) => {
                let tree = table.tree(*t);
                lines.push(Line::from(bold(format!("⌂ {}", tree.wt.name()))));
                lines.push(Line::from(dim(display_path(&tree.wt.path))));
                lines.push(Line::from(dim(tree.wt.branch.clone().unwrap_or_default())));
                let mut state = Vec::new();
                if let Some(c) = tree.changes {
                    state.push(Span::styled(
                        format!("changed {c}"),
                        Style::default().fg(Color::Yellow),
                    ));
                }
                if let Some(u) = tree.unpushed {
                    state.push(Span::styled(
                        format!("  unpushed {u}"),
                        Style::default().fg(Color::Magenta),
                    ));
                }
                if let Some(m) = tree.mr {
                    state.push(Span::styled(
                        format!(
                            "  !{} {}",
                            table.mr(m).mr.iid(),
                            table.mr(m).mr.state().as_str()
                        ),
                        Style::default().fg(Color::Blue),
                    ));
                }
                lines.push(Line::from(state));
                for agent in &tree.agents {
                    lines.push(Line::from(rows::agent_spans(agent)));
                }
                lines.push(Line::from(""));
                lines.extend(lazy(self.detail_key(&row.kind)));
            }
            Kind::Header => {}
            Kind::Agent(t, a) => {
                let agent = &table.tree(*t).agents[*a];
                lines.push(Line::from(vec![
                    bold(format!("◉ {} ", agent.name)),
                    Span::styled(
                        agent.status.as_str(),
                        Style::default().fg(rows::agent_color(agent.status)),
                    ),
                ]));
                lines.push(Line::from(dim(format!("in {}", table.tree(*t).wt.name()))));
                lines.push(Line::from(""));
                lines.extend(lazy(self.detail_key(&row.kind)));
            }
        }
        lines
    }
}

/// Jira wiki markup as plain text worth reading: headings, lists, code blocks,
/// links, monospace, images and tables lose their syntax.
fn jira_text(markup: &str) -> String {
    use regex::Regex;
    use std::sync::LazyLock;
    static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^h([1-6])\.\s*").unwrap());
    static BLOCK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\{(code|noformat|quote|panel)(:[^}]*)?\}\s*$").unwrap());
    static BULLET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([*#-]+)\s+").unwrap());
    static LINK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\[([^|\]]+)\|([^\]]+)\]").unwrap());
    static BARE_LINK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\[(https?://[^\]]+)\]").unwrap());
    static MONO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{\{(.+?)\}\}").unwrap());
    static IMAGE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"!([^!|\s]+)(\|[^!]*)?!").unwrap());
    static COLOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{color(:[^}]*)?\}").unwrap());

    let mut out = Vec::new();
    let mut in_block = false;
    for line in markup.lines() {
        if BLOCK.is_match(line.trim()) {
            in_block = !in_block;
            out.push("────".to_string());
            continue;
        }
        if in_block {
            out.push(format!("  {line}"));
            continue;
        }
        let mut text = line.to_string();
        if let Some(c) = HEADING.captures(&text) {
            let level: usize = c[1].parse().unwrap_or(1);
            text = format!("{} {}", "#".repeat(level), &text[c[0].len()..]);
        } else if let Some(c) = BULLET.captures(&text) {
            let depth = c[1].len();
            text = format!("{}• {}", "  ".repeat(depth - 1), &text[c[0].len()..]);
        } else if text.trim() == "----" {
            text = "────────".to_string();
        }
        text = LINK.replace_all(&text, "$1 ($2)").into_owned();
        text = BARE_LINK.replace_all(&text, "$1").into_owned();
        text = MONO.replace_all(&text, "`$1`").into_owned();
        text = IMAGE.replace_all(&text, "[image $1]").into_owned();
        text = COLOR.replace_all(&text, "").into_owned();
        text = text.replace("||", " | ");
        out.push(text);
    }
    out.join("\n")
}

fn display_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    match dirs::home_dir() {
        Some(home) => path.replacen(&*home.to_string_lossy(), "~", 1),
        None => path.to_string(),
    }
}

/// Ticket description, worktree's git status and unpushed commits, or the
/// agent's recent screen.
fn load_detail(config: &Config, key: &str) -> String {
    if let Some(ticket) = key.strip_prefix("desc:") {
        return match table::jira_client(config)
            .and_then(|c| c.get_issue_by_key(ticket).map_err(|e| e.to_string()))
        {
            Ok(issue) => issue
                .fields
                .description
                .map(|d| jira_text(&d))
                .unwrap_or_else(|| "(no description)".to_string()),
            Err(e) => format!("! {e}"),
        };
    }
    if let Some(path) = key.strip_prefix("git:") {
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
                .unwrap_or_default()
        };
        let status = git(&["status", "--short"]);
        let branch = git(&["branch", "--show-current"]);
        // branches are created --no-track: unpushed means not in origin/<branch>
        let unpushed = match git(&["rev-parse", "--verify", "-q", &format!("origin/{branch}")]) {
            remote if remote.is_empty() => "branch not pushed".to_string(),
            _ => git(&["log", "--oneline", "-15", &format!("origin/{branch}..HEAD")]),
        };
        let commits = git(&["log", "--oneline", "-15", "origin/HEAD..HEAD"]);
        let mut out = String::new();
        if status.is_empty() {
            out.push_str("nothing uncommitted\n");
        } else {
            out.push_str(&format!("uncommitted:\n{status}\n"));
        }
        if !unpushed.is_empty() {
            out.push_str(&format!("\nunpushed:\n{unpushed}\n"));
        }
        if !commits.is_empty() {
            out.push_str(&format!("\nbranch commits:\n{commits}\n"));
        }
        return out;
    }
    if let Some(agent) = key.strip_prefix("agent:") {
        return Command::new("herdr")
            .args([
                "agent",
                "read",
                agent,
                "--source",
                "recent-unwrapped",
                "--lines",
                "30",
            ])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
            .unwrap_or_else(|e| format!("! {e}"));
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::jira_text;

    #[test]
    fn jira_markup_reads_as_text() {
        let markup = "h2. Цель\n* пункт с {{code}}\n** вложенный [ссылка|https://x.y]\n{code:java}\nint a;\n{code}\n!shot.png|thumbnail!";
        assert_eq!(
            jira_text(markup),
            "## Цель\n• пункт с `code`\n  • вложенный ссылка (https://x.y)\n────\n  int a;\n────\n[image shot.png]"
        );
    }
}
