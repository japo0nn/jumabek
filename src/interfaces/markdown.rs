use colored::{ColoredString, Colorize};

pub const MAX_WIDTH: usize = 96;

#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    link: bool,
    dim: bool,
}

#[derive(Clone, Debug)]
struct Span {
    text: String,
    style: Style,
}

impl Span {
    fn width(&self) -> usize {
        self.text.chars().count()
    }
}

fn paint(span: &Span) -> String {
    let mut out: ColoredString = span.text.as_str().into();

    if span.style.code {
        out = out.yellow();
    }
    if span.style.link {
        out = out.bright_cyan().underline();
    }
    if span.style.dim {
        out = out.bright_black();
    }
    if span.style.bold {
        out = out.bold();
    }
    if span.style.italic {
        out = out.italic();
    }
    if span.style.strike {
        out = out.strikethrough();
    }

    out.to_string()
}

fn paint_all(spans: &[Span]) -> String {
    let mut out = String::new();
    let mut i = 0;

    // Neighbours sharing a style are painted as one run, so a bold phrase is
    // one escape sequence rather than one per word.
    while i < spans.len() {
        let style = spans[i].style;
        let mut text = String::new();
        while i < spans.len() && spans[i].style == style {
            text.push_str(&spans[i].text);
            i += 1;
        }
        out.push_str(&paint(&Span { text, style }));
    }

    out
}

fn plain(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

fn parse_inline(text: &str, base: Style) -> Vec<Span> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<Span> = Vec::new();
    let mut buffer = String::new();
    let mut i = 0;

    let flush = |buffer: &mut String, spans: &mut Vec<Span>| {
        if !buffer.is_empty() {
            spans.push(Span {
                text: std::mem::take(buffer),
                style: base,
            });
        }
    };

    while i < chars.len() {
        let rest = &chars[i..];

        if rest[0] == '\\' && rest.len() > 1 {
            buffer.push(rest[1]);
            i += 2;
            continue;
        }

        if rest[0] == '`'
            && let Some(end) = find(rest, 1, &['`'])
        {
            flush(&mut buffer, &mut spans);
            let mut style = base;
            style.code = true;
            spans.push(Span {
                text: rest[1..end].iter().collect(),
                style,
            });
            i += end + 1;
            continue;
        }

        if starts_with(rest, "**") || starts_with(rest, "__") {
            let marker = [rest[0], rest[1]];
            if let Some(end) = find_pair(rest, 2, marker) {
                flush(&mut buffer, &mut spans);
                let mut style = base;
                style.bold = true;
                let inner: String = rest[2..end].iter().collect();
                spans.extend(parse_inline(&inner, style));
                i += end + 2;
                continue;
            }
        }

        if starts_with(rest, "~~")
            && let Some(end) = find_pair(rest, 2, ['~', '~'])
        {
            flush(&mut buffer, &mut spans);
            let mut style = base;
            style.strike = true;
            let inner: String = rest[2..end].iter().collect();
            spans.extend(parse_inline(&inner, style));
            i += end + 2;
            continue;
        }

        if (rest[0] == '*' || rest[0] == '_')
            && rest.len() > 1
            && rest[1] != rest[0]
            && let Some(end) = find(rest, 1, &[rest[0]])
        {
            flush(&mut buffer, &mut spans);
            let mut style = base;
            style.italic = true;
            let inner: String = rest[1..end].iter().collect();
            spans.extend(parse_inline(&inner, style));
            i += end + 1;
            continue;
        }

        if rest[0] == '['
            && let Some(close) = find(rest, 1, &[']'])
            && rest.get(close + 1) == Some(&'(')
            && let Some(paren) = find(rest, close + 2, &[')'])
        {
            flush(&mut buffer, &mut spans);
            let label: String = rest[1..close].iter().collect();
            let url: String = rest[close + 2..paren].iter().collect();

            let mut style = base;
            style.link = true;
            spans.push(Span { text: label, style });

            // The URL is kept: a terminal cannot be clicked through a label.
            let mut faint = base;
            faint.dim = true;
            spans.push(Span {
                text: format!(" ({})", url),
                style: faint,
            });

            i += paren + 1;
            continue;
        }

        buffer.push(rest[0]);
        i += 1;
    }

    flush(&mut buffer, &mut spans);
    spans
}

fn starts_with(chars: &[char], prefix: &str) -> bool {
    let want: Vec<char> = prefix.chars().collect();
    chars.len() >= want.len() && chars[..want.len()] == want[..]
}

fn find(chars: &[char], from: usize, any_of: &[char]) -> Option<usize> {
    (from..chars.len()).find(|&i| any_of.contains(&chars[i]))
}

fn find_pair(chars: &[char], from: usize, marker: [char; 2]) -> Option<usize> {
    (from..chars.len().saturating_sub(1))
        .find(|&i| chars[i] == marker[0] && chars[i + 1] == marker[1])
}

fn wrap(spans: &[Span], width: usize) -> Vec<Vec<Span>> {
    if width == 0 {
        return vec![spans.to_vec()];
    }

    // A word is a run of spans with no space between them, so a link and the
    // full stop that follows it stay glued together across a wrap.
    let mut words: Vec<Vec<Span>> = Vec::new();
    let mut current: Vec<Span> = Vec::new();

    for span in spans {
        let mut pending = String::new();
        for ch in span.text.chars() {
            if ch == ' ' {
                if !pending.is_empty() {
                    current.push(Span {
                        text: std::mem::take(&mut pending),
                        style: span.style,
                    });
                }
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            } else {
                pending.push(ch);
            }
        }
        if !pending.is_empty() {
            current.push(Span {
                text: pending,
                style: span.style,
            });
        }
    }
    if !current.is_empty() {
        words.push(current);
    }

    let mut lines: Vec<Vec<Span>> = Vec::new();
    let mut line: Vec<Span> = Vec::new();
    let mut used = 0;

    // The separator is emitted before the next word rather than after the
    // previous one, so a line never ends in a space it cannot see.
    for word in words {
        let needed: usize = word.iter().map(Span::width).sum();
        let gap = if line.is_empty() { 0 } else { 1 };

        if used + gap + needed > width && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            used = 0;
        } else if gap == 1 {
            let before = line.last().map(|s| s.style).unwrap_or_default();
            let after = word.first().map(|s| s.style).unwrap_or_default();
            line.push(Span {
                text: " ".to_string(),
                style: if before == after {
                    before
                } else {
                    Style::default()
                },
            });
            used += 1;
        }

        used += needed;
        line.extend(word);
    }

    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }

    lines
}

fn is_rule(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 3
        && (trimmed.chars().all(|c| c == '-')
            || trimmed.chars().all(|c| c == '*')
            || trimmed.chars().all(|c| c == '_'))
}

fn heading_level(line: &str) -> Option<(u8, &str)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) {
        let rest = &line[hashes..];
        if rest.starts_with(' ') {
            return Some((hashes as u8, rest.trim()));
        }
    }
    None
}

fn bullet(line: &str) -> Option<(usize, &str)> {
    let indent = line.len() - line.trim_start().len();
    let trimmed = line.trim_start();
    for marker in ["- ", "* ", "+ ", "• "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return Some((indent, rest));
        }
    }
    None
}

fn ordered(line: &str) -> Option<(usize, String, &str)> {
    let indent = line.len() - line.trim_start().len();
    let trimmed = line.trim_start();
    let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    let rest = &trimmed[digits..];
    for marker in [". ", ") "] {
        if let Some(body) = rest.strip_prefix(marker) {
            return Some((indent, trimmed[..digits].to_string(), body));
        }
    }
    None
}

fn table_row(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') {
        return None;
    }
    let inner = trimmed.trim_start_matches('|').trim_end_matches('|');
    Some(
        inner
            .split('|')
            .map(|cell| cell.trim().to_string())
            .collect(),
    )
}

fn is_separator_row(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let body = cell.trim_matches(':');
            !body.is_empty() && body.chars().all(|c| c == '-')
        })
}

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Left,
    Center,
    Right,
}

fn alignments(cells: &[String]) -> Vec<Align> {
    cells
        .iter()
        .map(|cell| {
            let left = cell.starts_with(':');
            let right = cell.ends_with(':');
            match (left, right) {
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                _ => Align::Left,
            }
        })
        .collect()
}

fn pad(text: &str, painted: &str, width: usize, align: Align) -> String {
    let slack = width.saturating_sub(text.chars().count());
    match align {
        Align::Left => format!("{}{}", painted, " ".repeat(slack)),
        Align::Right => format!("{}{}", " ".repeat(slack), painted),
        Align::Center => {
            let left = slack / 2;
            format!(
                "{}{}{}",
                " ".repeat(left),
                painted,
                " ".repeat(slack - left)
            )
        }
    }
}

fn render_table(rows: &[Vec<String>], out: &mut Vec<String>) {
    let Some(header) = rows.first() else { return };

    let (aligns, body) = match rows.get(1) {
        Some(second) if is_separator_row(second) => (alignments(second), &rows[2..]),
        _ => (vec![Align::Left; header.len()], &rows[1..]),
    };

    let columns = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut widths = vec![0usize; columns];
    for row in rows.iter() {
        if is_separator_row(row) {
            continue;
        }
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }

    let align_of = |i: usize| *aligns.get(i).unwrap_or(&Align::Left);

    let rule = |left: &str, mid: &str, right: &str| {
        let mut line = String::from(left);
        for (i, w) in widths.iter().enumerate() {
            line.push_str(&"─".repeat(w + 2));
            line.push_str(if i + 1 == widths.len() { right } else { mid });
        }
        line.bright_black().to_string()
    };

    out.push(rule("┌", "┬", "┐"));

    let bar = "│".bright_black().to_string();
    let mut line = bar.clone();
    for (i, cell) in header.iter().enumerate() {
        let painted = cell.as_str().bright_white().bold().to_string();
        line.push_str(&format!(
            " {} {}",
            pad(cell, &painted, widths[i], align_of(i)),
            bar
        ));
    }
    out.push(line);
    out.push(rule("├", "┼", "┤"));

    for row in body {
        let mut line = bar.clone();
        for (i, width) in widths.iter().enumerate().take(columns) {
            let cell = row.get(i).cloned().unwrap_or_default();
            let spans = parse_inline(&cell, Style::default());
            let painted = paint_all(&spans);
            line.push_str(&format!(
                " {} {}",
                pad(&plain(&spans), &painted, *width, align_of(i)),
                bar
            ));
        }
        out.push(line);
    }

    out.push(rule("└", "┴", "┘"));
}

/// Renders markdown into styled terminal lines. `width` is the room available
/// for the text itself, with any indent already subtracted.
pub fn render(markdown: &str, width: usize) -> Vec<String> {
    let width = width.clamp(20, MAX_WIDTH);
    let mut out: Vec<String> = Vec::new();
    let lines: Vec<&str> = markdown.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_end();

        if let Some(rest) = trimmed.trim_start().strip_prefix("```") {
            let language = rest.trim().to_string();
            let mut body: Vec<String> = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                body.push(lines[i].to_string());
                i += 1;
            }
            i += 1;

            let inner = body
                .iter()
                .map(|l| l.chars().count())
                .chain(std::iter::once(language.chars().count() + 2))
                .max()
                .unwrap_or(0)
                .min(width.saturating_sub(2));

            let top = if language.is_empty() {
                format!("┌{}┐", "─".repeat(inner + 2))
            } else {
                let dashes = (inner + 1).saturating_sub(language.chars().count() + 2);
                format!("┌─ {} {}┐", language, "─".repeat(dashes))
            };
            out.push(top.bright_black().to_string());
            for body_line in &body {
                let slack = inner.saturating_sub(body_line.chars().count());
                out.push(format!(
                    "{} {}{} {}",
                    "│".bright_black(),
                    body_line.bright_white(),
                    " ".repeat(slack),
                    "│".bright_black()
                ));
            }
            out.push(
                format!("└{}┘", "─".repeat(inner + 2))
                    .bright_black()
                    .to_string(),
            );
            continue;
        }

        if trimmed.trim().is_empty() {
            out.push(String::new());
            i += 1;
            continue;
        }

        if is_rule(trimmed) {
            out.push("─".repeat(width).bright_black().to_string());
            i += 1;
            continue;
        }

        if let Some((level, text)) = heading_level(trimmed) {
            let spans = parse_inline(text, Style::default());
            let body = plain(&spans);
            let painted = match level {
                1 | 2 => body.bright_cyan().bold().to_string(),
                3 => body.bright_white().bold().to_string(),
                _ => body.white().bold().to_string(),
            };
            out.push(painted);
            if level <= 2 {
                let rule = "─".repeat(body.chars().count().min(width));
                out.push(rule.bright_black().to_string());
            }
            i += 1;
            continue;
        }

        if table_row(trimmed).is_some() {
            let mut rows: Vec<Vec<String>> = Vec::new();
            while i < lines.len() {
                match table_row(lines[i].trim_end()) {
                    Some(cells) => {
                        rows.push(cells);
                        i += 1;
                    }
                    None => break,
                }
            }
            render_table(&rows, &mut out);
            continue;
        }

        if let Some(rest) = trimmed.trim_start().strip_prefix("> ") {
            let spans = parse_inline(rest, Style::default());
            for wrapped in wrap(&spans, width.saturating_sub(2)) {
                out.push(format!("{} {}", "│".bright_cyan(), paint_all(&wrapped)));
            }
            i += 1;
            continue;
        }

        if let Some((indent, rest)) = bullet(trimmed) {
            let depth = indent / 2;
            let pad_left = "  ".repeat(depth);
            let marker = if depth == 0 { "•" } else { "◦" };
            let spans = parse_inline(rest, Style::default());
            let room = width.saturating_sub(pad_left.len() + 2);

            for (n, wrapped) in wrap(&spans, room).into_iter().enumerate() {
                if n == 0 {
                    out.push(format!(
                        "{}{} {}",
                        pad_left,
                        marker.bright_cyan(),
                        paint_all(&wrapped)
                    ));
                } else {
                    out.push(format!("{}  {}", pad_left, paint_all(&wrapped)));
                }
            }
            i += 1;
            continue;
        }

        if let Some((indent, number, rest)) = ordered(trimmed) {
            let pad_left = "  ".repeat(indent / 2);
            let label = format!("{}.", number);
            let spans = parse_inline(rest, Style::default());
            let room = width.saturating_sub(pad_left.len() + label.len() + 1);

            for (n, wrapped) in wrap(&spans, room).into_iter().enumerate() {
                if n == 0 {
                    out.push(format!(
                        "{}{} {}",
                        pad_left,
                        label.bright_cyan(),
                        paint_all(&wrapped)
                    ));
                } else {
                    out.push(format!(
                        "{}{} {}",
                        pad_left,
                        " ".repeat(label.len()),
                        paint_all(&wrapped)
                    ));
                }
            }
            i += 1;
            continue;
        }

        let spans = parse_inline(trimmed, Style::default());
        for wrapped in wrap(&spans, width) {
            out.push(paint_all(&wrapped));
        }
        i += 1;
    }

    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }

    out
}

/// Markdown with the syntax taken out, for anything that is spoken rather than
/// printed.
pub fn to_speech(markdown: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let lines: Vec<&str> = markdown.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        if line.trim_start().starts_with("```") {
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                i += 1;
            }
            i += 1;
            out.push("code block".to_string());
            continue;
        }

        if is_rule(line) || table_row(line).is_some_and(|c| is_separator_row(&c)) {
            i += 1;
            continue;
        }

        let stripped = if let Some((_, text)) = heading_level(line) {
            text.to_string()
        } else if let Some((_, rest)) = bullet(line) {
            rest.to_string()
        } else if let Some((_, _, rest)) = ordered(line) {
            rest.to_string()
        } else if let Some(rest) = line.trim_start().strip_prefix("> ") {
            rest.to_string()
        } else if let Some(cells) = table_row(line) {
            cells.join(", ")
        } else {
            line.to_string()
        };

        let spans = parse_inline(&stripped, Style::default());
        let mut text = String::new();
        for span in &spans {
            if span.style.dim && span.text.starts_with(" (http") {
                continue;
            }
            text.push_str(&span.text);
        }

        let text = text.trim().to_string();
        if !text.is_empty() {
            out.push(text);
        }
        i += 1;
    }

    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_render(markdown: &str, width: usize) -> Vec<String> {
        colored::control::set_override(false);
        let out = render(markdown, width);
        colored::control::unset_override();
        out
    }

    #[test]
    fn bold_markers_do_not_survive() {
        let out = plain_render("**iPhone Air 256GB:**", 60);
        assert_eq!(out, vec!["iPhone Air 256GB:"]);
    }

    #[test]
    fn bold_inside_a_bullet_is_unwrapped() {
        let out = plain_render("- Facebook: **779 990 T**", 60);
        assert_eq!(out, vec!["• Facebook: 779 990 T"]);
    }

    #[test]
    fn a_heading_gets_a_rule_under_it() {
        let out = plain_render("## Prices", 60);
        assert_eq!(out, vec!["Prices", "──────"]);
    }

    #[test]
    fn inline_code_keeps_its_contents() {
        let out = plain_render("run `cargo build` now", 60);
        assert_eq!(out, vec!["run cargo build now"]);
    }

    #[test]
    fn a_link_shows_label_and_url() {
        let out = plain_render("see [the docs](https://x.dev)", 60);
        assert_eq!(out, vec!["see the docs (https://x.dev)"]);
    }

    #[test]
    fn long_text_wraps_at_the_given_width() {
        let out = plain_render(&"word ".repeat(20), 24);
        assert!(out.len() > 1);
        for line in &out {
            assert!(line.chars().count() <= 24, "line too long: {:?}", line);
        }
    }

    #[test]
    fn wrapping_never_leaves_a_trailing_space() {
        let out = plain_render(&"word ".repeat(20), 24);
        for line in &out {
            assert_eq!(line.trim_end(), line.as_str());
        }
    }

    #[test]
    fn cyrillic_wraps_by_characters_not_bytes() {
        let out = plain_render(&"цена ".repeat(20), 24);
        for line in &out {
            assert!(line.chars().count() <= 24, "line too long: {:?}", line);
        }
    }

    #[test]
    fn a_code_block_is_boxed() {
        let out = plain_render("```rust\nfn main() {}\n```", 60);
        assert!(out[0].starts_with("┌─ rust"));
        assert!(out[1].contains("fn main() {}"));
        assert!(out.last().unwrap().starts_with("└"));
    }

    #[test]
    fn a_code_block_is_rectangular() {
        for source in [
            "```bash\ncurl -s https://x | grep p\n```",
            "```\nplain\n```",
            "```rust\nfn a() {}\nfn bbbbbbbbbbbbbbbb() {}\n```",
        ] {
            let out = plain_render(source, 60);
            let widths: Vec<usize> = out.iter().map(|l| l.chars().count()).collect();
            assert!(
                widths.iter().all(|w| *w == widths[0]),
                "ragged box for {:?}: {:?}",
                source,
                widths
            );
        }
    }

    #[test]
    fn punctuation_stays_with_the_link() {
        let out = plain_render("see [docs](https://x.dev).", 60);
        assert_eq!(out, vec!["see docs (https://x.dev)."]);
    }

    #[test]
    fn a_table_is_aligned() {
        let out = plain_render("| a | bbb |\n| --- | ---: |\n| 1 | 2 |", 60);
        assert_eq!(out.len(), 5);
        let widths: Vec<usize> = out.iter().map(|l| l.chars().count()).collect();
        assert!(
            widths.iter().all(|w| *w == widths[0]),
            "ragged table: {:?}",
            widths
        );
    }

    #[test]
    fn an_ordered_list_keeps_its_numbers() {
        let out = plain_render("1. first\n2. second", 60);
        assert_eq!(out, vec!["1. first", "2. second"]);
    }

    #[test]
    fn a_quote_is_barred() {
        let out = plain_render("> careful", 60);
        assert_eq!(out, vec!["│ careful"]);
    }

    #[test]
    fn an_unclosed_marker_is_left_alone() {
        let out = plain_render("2 ** 8 is 256", 60);
        assert_eq!(out, vec!["2 ** 8 is 256"]);
    }

    #[test]
    fn an_escaped_marker_is_literal() {
        let out = plain_render(r"\*not italic\*", 60);
        assert_eq!(out, vec!["*not italic*"]);
    }

    #[test]
    fn speech_drops_the_syntax() {
        let spoken = to_speech("## Prices\n\n- **iPhone**: `764 990`\n\n| a |\n| --- |\n| 1 |");
        assert_eq!(spoken, "Prices\niPhone: 764 990\na\n1");
    }

    #[test]
    fn speech_does_not_read_urls_aloud() {
        let spoken = to_speech("see [the docs](https://example.com/a/b)");
        assert_eq!(spoken, "see the docs");
    }

    #[test]
    fn speech_summarises_code_blocks() {
        let spoken = to_speech("here:\n```rust\nfn main() {}\n```\ndone");
        assert_eq!(spoken, "here:\ncode block\ndone");
    }

    #[test]
    fn empty_input_renders_nothing() {
        assert!(plain_render("", 60).is_empty());
        assert_eq!(to_speech(""), "");
    }
}
