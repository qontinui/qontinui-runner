use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::knowledge_acquisition::types::{
    KnowledgeDomain, KnowledgeMetadata, KnowledgeResult, SearchProvider,
};

const SPLOITUS_URL: &str = "https://sploitus.com/search";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_SOURCE_BYTES: usize = 50_000;

pub struct Sploitus {
    client: Client,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ExploitType {
    Exploits,
    Tools,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SortOrder {
    Default,
    Date,
    Score,
}

/// Request body for Sploitus search API
#[derive(Debug, Serialize)]
struct SploitusRequestBody {
    #[serde(rename = "type")]
    exploit_type: String,
    sort: String,
    query: String,
    title: bool,
    offset: usize,
}

// --- Response types ---

#[derive(Debug, Deserialize)]
struct SploitusApiResponse {
    #[serde(default)]
    exploits: Vec<SploitusExploit>,
}

#[derive(Debug, Clone, Deserialize)]
struct SploitusExploit {
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    href: String,
    #[serde(default)]
    score: Option<f64>,
    #[serde(default, rename = "publishedAt")]
    published: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    language: Option<String>,
}

impl Sploitus {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// Search for exploits and tools by query (software name, version, or CVE ID)
    pub async fn search(
        &self,
        query: &str,
        exploit_type: ExploitType,
        sort: SortOrder,
        max_results: usize,
    ) -> Result<Vec<KnowledgeResult>, String> {
        let max_results = max_results.clamp(1, 25);

        let body = SploitusRequestBody {
            exploit_type: match exploit_type {
                ExploitType::Exploits => "exploits".to_string(),
                ExploitType::Tools => "tools".to_string(),
            },
            sort: match sort {
                SortOrder::Default => "default".to_string(),
                SortOrder::Date => "date".to_string(),
                SortOrder::Score => "score".to_string(),
            },
            query: query.to_string(),
            title: false,
            offset: 0,
        };

        let resp = self
            .client
            .post(SPLOITUS_URL)
            .header("User-Agent", USER_AGENT)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&body)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("Sploitus search failed: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("Sploitus returned HTTP {}", resp.status()));
        }

        let response: SploitusApiResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse Sploitus response: {e}"))?;

        let mut results: Vec<KnowledgeResult> = response
            .exploits
            .into_iter()
            .take(max_results)
            .map(|e| exploit_to_result(&e, query))
            .collect();

        // Enforce total output cap (80KB)
        let mut total_bytes = 0;
        results.retain(|r| {
            total_bytes += r.content.len();
            total_bytes <= 80_000
        });

        Ok(results)
    }
}

/// Convert a Sploitus exploit to a KnowledgeResult
fn exploit_to_result(exploit: &SploitusExploit, query: &str) -> KnowledgeResult {
    let mut content = format!("## {} ({})\n\n", exploit.title, exploit.id);

    if let Some(score) = exploit.score {
        let severity = match score {
            s if s >= 9.0 => "CRITICAL",
            s if s >= 7.0 => "HIGH",
            s if s >= 4.0 => "MEDIUM",
            _ => "LOW",
        };
        content.push_str(&format!("**CVSS Score**: {score:.1} ({severity})\n"));
    }

    if !exploit.published.is_empty() {
        content.push_str(&format!("**Published**: {}\n", exploit.published));
    }

    if let Some(ref lang) = exploit.language {
        content.push_str(&format!("**Language**: {lang}\n"));
    }

    if !exploit.href.is_empty() {
        content.push_str(&format!("**Source**: {}\n", exploit.href));
    }

    // Include exploit source code, capped at MAX_SOURCE_BYTES
    if !exploit.source.is_empty() {
        content.push_str("\n### Exploit Code\n```\n");
        if exploit.source.len() > MAX_SOURCE_BYTES {
            let mut end = MAX_SOURCE_BYTES;
            while end > 0 && !exploit.source.is_char_boundary(end) {
                end -= 1;
            }
            content.push_str(&exploit.source[..end]);
            content.push_str("\n[... truncated]\n");
        } else {
            content.push_str(&exploit.source);
        }
        content.push_str("\n```\n");
    }

    // Extract CVE ID from title or ID if present
    let cve_id = extract_cve_id(&exploit.title)
        .or_else(|| extract_cve_id(&exploit.id))
        .or_else(|| extract_cve_id(query));

    KnowledgeResult {
        provider: SearchProvider::Sploitus,
        query: query.to_string(),
        title: exploit.title.clone(),
        content,
        url: if exploit.href.is_empty() {
            None
        } else {
            Some(exploit.href.clone())
        },
        relevance_score: exploit.score.map(|s| s / 10.0), // normalize to 0-1
        metadata: KnowledgeMetadata {
            domain: Some(KnowledgeDomain::VulnerabilityIntelligence),
            cvss_score: exploit.score,
            cve_id,
            ..Default::default()
        },
    }
}

/// Extract CVE ID from a string (e.g., "CVE-2021-44228")
fn extract_cve_id(s: &str) -> Option<String> {
    static CVE_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"CVE-\d{4}-\d{4,}").unwrap());
    CVE_RE.find(s).map(|m| m.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sploitus_response() {
        let json = r##"{
            "exploits": [{
                "id": "EDB-ID:51884",
                "title": "Apache 2.4.49 - Path Traversal",
                "href": "https://www.exploit-db.com/exploits/51884",
                "score": 7.5,
                "publishedAt": "2023-01-15",
                "source": "import requests",
                "language": "python"
            }]
        }"##;

        let response: SploitusApiResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.exploits.len(), 1);

        let result = exploit_to_result(&response.exploits[0], "apache 2.4.49");
        assert_eq!(result.provider, SearchProvider::Sploitus);
        assert!(result.content.contains("7.5"));
        assert!(result.content.contains("HIGH"));
        assert!(result.content.contains("import requests"));
        assert_eq!(result.metadata.cvss_score, Some(7.5));
    }

    #[test]
    fn test_extract_cve_id() {
        assert_eq!(
            extract_cve_id("Apache CVE-2021-44228 Log4Shell"),
            Some("CVE-2021-44228".to_string())
        );
        assert_eq!(extract_cve_id("no cve here"), None);
        assert_eq!(
            extract_cve_id("CVE-2024-12345"),
            Some("CVE-2024-12345".to_string())
        );
    }

    #[test]
    fn test_source_truncation() {
        let exploit = SploitusExploit {
            id: "TEST-1".to_string(),
            title: "Large exploit".to_string(),
            href: String::new(),
            score: None,
            published: String::new(),
            source: "x".repeat(60_000),
            language: None,
        };
        let result = exploit_to_result(&exploit, "test");
        assert!(result.content.len() < 55_000);
        assert!(result.content.contains("[... truncated]"));
    }

    #[test]
    fn test_empty_response() {
        let json = r#"{"exploits": []}"#;
        let response: SploitusApiResponse = serde_json::from_str(json).unwrap();
        assert!(response.exploits.is_empty());
    }
}
