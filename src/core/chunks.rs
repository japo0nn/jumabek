use std::collections::HashMap;

use crate::core::languages::Language;

pub const MAX_CHUNKS: u32 = 64;
pub const MAX_MODULE_BYTES: usize = 512 * 1024;

#[derive(Debug, PartialEq)]
pub enum ChunkOutcome {
    Buffered {
        received: u32,
        total: u32,
    },
    Complete {
        code: String,
        dependencies: Vec<String>,
        language: Language,
    },
    Rejected(String),
}

#[derive(Debug, Default)]
struct Buffer {
    total: u32,
    parts: HashMap<u32, String>,
    dependencies: Vec<String>,
    bytes: usize,
    language: Language,
}

#[derive(Debug, Default)]
pub struct ChunkBuffers {
    modules: HashMap<String, Buffer>,
}

impl ChunkBuffers {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn pending(&self) -> Vec<&str> {
        self.modules.keys().map(|k| k.as_str()).collect()
    }

    pub fn forget(&mut self, module: &str) {
        self.modules.remove(module);
    }

    pub fn push(
        &mut self,
        module: &str,
        index: u32,
        total: u32,
        code: &str,
        dependencies: &[String],
        language: Language,
    ) -> ChunkOutcome {
        if total == 0 || total > MAX_CHUNKS {
            return ChunkOutcome::Rejected(format!(
                "total_chunks must be between 1 and {}, got {}",
                MAX_CHUNKS, total
            ));
        }
        if index == 0 || index > total {
            return ChunkOutcome::Rejected(format!(
                "chunk_index must be between 1 and {}, got {}",
                total, index
            ));
        }

        let buffer = self.modules.entry(module.to_string()).or_default();

        if buffer.parts.is_empty() {
            buffer.language = language;
        } else if buffer.language != language {
            let previous = buffer.language;
            self.modules.remove(module);
            return ChunkOutcome::Rejected(format!(
                "language changed from {} to {} halfway through; buffer dropped, start again \
                 from chunk 1 with one language for the whole module",
                previous, language
            ));
        }

        if buffer.total == 0 {
            buffer.total = total;
        } else if buffer.total != total {
            let previous = buffer.total;
            self.modules.remove(module);
            return ChunkOutcome::Rejected(format!(
                "total_chunks changed from {} to {} halfway through; buffer dropped, start again \
                 from chunk 1",
                previous, total
            ));
        }

        if buffer.bytes + code.len() > MAX_MODULE_BYTES {
            self.modules.remove(module);
            return ChunkOutcome::Rejected(format!(
                "module '{}' exceeds the {} KB limit; buffer dropped",
                module,
                MAX_MODULE_BYTES / 1024
            ));
        }

        if let Some(previous) = buffer.parts.insert(index, code.to_string()) {
            buffer.bytes -= previous.len();
        }
        buffer.bytes += code.len();

        for dependency in dependencies {
            if !buffer.dependencies.contains(dependency) {
                buffer.dependencies.push(dependency.clone());
            }
        }

        let received = buffer.parts.len() as u32;
        if received < buffer.total {
            return ChunkOutcome::Buffered {
                received,
                total: buffer.total,
            };
        }

        let buffer = self.modules.remove(module).expect("buffer just existed");

        let mut code = String::with_capacity(buffer.bytes + buffer.total as usize);
        for index in 1..=buffer.total {
            code.push_str(buffer.parts.get(&index).map(String::as_str).unwrap_or(""));
            code.push('\n');
        }

        ChunkOutcome::Complete {
            code,
            dependencies: buffer.dependencies,
            language: buffer.language,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffers() -> ChunkBuffers {
        ChunkBuffers::new()
    }

    #[test]
    fn a_single_chunk_completes_immediately() {
        let mut b = buffers();
        match b.push("m", 1, 1, "fn main() {}", &[], Language::Rust) {
            ChunkOutcome::Complete { code, .. } => assert!(code.contains("fn main")),
            other => panic!("unexpected: {:?}", other),
        }
        assert!(b.pending().is_empty());
    }

    #[test]
    fn chunks_are_assembled_in_index_order_even_if_they_arrive_shuffled() {
        let mut b = buffers();
        assert_eq!(
            b.push("m", 3, 3, "third", &[], Language::Rust),
            ChunkOutcome::Buffered {
                received: 1,
                total: 3
            }
        );
        b.push("m", 1, 3, "first", &[], Language::Rust);

        match b.push("m", 2, 3, "second", &[], Language::Rust) {
            ChunkOutcome::Complete { code, .. } => {
                assert_eq!(code, "first\nsecond\nthird\n");
            }
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn dependencies_from_every_chunk_are_merged_once() {
        let mut b = buffers();
        b.push("m", 1, 2, "a", &["regex".to_string()], Language::Rust);
        match b.push(
            "m",
            2,
            2,
            "b",
            &["regex".to_string(), "reqwest".to_string()],
            Language::Rust,
        ) {
            ChunkOutcome::Complete { dependencies, .. } => {
                assert_eq!(dependencies, vec!["regex", "reqwest"]);
            }
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn a_resent_chunk_replaces_the_old_one() {
        let mut b = buffers();
        b.push("m", 1, 2, "broken", &[], Language::Rust);
        b.push("m", 1, 2, "fixed", &[], Language::Rust);

        match b.push("m", 2, 2, "tail", &[], Language::Rust) {
            ChunkOutcome::Complete { code, .. } => {
                assert_eq!(code, "fixed\ntail\n");
                assert!(!code.contains("broken"));
            }
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn two_modules_do_not_mix() {
        let mut b = buffers();
        b.push("alpha", 1, 2, "A1", &[], Language::Rust);
        b.push("beta", 1, 2, "B1", &[], Language::Rust);
        b.push("beta", 2, 2, "B2", &[], Language::Rust);

        assert_eq!(b.pending(), vec!["alpha"]);
    }

    #[test]
    fn out_of_range_indexes_are_rejected() {
        let mut b = buffers();
        assert!(matches!(
            b.push("m", 0, 3, "x", &[], Language::Rust),
            ChunkOutcome::Rejected(_)
        ));
        assert!(matches!(
            b.push("m", 4, 3, "x", &[], Language::Rust),
            ChunkOutcome::Rejected(_)
        ));
        assert!(matches!(
            b.push("m", 1, 0, "x", &[], Language::Rust),
            ChunkOutcome::Rejected(_)
        ));
        assert!(matches!(
            b.push("m", 1, MAX_CHUNKS + 1, "x", &[], Language::Rust),
            ChunkOutcome::Rejected(_)
        ));
    }

    #[test]
    fn changing_the_total_midway_drops_the_buffer() {
        let mut b = buffers();
        b.push("m", 1, 3, "a", &[], Language::Rust);
        assert!(matches!(
            b.push("m", 2, 5, "b", &[], Language::Rust),
            ChunkOutcome::Rejected(_)
        ));
        assert!(b.pending().is_empty(), "stale buffer survived");
    }

    #[test]
    fn an_oversized_module_is_dropped_instead_of_eating_memory() {
        let mut b = buffers();
        let big = "x".repeat(MAX_MODULE_BYTES / 2 + 1);

        b.push("m", 1, 3, &big, &[], Language::Rust);
        assert!(matches!(
            b.push("m", 2, 3, &big, &[], Language::Rust),
            ChunkOutcome::Rejected(_)
        ));
        assert!(b.pending().is_empty());
    }

    #[test]
    fn the_language_travels_with_the_finished_module() {
        let mut b = buffers();
        match b.push("m", 1, 1, "print('hi')", &[], Language::Python) {
            ChunkOutcome::Complete { language, .. } => assert_eq!(language, Language::Python),
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn changing_the_language_midway_drops_the_buffer() {
        let mut b = buffers();
        b.push("m", 1, 2, "a", &[], Language::Python);

        assert!(
            matches!(
                b.push("m", 2, 2, "b", &[], Language::Node),
                ChunkOutcome::Rejected(_)
            ),
            "half a Python skill and half a Node one would have been concatenated"
        );
        assert!(b.pending().is_empty(), "stale buffer survived");
    }

    #[test]
    fn forget_clears_a_half_built_module() {
        let mut b = buffers();
        b.push("m", 1, 2, "a", &[], Language::Rust);
        b.forget("m");
        assert!(b.pending().is_empty());
    }
}
