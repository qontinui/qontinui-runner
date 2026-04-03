use regex::Regex;
use reqwest::Client;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use crate::knowledge_acquisition::types::{
    KnowledgeDomain, KnowledgeMetadata, KnowledgeResult, SearchProvider,
};

const DUCKDUCKGO_URL: &str = "https://html.duckduckgo.com/html/";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
const RATE_LIMIT_MS: u64 = 2000;
const MAX_RETRIES: usize = 3;

// Cached regexes — compiled once, reused across all calls
static URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<a[^>]*class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#).unwrap()
});
static SNIPPET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<a[^>]*class="result__snippet"[^>]*>(.*?)</a>"#).unwrap());
static FALLBACK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"href="(https?://[^"]+)"[^>]*>([^<]+)</a>.*?class="result__snippet"[^>]*>([^<]+)"#)
        .unwrap()
});
static HTML_TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());

pub struct DuckDuckGo {
    client: Client,
    last_request: Arc<Mutex<Instant>>,
}

impl DuckDuckGo {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            last_request: Arc::new(Mutex::new(Instant::now() - Duration::from_secs(10))),
        }
    }

    /// Search DuckDuckGo and return parsed results
    pub async fn search(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<KnowledgeResult>, String> {
        let max_results = max_results.clamp(1, 10);

        // Rate limit
        self.wait_for_rate_limit().await;

        let html = self.fetch_with_retry(query).await?;
        let results = parse_results(&html, query, max_results);

        Ok(results)
    }

    async fn wait_for_rate_limit(&self) {
        let mut last = self.last_request.lock().await;
        let elapsed = last.elapsed();
        let rate_limit = Duration::from_millis(RATE_LIMIT_MS);
        if elapsed < rate_limit {
            tokio::time::sleep(rate_limit - elapsed).await;
        }
        *last = Instant::now();
    }

    async fn fetch_with_retry(&self, query: &str) -> Result<String, String> {
        let mut last_error = String::new();

        for attempt in 0..MAX_RETRIES {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(1000 * attempt as u64)).await;
            }

            match self
                .client
                .post(DUCKDUCKGO_URL)
                .header("User-Agent", USER_AGENT)
                .header("Accept", "text/html")
                .header("Accept-Language", "en-US,en;q=0.9")
                .form(&[("q", query)])
                .timeout(Duration::from_secs(30))
                .send()
                .await
            {
                Ok(resp) => {
                    if resp.status().is_success() {
                        match resp.text().await {
                            Ok(text) => return Ok(text),
                            Err(e) => last_error = format!("Failed to read response body: {e}"),
                        }
                    } else {
                        last_error = format!("HTTP {}", resp.status());
                    }
                }
                Err(e) => {
                    last_error = format!("Request failed: {e}");
                }
            }
        }

        Err(format!(
            "DuckDuckGo search failed after {MAX_RETRIES} attempts: {last_error}"
        ))
    }
}

/// Parse DuckDuckGo HTML search results using cached regexes
fn parse_results(html: &str, query: &str, max_results: usize) -> Vec<KnowledgeResult> {
    let mut results = Vec::new();

    let all_urls: Vec<_> = URL_RE.captures_iter(html).collect();
    let all_snippets: Vec<_> = SNIPPET_RE.captures_iter(html).collect();

    for (i, url_cap) in all_urls.iter().enumerate() {
        if results.len() >= max_results {
            break;
        }

        let raw_url = url_cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let raw_title = url_cap.get(2).map(|m| m.as_str()).unwrap_or("");

        let url = decode_ddg_url(raw_url);
        let title = strip_html_tags(raw_title);
        let snippet = all_snippets
            .get(i)
            .and_then(|c| c.get(1))
            .map(|m| strip_html_tags(m.as_str()))
            .unwrap_or_default();

        if url.is_empty() || title.is_empty() {
            continue;
        }

        results.push(KnowledgeResult {
            provider: SearchProvider::DuckDuckGo,
            query: query.to_string(),
            title: title.clone(),
            content: format!("## {title}\n\n{snippet}\n\nSource: {url}"),
            url: Some(url),
            relevance_score: None,
            metadata: KnowledgeMetadata {
                domain: Some(KnowledgeDomain::General),
                ..Default::default()
            },
        });
    }

    // Fallback: if the structured parsing found nothing, try a simpler pattern
    if results.is_empty() {
        for cap in FALLBACK_RE.captures_iter(html) {
            if results.len() >= max_results {
                break;
            }
            let url = cap
                .get(1)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            let title = cap
                .get(2)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            let snippet = cap
                .get(3)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();

            if !url.is_empty() && !title.is_empty() {
                results.push(KnowledgeResult {
                    provider: SearchProvider::DuckDuckGo,
                    query: query.to_string(),
                    title: title.clone(),
                    content: format!("## {title}\n\n{snippet}\n\nSource: {url}"),
                    url: Some(url),
                    relevance_score: None,
                    metadata: KnowledgeMetadata {
                        domain: Some(KnowledgeDomain::General),
                        ..Default::default()
                    },
                });
            }
        }
    }

    results
}

/// Decode DuckDuckGo redirect URLs
/// DDG wraps result URLs like: //duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com&rut=...
fn decode_ddg_url(raw: &str) -> String {
    if raw.contains("duckduckgo.com/l/") || raw.contains("duckduckgo.com/l?") {
        if let Some(start) = raw.find("uddg=") {
            let encoded = &raw[start + 5..];
            let end = encoded.find('&').unwrap_or(encoded.len());
            let encoded = &encoded[..end];
            return urlencoding_decode(encoded);
        }
    }

    if raw.starts_with("//") {
        return format!("https:{raw}");
    }

    raw.to_string()
}

/// Percent-decoding that correctly handles multi-byte UTF-8 sequences.
/// Decodes into raw bytes first, then interprets as UTF-8.
fn urlencoding_decode(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len());
    let mut iter = s.bytes();
    while let Some(b) = iter.next() {
        if b == b'%' {
            let hi = iter.next().unwrap_or(b'0');
            let lo = iter.next().unwrap_or(b'0');
            let hex = [hi, lo];
            if let Ok(hex_str) = std::str::from_utf8(&hex) {
                if let Ok(val) = u8::from_str_radix(hex_str, 16) {
                    bytes.push(val);
                    continue;
                }
            }
            bytes.push(b'%');
            bytes.push(hi);
            bytes.push(lo);
        } else if b == b'+' {
            bytes.push(b' ');
        } else {
            bytes.push(b);
        }
    }
    String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Strip HTML tags from a string
fn strip_html_tags(s: &str) -> String {
    let text = HTML_TAG_RE.replace_all(s, "").to_string();
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_ddg_url_redirect() {
        let raw = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpath&rut=abc";
        assert_eq!(decode_ddg_url(raw), "https://example.com/path");
    }

    #[test]
    fn test_decode_ddg_url_direct() {
        assert_eq!(decode_ddg_url("https://example.com"), "https://example.com");
    }

    #[test]
    fn test_decode_ddg_url_protocol_relative() {
        assert_eq!(
            decode_ddg_url("//example.com/page"),
            "https://example.com/page"
        );
    }

    #[test]
    fn test_strip_html_tags() {
        assert_eq!(
            strip_html_tags("<b>bold</b> and <i>italic</i>"),
            "bold and italic"
        );
        assert_eq!(strip_html_tags("no &amp; tags"), "no & tags");
    }

    #[test]
    fn test_urlencoding_decode() {
        assert_eq!(urlencoding_decode("hello%20world"), "hello world");
        assert_eq!(
            urlencoding_decode("https%3A%2F%2Fexample.com"),
            "https://example.com"
        );
        assert_eq!(urlencoding_decode("a+b"), "a b");
    }

    #[test]
    fn test_urlencoding_decode_multibyte_utf8() {
        // é = U+00E9 = UTF-8 bytes C3 A9
        assert_eq!(urlencoding_decode("%C3%A9"), "é");
        // 日 = U+65E5 = UTF-8 bytes E6 97 A5
        assert_eq!(urlencoding_decode("%E6%97%A5"), "日");
    }

    #[test]
    fn test_parse_results_with_sample_html() {
        let html = r#"
        <div class="result results_links results_links_deep web-result">
            <div class="links_main links_deep result__body">
                <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fstackoverflow.com%2Fquestions%2F12345">How to fix Rust borrow error</a>
                <a class="result__snippet">The borrow checker prevents you from using a value after it has been moved. Try using a reference instead.</a>
            </div>
        </div>
        "#;

        let results = parse_results(html, "rust borrow error", 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "How to fix Rust borrow error");
        assert!(results[0]
            .url
            .as_ref()
            .unwrap()
            .contains("stackoverflow.com"));
        assert!(results[0].content.contains("borrow checker"));
    }

    #[test]
    fn test_parse_results_max_limit() {
        let mut html = String::new();
        for i in 0..10 {
            html.push_str(&format!(
                r#"<a class="result__a" href="https://example.com/{i}">Result {i}</a>
                   <a class="result__snippet">Snippet {i}</a>"#
            ));
        }
        let results = parse_results(&html, "test", 3);
        assert!(results.len() <= 3);
    }

    #[test]
    fn test_parse_results_empty_html() {
        let results = parse_results("<html><body>No results</body></html>", "query", 5);
        assert!(results.is_empty());
    }
}
