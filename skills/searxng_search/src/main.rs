use jumabek_sdk::{MethodInfo, ModuleMetadata, SkillError, SkillModule, SkillOutput};

struct SearxngSearch {
    metadata: ModuleMetadata,
}

impl SearxngSearch {
    fn new() -> Self {
        SearxngSearch {
            metadata: ModuleMetadata {
                name: "searxng_search".to_string(),
                version: "0.2.0".to_string(),
                description: "Search the web via LOCAL SearXNG container at http://localhost:8888 — aggregates Google, Bing, DuckDuckGo and more".to_string(),
            },
        }
    }
}

#[async_trait::async_trait]
impl SkillModule for SearxngSearch {
    fn get_metadata(&self) -> &ModuleMetadata {
        &self.metadata
    }
    fn health_check(&self) -> bool {
        true
    }
    fn available_methods(&self) -> Vec<MethodInfo> {
        vec![
            MethodInfo {
                method: "search".to_string(),
                description:
                    "Search the web via local SearXNG. Returns result title, snippet and URL."
                        .to_string(),
                args_description: "A search query string (e.g. 'latest news Kazakhstan 2026')"
                    .to_string(),
            },
            MethodInfo {
                method: "search_news".to_string(),
                description: "Search news specifically via local SearXNG (uses news engines)."
                    .to_string(),
                args_description: "A news query string".to_string(),
            },
        ]
    }
    async fn execute(&self, method: &str, args: &str) -> Result<SkillOutput, SkillError> {
        let (query_encoded, category) = match method {
            "search" => (urlencode(args), "general".to_string()),
            "search_news" => (urlencode(args), "news".to_string()),
            other => return Err(SkillError::NotFound(format!("unknown method '{}'", other))),
        };
        let base = "http://localhost:8888";
        let url = format!(
            "{}/search?q={}&format=json&categories={}&language=en&pageno=1",
            base, query_encoded, category
        );
        let resp = ureq::get(&url)
            .set(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) JumaBek/1.0",
            )
            .call()
            .map_err(|e| {
                SkillError::ExecutionFailed(format!("local SearXNG request failed: {}", e))
            })?;
        let body = resp
            .into_string()
            .map_err(|e| SkillError::ExecutionFailed(format!("failed to read body: {}", e)))?;
        #[derive(serde::Deserialize)]
        struct SearxngResponse {
            results: Vec<ResultItem>,
        }
        #[derive(serde::Deserialize)]
        struct ResultItem {
            title: Option<String>,
            content: Option<String>,
            url: Option<String>,
            engine: Option<String>,
        }
        let parsed: Result<SearxngResponse, _> = serde_json::from_str(&body);
        match parsed {
            Ok(data) => {
                let mut lines: Vec<String> = Vec::new();
                if data.results.is_empty() {
                    lines.push("No results found.".to_string());
                } else {
                    for (i, r) in data.results.iter().enumerate().take(25) {
                        let title = r.title.as_deref().unwrap_or("Untitled");
                        let snippet = r.content.as_deref().unwrap_or("");
                        let url = r.url.as_deref().unwrap_or("");
                        let engine = r.engine.as_deref().unwrap_or("");
                        lines.push(format!("{}. {}", i + 1, title));
                        if !snippet.is_empty() {
                            lines.push(format!("   {}", snippet));
                        }
                        if !url.is_empty() {
                            lines.push(format!("   🔗 {}", url));
                        }
                        if !engine.is_empty() {
                            lines.push(format!("   (via {})", engine));
                        }
                        lines.push(String::new());
                    }
                }
                Ok(SkillOutput::Text(lines.join("\n")))
            }
            Err(e) => {
                let snippet = if body.len() > 1000 {
                    format!("{}...", &body[..1000])
                } else {
                    body.clone()
                };
                Ok(SkillOutput::Text(format!(
                    "local SearXNG returned unparseable JSON ({}). Raw response (first 1000 chars):\n{}",
                    e, snippet
                )))
            }
        }
    }
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "%20".to_string(),
            _ => format!("%{:02X}", b),
        })
        .collect()
}

#[tokio::main]
async fn main() {
    jumabek_sdk::runtime::run_skill(SearxngSearch::new())
        .await
        .unwrap();
}
