pub fn extract_json_payload(content: &str) -> String {
    let without_reasoning = strip_reasoning(content.trim());
    let trimmed = strip_code_fence(without_reasoning.trim());

    if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return trimmed.to_string();
    }

    let Some(candidate) = extract_braced_substring(trimmed) else {
        return trimmed.to_string();
    };

    if serde_json::from_str::<serde_json::Value>(&candidate).is_ok() {
        return candidate;
    }

    escape_raw_control_chars_in_strings(&candidate)
}

pub fn looks_truncated(content: &str) -> bool {
    let without_reasoning = strip_reasoning(content.trim());
    let trimmed = strip_code_fence(without_reasoning.trim());
    let has_open = trimmed.contains('{') || trimmed.contains('[');
    has_open && extract_braced_substring(trimmed).is_none()
}

fn strip_reasoning(text: &str) -> &str {
    const BLOCKS: [(&str, &str); 3] = [
        ("<think>", "</think>"),
        ("<thinking>", "</thinking>"),
        ("<reasoning>", "</reasoning>"),
    ];

    let mut rest = text;

    loop {
        let mut moved = false;

        for (open, close) in BLOCKS {
            let Some(start) = rest.find(open) else {
                continue;
            };

            rest = match rest[start..].find(close) {
                Some(end) => &rest[start + end + close.len()..],
                None => "",
            }
            .trim_start();

            moved = true;
        }

        if !moved {
            return rest;
        }
    }
}

fn strip_code_fence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };

    let body = match rest.find('\n') {
        Some(idx) => &rest[idx + 1..],
        None => return text,
    };

    body.trim_end()
        .strip_suffix("```")
        .map(|s| s.trim())
        .unwrap_or(body)
}

fn extract_braced_substring(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.iter().position(|&c| c == '{' || c == '[')?;

    let open = chars[start];
    let close = if open == '{' { '}' } else { ']' };

    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut end: Option<usize> = None;

    for (i, &c) in chars.iter().enumerate().skip(start) {
        if escaped {
            escaped = false;
            continue;
        }

        match c {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            c if !in_string && c == open => depth += 1,
            c if !in_string && c == close => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }

    end.map(|e| chars[start..=e].iter().collect())
}

fn escape_raw_control_chars_in_strings(json: &str) -> String {
    let mut result = String::with_capacity(json.len() + 16);
    let mut in_string = false;
    let mut escaped = false;

    for ch in json.chars() {
        if escaped {
            result.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' if in_string => {
                escaped = true;
                result.push(ch);
            }
            '"' => {
                in_string = !in_string;
                result.push(ch);
            }
            '\n' if in_string => result.push_str("\\n"),
            '\r' if in_string => result.push_str("\\r"),
            '\t' if in_string => result.push_str("\\t"),
            c if in_string && (c as u32) < 0x20 => {
                result.push_str(&format!("\\u{:04x}", c as u32));
            }
            _ => result.push(ch),
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parses(s: &str) -> bool {
        serde_json::from_str::<serde_json::Value>(s).is_ok()
    }

    #[test]
    fn passes_clean_json_through() {
        let raw = r#"{"message":"ok","is_done":true,"actions":[]}"#;
        assert_eq!(extract_json_payload(raw), raw);
    }

    #[test]
    fn unwraps_markdown_fence() {
        let raw = "```json\n{\"message\":\"ok\",\"is_done\":true,\"actions\":[]}\n```";
        let out = extract_json_payload(raw);
        assert!(parses(&out), "got: {out}");
        assert!(out.starts_with('{'));
    }

    #[test]
    fn strips_prose_around_json() {
        let raw =
            "Sure! Here is the result:\n{\"message\":\"ok\",\"actions\":[]}\nHope that helps.";
        let out = extract_json_payload(raw);
        assert!(parses(&out), "got: {out}");
        assert!(!out.contains("Hope"));
    }

    #[test]
    fn escapes_raw_newlines_inside_strings() {
        let raw = "{\"message\":\"line one\nline two\",\"actions\":[]}";
        assert!(!parses(raw));
        let out = extract_json_payload(raw);
        assert!(parses(&out), "got: {out}");
    }

    #[test]
    fn keeps_braces_inside_strings() {
        let raw = r#"{"message":"use {} for empty","actions":[]}"#;
        let out = extract_json_payload(raw);
        assert!(parses(&out));
        assert!(out.contains("use {} for empty"));
    }

    #[test]
    fn detects_truncation() {
        assert!(looks_truncated(r#"{"message":"cut off here"#));
        assert!(!looks_truncated(r#"{"message":"fine"}"#));
        assert!(!looks_truncated("no json at all"));
    }

    #[test]
    fn survives_plain_text() {
        assert_eq!(extract_json_payload("just words"), "just words");
    }

    #[test]
    fn a_reasoning_block_full_of_braces_does_not_win_over_the_answer() {
        let content = "<think>\nI should reply with {\"message\": ...} and set is_done.\n\
                       Maybe {\"actions\": []} too.\n</think>\n\
                       {\"message\":\"hi\",\"is_done\":true,\"actions\":[]}";

        let payload = extract_json_payload(content);
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("not JSON");

        assert_eq!(parsed["message"], "hi");
        assert_eq!(parsed["is_done"], true);
    }

    #[test]
    fn a_thought_that_never_closes_is_not_mined_for_braces() {
        let content = "<think>I will answer with {\"message\": something";

        assert!(
            !extract_json_payload(content).contains("message"),
            "a half-written thought was read as the answer"
        );
    }

    #[test]
    fn several_thoughts_in_a_row_are_all_dropped() {
        let content = "<think>first {</think> noise <thinking>second {</thinking>\
                       {\"message\":\"done\",\"is_done\":true,\"actions\":[]}";

        let parsed: serde_json::Value =
            serde_json::from_str(&extract_json_payload(content)).expect("not JSON");
        assert_eq!(parsed["message"], "done");
    }

    #[test]
    fn a_reply_with_no_reasoning_is_untouched() {
        let plain = "{\"message\":\"hi\",\"is_done\":true,\"actions\":[]}";
        assert_eq!(extract_json_payload(plain), plain);
    }

    #[test]
    fn a_thought_before_a_fenced_answer_still_finds_it() {
        let content = "<think>planning</think>\n```json\n                       {\"message\":\"hi\",\"is_done\":true,\"actions\":[]}\n```";

        let parsed: serde_json::Value =
            serde_json::from_str(&extract_json_payload(content)).expect("not JSON");
        assert_eq!(parsed["message"], "hi");
    }
}
