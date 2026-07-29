use std::sync::OnceLock;

use tiktoken_rs::CoreBPE;

const CHARS_PER_TOKEN_FALLBACK: usize = 4;
const PER_MESSAGE_OVERHEAD: usize = 4;

fn tokenizer() -> Option<&'static CoreBPE> {
    static TOKENIZER: OnceLock<Option<CoreBPE>> = OnceLock::new();
    TOKENIZER
        .get_or_init(|| match tiktoken_rs::cl100k_base() {
            Ok(bpe) => Some(bpe),
            Err(e) => {
                eprintln!("[token_counter] tokenizer unavailable, falling back to estimate: {e}");
                None
            }
        })
        .as_ref()
}

pub fn count(text: &str) -> usize {
    match tokenizer() {
        Some(bpe) => bpe.encode_ordinary(text).len(),
        None => text.chars().count().div_ceil(CHARS_PER_TOKEN_FALLBACK),
    }
}

pub fn count_message(role: &str, content: &str) -> usize {
    count(role) + count(content) + PER_MESSAGE_OVERHEAD
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_ascii() {
        assert!(count("hello world") > 0);
        assert!(count("hello world") < 5);
    }

    #[test]
    fn counts_cyrillic_higher_than_ascii() {
        let ru = count("привет мир");
        let en = count("hello world");
        assert!(ru > en, "cyrillic {ru} should cost more than ascii {en}");
    }

    #[test]
    fn empty_is_zero() {
        assert_eq!(count(""), 0);
    }

    #[test]
    fn message_adds_overhead() {
        let bare = count("hi");
        let msg = count_message("user", "hi");
        assert!(msg > bare + PER_MESSAGE_OVERHEAD - 1);
    }
}
