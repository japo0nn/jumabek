use std::sync::OnceLock;

use rust_stemmers::{Algorithm, Stemmer};

const MAX_TERMS: usize = 24;
const MIN_TERM_LEN: usize = 2;

fn russian() -> &'static Stemmer {
    static STEMMER: OnceLock<Stemmer> = OnceLock::new();
    STEMMER.get_or_init(|| Stemmer::create(Algorithm::Russian))
}

fn english() -> &'static Stemmer {
    static STEMMER: OnceLock<Stemmer> = OnceLock::new();
    STEMMER.get_or_init(|| Stemmer::create(Algorithm::English))
}

fn is_cyrillic(word: &str) -> bool {
    word.chars()
        .any(|c| matches!(c, 'а'..='я' | 'А'..='Я' | 'ё' | 'Ё'))
}

pub fn stem_word(word: &str) -> String {
    let lowered = word.to_lowercase();
    let stemmer = if is_cyrillic(&lowered) {
        russian()
    } else {
        english()
    };
    stemmer.stem(&lowered).to_string()
}

pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= MIN_TERM_LEN)
        .map(stem_word)
        .collect()
}

pub fn to_search_text(text: &str) -> String {
    tokenize(text).join(" ")
}

pub fn build_match_query(raw: &str) -> Option<String> {
    let mut seen: Vec<String> = Vec::new();

    for stem in tokenize(raw) {
        if !seen.contains(&stem) {
            seen.push(stem);
        }
        if seen.len() >= MAX_TERMS {
            break;
        }
    }

    if seen.is_empty() {
        return None;
    }

    Some(
        seen.iter()
            .map(|s| format!("\"{}\"", s))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_russian_word_forms_together() {
        let base = stem_word("файл");
        for form in ["файлы", "файлов", "файлами", "файлу", "ФАЙЛАХ"] {
            assert_eq!(stem_word(form), base, "form {form} did not fold");
        }
    }

    #[test]
    fn folds_english_word_forms_together() {
        let base = stem_word("file");
        for form in ["files", "FILES"] {
            assert_eq!(stem_word(form), base, "form {form} did not fold");
        }
    }

    #[test]
    fn a_query_and_the_stored_text_meet_in_the_middle() {
        let stored = to_search_text("Открывал файлы в папке Документы");
        let query = build_match_query("файл документ").unwrap();

        for term in ["файл", "документ"] {
            let stem = stem_word(term);
            assert!(stored.contains(&stem), "index lost {stem}: {stored}");
            assert!(query.contains(&stem), "query lost {stem}: {query}");
        }
    }

    #[test]
    fn neutralises_fts_operators_and_punctuation() {
        let query = build_match_query("NOT a real query").unwrap();
        assert!(!query.contains(" NOT "), "got: {query}");

        assert!(build_match_query("предыдущие задачи с файлом doc.txt").is_some());
        assert!(build_match_query("C:/Users/sosa — что я делал?").is_some());
    }

    #[test]
    fn drops_duplicate_stems() {
        let query = build_match_query("файл файлы файлов файлами").unwrap();
        assert_eq!(query.matches(" OR ").count(), 0, "got: {query}");
    }

    #[test]
    fn returns_none_when_nothing_searchable() {
        assert!(build_match_query("?! -- ...").is_none());
        assert!(build_match_query("").is_none());
    }

    #[test]
    fn keeps_numbers_and_extensions_findable() {
        let stored = to_search_text("отчёт за 2026 год лежит в doc.txt");
        for needle in ["2026", "doc", "txt"] {
            assert!(stored.contains(needle), "index lost {needle}: {stored}");
        }
    }
}
