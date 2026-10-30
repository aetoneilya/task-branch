//! Markdown in the details pane: task notes, and Jira descriptions once
//! `jira_text` has turned them into it. Styled, and wrapped to the pane with
//! wrapped lines indented under the item's text, which ratatui's wrap can't do.

use crate::table::look::clean;
use chrono::{Datelike, NaiveDate};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use regex::Regex;
use std::sync::LazyLock;

static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(#{1,6})\s+(.*)$").unwrap());
static RULE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(-{3,}|\*{3,}|_{3,})$").unwrap());
static ITEM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^( *)([-*+]|\d+[.)])\s+(?:\[([ xX])\]\s+)?(.*)$").unwrap());
/// A dated line: `- 2026-10-20 slug: text` from `wt note`, `- 2026-10-20: text`
/// from the N key.
static DATED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-*+] (\d{4}-\d{2}-\d{2})(?: ([^\s:]+))?: (.+)$").unwrap());
static INLINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"`([^`]+)`",
        r"|\*\*(.+?)\*\*|__(.+?)__",
        r"|~~(.+?)~~",
        r"|\*([^*\s](?:[^*]*[^*\s])?)\*",
        r"|!?\[\[([^\]|]+)(?:\|([^\]]+))?\]\]",
        r"|\[([^\]]+)\]\(([^)\s]+)\)",
        r"|(https?://[^\s)>\]]+)",
    ))
    .unwrap()
});

/// Slugs of the agents' tickets, told apart by color; cyan and yellow are
/// taken by markers and code.
const SLUG_COLORS: [Color; 6] = [
    Color::Blue,
    Color::Magenta,
    Color::Green,
    Color::LightRed,
    Color::LightBlue,
    Color::LightMagenta,
];

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn marker() -> Style {
    Style::default().fg(Color::Cyan)
}

/// `text` drawn `width` columns wide; 0 leaves wrapping to the pane.
pub(super) fn lines(text: &str, width: usize) -> Vec<Line<'static>> {
    lines_on(text, width, chrono::Local::now().date_naive())
}

fn lines_on(text: &str, width: usize, today: NaiveDate) -> Vec<Line<'static>> {
    let width = if width == 0 { u16::MAX as usize } else { width };
    let mut out = Vec::new();
    let mut fence = false;
    // the date and slug of the dated line above: its header covers this one
    let mut group: Option<(String, String)> = None;
    for line in text.lines().map(clean) {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = !fence;
            out.push(Line::styled("────", dim()));
            group = None;
            continue;
        }
        if fence {
            let code = vec![Span::styled(line, Style::default().fg(Color::Yellow))];
            out.extend(wrap(
                code,
                vec![Span::raw("  ")],
                vec![Span::raw("  ")],
                width,
            ));
            continue;
        }
        if let Some(c) = DATED.captures(&line) {
            let key = (
                c[1].to_string(),
                c.get(2).map_or("", |s| s.as_str()).to_string(),
            );
            if group.as_ref() != Some(&key) {
                if group.is_some() {
                    out.push(Line::default());
                }
                out.push(Line::from(dated_header(&key.0, &key.1, today)));
                group = Some(key);
            }
            let first = vec![Span::styled("• ", marker())];
            out.extend(wrap(
                inline(&c[3], Style::default()),
                first,
                vec![Span::raw("  ")],
                width,
            ));
            continue;
        }
        group = None;
        if let Some(c) = HEADING.captures(&line) {
            let style = marker().add_modifier(Modifier::BOLD);
            let mut spans = inline(&c[2], style);
            if c[1].len() <= 2 {
                let used = Line::from(spans.clone()).width() + 1;
                let fill = width.saturating_sub(used).min(200);
                if fill > 0 {
                    spans.push(Span::styled(format!(" {}", "─".repeat(fill)), dim()));
                }
            }
            out.extend(wrap(spans, vec![], vec![], width));
        } else if RULE.is_match(trimmed) {
            out.push(Line::styled("─".repeat(width.min(40)), dim()));
        } else if let Some(c) = ITEM.captures(&line) {
            let indent = c[1].len();
            let (mark, done) = match (c.get(3).map(|m| m.as_str()), &c[2]) {
                (Some(" "), _) => ("☐".to_string(), false),
                (Some(_), _) => ("☑".to_string(), true),
                (None, m) if m.ends_with(['.', ')']) => (m.to_string(), false),
                (None, _) if indent / 2 % 2 == 1 => ("◦".to_string(), false),
                (None, _) => ("•".to_string(), false),
            };
            let first = vec![
                Span::raw(" ".repeat(indent)),
                Span::styled(format!("{mark} "), marker()),
            ];
            let hang = indent + Span::raw(mark.as_str()).width() + 1;
            let base = if done { dim() } else { Style::default() };
            out.extend(wrap(
                inline(&c[4], base),
                first,
                vec![Span::raw(" ".repeat(hang))],
                width,
            ));
        } else if let Some(quote) = trimmed.strip_prefix('>') {
            let bar = || vec![Span::styled("▎ ", dim())];
            let text = inline(quote.trim_start(), Style::default().fg(Color::Gray));
            out.extend(wrap(text, bar(), bar(), width));
        } else {
            out.extend(wrap(inline(&line, Style::default()), vec![], vec![], width));
        }
    }
    out
}

/// `20 Oct · slug`: the date dim, the slug in its color; `today`, `yesterday`,
/// and the year only when it isn't this one.
fn dated_header(date: &str, slug: &str, today: NaiveDate) -> Vec<Span<'static>> {
    let shown = match NaiveDate::parse_from_str(date, "%Y-%m-%d") {
        Ok(d) if d == today => "today".to_string(),
        Ok(d) if today.pred_opt() == Some(d) => "yesterday".to_string(),
        Ok(d) if d.year() == today.year() => d.format("%-d %b").to_string(),
        Ok(d) => d.format("%-d %b %Y").to_string(),
        Err(_) => date.to_string(),
    };
    let mut spans = vec![Span::styled(shown, dim())];
    if !slug.is_empty() {
        let hash = slug
            .bytes()
            .fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize));
        let color = SLUG_COLORS[hash % SLUG_COLORS.len()];
        spans.push(Span::styled(" · ", dim()));
        spans.push(Span::styled(slug.to_string(), Style::default().fg(color)));
    }
    spans
}

/// `code`, **bold**, *italic*, ~~struck~~, [[wikilinks]], [links](url) and bare
/// urls in `base`, without their syntax; a link shows its text alone.
fn inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let link = base.fg(Color::LightBlue).add_modifier(Modifier::UNDERLINED);
    let mut spans = Vec::new();
    let mut at = 0;
    for c in INLINE.captures_iter(text) {
        let whole = c.get(0).unwrap();
        if whole.start() > at {
            spans.push(Span::styled(text[at..whole.start()].to_string(), base));
        }
        let group = |i: usize| c.get(i).map(|m| m.as_str());
        let (shown, style) = if let Some(code) = group(1) {
            (code, base.fg(Color::Yellow))
        } else if let Some(bold) = group(2).or(group(3)) {
            (bold, base.add_modifier(Modifier::BOLD))
        } else if let Some(struck) = group(4) {
            (struck, base.add_modifier(Modifier::CROSSED_OUT))
        } else if let Some(italic) = group(5) {
            (italic, base.add_modifier(Modifier::ITALIC))
        } else if let Some(target) = group(6) {
            (group(7).unwrap_or(target), link)
        } else if let Some(label) = group(8) {
            (label, link)
        } else {
            (whole.as_str(), dim().add_modifier(Modifier::UNDERLINED))
        };
        spans.push(Span::styled(shown.to_string(), style));
        at = whole.end();
    }
    if at < text.len() || spans.is_empty() {
        spans.push(Span::styled(text[at..].to_string(), base));
    }
    spans
}

/// `spans` wrapped at word boundaries to `width` columns: the first line starts
/// with `first`, the rest with `rest`. Leading spaces stay; a word wider than a
/// line is cut.
pub(super) fn wrap(
    spans: Vec<Span<'static>>,
    first: Vec<Span<'static>>,
    rest: Vec<Span<'static>>,
    width: usize,
) -> Vec<Line<'static>> {
    let span_width = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>();
    // (the spaces before a word, the word): a word may run across styles
    let mut words: Vec<(Vec<Span<'static>>, Vec<Span<'static>>)> = Vec::new();
    let mut gap = Vec::new();
    let mut in_word = false;
    for span in &spans {
        for (space, run) in runs(&span.content) {
            let piece = Span::styled(run.to_string(), span.style);
            if space {
                gap.push(piece);
                in_word = false;
            } else if in_word {
                words.last_mut().unwrap().1.push(piece);
            } else {
                words.push((std::mem::take(&mut gap), vec![piece]));
                in_word = true;
            }
        }
    }

    let indent = span_width(&rest);
    let mut lines = Vec::new();
    let mut used = span_width(&first);
    let mut line = first;
    for (i, (gap, word)) in words.into_iter().enumerate() {
        let gap_width = span_width(&gap);
        if i > 0 && used + gap_width + span_width(&word) > width {
            lines.push(Line::from(std::mem::replace(&mut line, rest.clone())));
            used = indent;
        } else {
            used += gap_width;
            line.extend(gap);
        }
        for mut piece in word {
            let mut w = piece.width();
            while w > 0 && used + w > width {
                let (head, tail) = split_at_width(&piece.content, width.saturating_sub(used));
                line.push(Span::styled(head, piece.style));
                lines.push(Line::from(std::mem::replace(&mut line, rest.clone())));
                used = indent;
                piece = Span::styled(tail, piece.style);
                w = piece.width();
            }
            used += w;
            line.push(piece);
        }
    }
    lines.push(Line::from(line));
    lines
}

/// `text` as runs of spaces and of the rest: (is space, run).
fn runs(text: &str) -> impl Iterator<Item = (bool, &str)> {
    let mut rest = text;
    std::iter::from_fn(move || {
        let space = rest.chars().next()?.is_whitespace();
        let end = rest
            .find(|c: char| c.is_whitespace() != space)
            .unwrap_or(rest.len());
        let (run, tail) = rest.split_at(end);
        rest = tail;
        Some((space, run))
    })
}

/// `text` cut after at most `max` columns, but after its first character at least.
fn split_at_width(text: &str, max: usize) -> (String, String) {
    let mut used = 0;
    for (i, c) in text.char_indices() {
        let w = Span::raw(&*c.encode_utf8(&mut [0; 4])).width();
        if i > 0 && used + w > max {
            return (text[..i].to_string(), text[i..].to_string());
        }
        used += w;
    }
    (text.to_string(), String::new())
}

#[cfg(test)]
mod tests {
    use super::{inline, lines_on};
    use chrono::NaiveDate;
    use ratatui::style::{Color, Modifier};
    use ratatui::text::Line;

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn draw(markdown: &str, width: usize) -> Vec<Line<'static>> {
        lines_on(
            markdown,
            width,
            NaiveDate::from_ymd_opt(2026, 10, 28).unwrap(),
        )
    }

    #[test]
    fn inline_syntax_turns_into_styles() {
        let spans = inline(
            "a `code` **b** *i* [[note#x|alias]] [mr](https://x.y/1) https://z",
            Default::default(),
        );
        let shown: Vec<_> = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            shown,
            [
                "a ",
                "code",
                " ",
                "b",
                " ",
                "i",
                " ",
                "alias",
                " ",
                "mr",
                " ",
                "https://z"
            ]
        );
        assert_eq!(spans[1].style.fg, Some(Color::Yellow));
        assert!(spans[3].style.add_modifier.contains(Modifier::BOLD));
        assert!(spans[5].style.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(spans[7].style.fg, Some(Color::LightBlue));
        assert_eq!(spans[9].style.fg, Some(Color::LightBlue));
    }

    #[test]
    fn wrapped_items_hang_under_their_text() {
        let lines = draw(
            "- one two three four\n  - five six seven\nplain words here",
            12,
        );
        let shown: Vec<_> = lines.iter().map(text).collect();
        assert_eq!(
            shown,
            [
                "• one two",
                "  three four",
                "  ◦ five six",
                "    seven",
                "plain words",
                "here"
            ]
        );
    }

    #[test]
    fn long_words_are_cut() {
        let lines = draw("- abcdefghij", 6);
        let shown: Vec<_> = lines.iter().map(text).collect();
        assert_eq!(shown, ["• abcd", "  efgh", "  ij"]);
    }

    #[test]
    fn dated_lines_group_under_date_and_slug() {
        let note = "## Agent notes\n\
            - 2026-10-20 filter: first\n\
            - 2026-10-20 filter: second\n\
            - 2026-10-27 filter: third\n\
            - 2026-10-28: mine\n\
            - 2025-01-02 old: before";
        let lines = draw(note, 40);
        let shown: Vec<_> = lines.iter().map(text).collect();
        assert_eq!(
            shown,
            [
                format!("Agent notes {}", "─".repeat(28)),
                "20 Oct · filter".into(),
                "• first".into(),
                "• second".into(),
                "".into(),
                "yesterday · filter".into(),
                "• third".into(),
                "".into(),
                "today".into(),
                "• mine".into(),
                "".into(),
                "2 Jan 2025 · old".into(),
                "• before".into(),
            ]
        );
        assert_eq!(lines[1].spans[0].style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn other_lines_pass_through() {
        let lines = draw(
            "- 2026-10-20 no colon here\n- [x] done\n```\nlet a;\n```",
            40,
        );
        let shown: Vec<_> = lines.iter().map(text).collect();
        assert_eq!(
            shown,
            [
                "• 2026-10-20 no colon here",
                "☑ done",
                "────",
                "  let a;",
                "────"
            ]
        );
        assert_eq!(lines[3].spans[1].style.fg, Some(Color::Yellow));
    }
}
