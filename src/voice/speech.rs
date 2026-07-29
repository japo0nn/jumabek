const CODE_PLACEHOLDER: &str = "фрагмент кода";

pub fn to_speakable(text: &str) -> String {
    let without_code = strip_code_blocks(text);
    let without_links = strip_links(&without_code);
    let without_marks = strip_inline_marks(&without_links);
    let shortened = shorten_paths(&without_marks);
    collapse_whitespace(&shortened)
}

fn strip_code_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut inside = false;

    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if !inside {
                out.push_str(CODE_PLACEHOLDER);
                out.push('\n');
            }
            inside = !inside;
            continue;
        }

        if !inside {
            out.push_str(line);
            out.push('\n');
        }
    }

    out
}

fn strip_links(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '['
            && let Some(close) = find_from(&chars, i + 1, ']')
            && chars.get(close + 1) == Some(&'(')
            && let Some(paren) = find_from(&chars, close + 2, ')')
        {
            out.extend(&chars[i + 1..close]);
            i = paren + 1;
            continue;
        }

        out.push(chars[i]);
        i += 1;
    }

    out
}

fn find_from(chars: &[char], start: usize, needle: char) -> Option<usize> {
    chars[start..]
        .iter()
        .position(|&c| c == needle)
        .map(|p| p + start)
}

fn strip_inline_marks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());

    for line in text.lines() {
        let trimmed = line.trim_start();

        let body = trimmed
            .trim_start_matches('#')
            .trim_start_matches(['>', '|'])
            .trim_start();

        let body = match body.strip_prefix("- ").or_else(|| body.strip_prefix("* ")) {
            Some(rest) => rest,
            None => body,
        };

        if body
            .chars()
            .all(|c| c == '-' || c == '=' || c == '|' || c.is_whitespace())
            && !body.trim().is_empty()
        {
            continue;
        }

        for ch in body.chars() {
            match ch {
                '*' | '_' | '`' | '#' | '~' => {}
                other => out.push(other),
            }
        }
        out.push('\n');
    }

    out
}

fn shorten_paths(text: &str) -> String {
    text.split_whitespace()
        .map(|token| {
            let cleaned = token.trim_matches(|c: char| c == '"' || c == '\'' || c == ',');
            if looks_like_path(cleaned) {
                basename(cleaned).to_string()
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn looks_like_path(token: &str) -> bool {
    let separators = token.matches(['/', '\\']).count();
    if separators < 2 {
        return false;
    }
    !token.starts_with("http://") && !token.starts_with("https://")
}

fn basename(token: &str) -> &str {
    token
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(token)
}

fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_space = false;

    for ch in text.chars() {
        if ch.is_whitespace() {
            if !last_was_space && !out.is_empty() {
                out.push(' ');
            }
            last_was_space = true;
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }

    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_code_blocks() {
        let text = "Вот решение:\n```rust\nfn main() {}\n```\nГотово.";
        let out = to_speakable(text);
        assert!(!out.contains("fn main"), "got: {out}");
        assert!(out.contains(CODE_PLACEHOLDER), "got: {out}");
        assert!(out.contains("Готово"));
    }

    #[test]
    fn drops_inline_markup() {
        let out = to_speakable("## Заголовок\n**жирный** и `код` и _курсив_");
        assert_eq!(out, "Заголовок жирный и код и курсив");
    }

    #[test]
    fn keeps_link_text_drops_url() {
        let out = to_speakable("смотри [документацию](https://example.com/docs)");
        assert_eq!(out, "смотри документацию");
    }

    #[test]
    fn shortens_long_paths() {
        let out = to_speakable("файл лежит в C:/Users/sosa/Documents/doc.txt");
        assert!(out.ends_with("doc.txt"), "got: {out}");
        assert!(!out.contains("Users"), "got: {out}");
    }

    #[test]
    fn leaves_urls_alone() {
        let out = to_speakable("открой https://example.com/a/b/c");
        assert!(out.contains("https://example.com/a/b/c"), "got: {out}");
    }

    #[test]
    fn drops_table_rulers() {
        let out = to_speakable("| имя | тип |\n|---|---|\n| файл | текст |");
        assert!(!out.contains("---"), "got: {out}");
        assert!(out.contains("файл"));
    }

    #[test]
    fn flattens_bullets_and_whitespace() {
        let out = to_speakable("- первый\n- второй\n\n\n  третий");
        assert_eq!(out, "первый второй третий");
    }

    #[test]
    fn plain_sentence_survives_untouched() {
        let text = "В текущей папке 6 файлов.";
        assert_eq!(to_speakable(text), text);
    }
}
