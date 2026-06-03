//! Speculative MCP / link pre-hydration (Track C3).
//!
//! Before the first agentic turn, the runner performs a handful of *obvious*
//! lookups so the first prompt can be primed with context the model would
//! otherwise have to discover via tool round-trips:
//!
//! 1. **coord `tools/list`** — enumerate available MCP tools so the prompt can
//!    hint what's callable.
//! 2. **URL / issue-ref fetch** — any `http(s)://` URLs or `#123` /
//!    `owner/repo#123` issue refs present in the task description are fetched
//!    (best-effort) so their gist is available up front.
//!
//! ## Strictly best-effort & never-blocking
//!
//! Every fetch here is wrapped so that a failure NEVER blocks or fails the
//! task — errors are swallowed + logged and setup proceeds. The whole
//! pre-hydration is also time-bounded.
//!
//! ## Honesty about staleness
//!
//! A pre-fetched tool list or URL body can go stale between pre-hydration and
//! the moment the model reads it. Every injected block is labeled with the
//! UTC timestamp at which it was fetched, so the model can reason about
//! freshness rather than trusting it blindly.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::commands::AppState;

/// Per-fetch timeout for speculative URL fetches.
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);
/// Overall budget for the whole pre-hydration pass.
const PREHYDRATION_BUDGET: Duration = Duration::from_secs(20);
/// Max number of URLs/refs to speculatively fetch (avoid runaway prompts).
const MAX_URLS: usize = 5;
/// Max chars kept from a single fetched URL body in the summary.
const MAX_URL_BODY_CHARS: usize = 1_500;
/// Max tool names listed in the summary before eliding the tail.
const MAX_TOOLS_LISTED: usize = 60;

/// A cached, timestamped pre-hydration summary for one execution.
#[derive(Debug, Clone)]
pub struct PrehydrationEntry {
    /// UTC RFC3339 timestamp at which the data was fetched.
    pub fetched_at: String,
    /// Compact, prompt-ready summary (already labeled with `fetched_at`).
    pub summary: String,
}

/// In-memory cache of pre-hydration summaries, keyed by `execution_id`.
///
/// Lives on [`AppState`]. Cleared opportunistically; a bounded number of
/// entries is fine since executions are short-lived and entries are small.
#[derive(Debug, Default)]
pub struct PrehydrationCache {
    inner: Mutex<HashMap<String, PrehydrationEntry>>,
}

impl PrehydrationCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store a pre-hydration summary for an execution.
    pub async fn put(&self, execution_id: &str, entry: PrehydrationEntry) {
        self.inner
            .lock()
            .await
            .insert(execution_id.to_string(), entry);
    }

    /// Fetch a previously stored summary, if any.
    pub async fn get(&self, execution_id: &str) -> Option<PrehydrationEntry> {
        self.inner.lock().await.get(execution_id).cloned()
    }

    /// Drop the entry for a completed execution.
    pub async fn remove(&self, execution_id: &str) {
        self.inner.lock().await.remove(execution_id);
    }
}

/// Run speculative pre-hydration for an execution and cache the result.
///
/// Never blocks the task: the whole pass is bounded by
/// [`PREHYDRATION_BUDGET`] and every individual lookup swallows its errors.
/// Returns the compact summary that was cached (also retrievable later via
/// [`PrehydrationCache::get`]), or `None` if nothing useful was gathered.
pub async fn prehydrate(
    app_state: &Arc<AppState>,
    execution_id: &str,
    task_description: &str,
) -> Option<String> {
    let fetched_at = chrono::Utc::now().to_rfc3339();

    // Bound the whole pass. On timeout we still cache whatever the inner
    // future managed to return before being dropped — but since `timeout`
    // drops the future, we instead build the summary inside the bounded
    // future and only cache on completion.
    let built = tokio::time::timeout(
        PREHYDRATION_BUDGET,
        build_summary(app_state, task_description, &fetched_at),
    )
    .await;

    let summary = match built {
        Ok(Some(s)) => s,
        Ok(None) => {
            debug!(
                "PREHYDRATION: nothing to pre-hydrate for execution {}",
                execution_id
            );
            return None;
        }
        Err(_) => {
            warn!(
                "PREHYDRATION: budget ({:?}) exceeded for execution {}; proceeding without pre-hydration",
                PREHYDRATION_BUDGET, execution_id
            );
            return None;
        }
    };

    app_state
        .prehydration_cache
        .put(
            execution_id,
            PrehydrationEntry {
                fetched_at,
                summary: summary.clone(),
            },
        )
        .await;

    info!(
        "PREHYDRATION: cached {} char summary for execution {}",
        summary.len(),
        execution_id
    );

    Some(summary)
}

/// Build the compact, timestamp-labeled summary. Each section is independently
/// best-effort.
async fn build_summary(
    app_state: &Arc<AppState>,
    task_description: &str,
    fetched_at: &str,
) -> Option<String> {
    let mut sections: Vec<String> = Vec::new();

    // (a) coord tools/list — available MCP tools.
    let tools = crate::runtime_env::get_available_mcp_tools(app_state).await;
    if !tools.is_empty() {
        let mut listed: Vec<String> = tools.iter().take(MAX_TOOLS_LISTED).cloned().collect();
        let elided = tools.len().saturating_sub(listed.len());
        if elided > 0 {
            listed.push(format!("…(+{} more)", elided));
        }
        sections.push(format!(
            "### Available MCP tools (pre-fetched {})\n{}",
            fetched_at,
            listed
                .iter()
                .map(|t| format!("- {}", t))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    // (b) URLs / issue refs in the task description.
    let urls = extract_urls(task_description);
    if !urls.is_empty() {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .user_agent("qontinui-runner-prehydration")
            .build()
            .ok();

        if let Some(client) = client {
            for url in urls.into_iter().take(MAX_URLS) {
                match fetch_url_gist(&client, &url).await {
                    Some(gist) => sections.push(format!(
                        "### Pre-fetched link: {} (fetched {})\n{}",
                        url, fetched_at, gist
                    )),
                    None => debug!("PREHYDRATION: failed to fetch {}", url),
                }
            }
        }
    }

    if sections.is_empty() {
        return None;
    }

    let mut out = String::new();
    out.push_str("## Pre-Hydrated Context (speculative, best-effort)\n\n");
    out.push_str(&format!(
        "The following was fetched at {} BEFORE this turn and MAY BE STALE. \
         Verify with a live tool call if correctness matters.\n\n",
        fetched_at
    ));
    out.push_str(&sections.join("\n\n"));
    out.push_str("\n\n---\n\n");
    Some(out)
}

/// Fetch a URL and return a truncated text gist, or `None` on any failure.
async fn fetch_url_gist(client: &reqwest::Client, url: &str) -> Option<String> {
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.text().await.ok()?;
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    let gist: String = trimmed.chars().take(MAX_URL_BODY_CHARS).collect();
    if gist.chars().count() < trimmed.chars().count() {
        Some(format!("{}…", gist))
    } else {
        Some(gist)
    }
}

/// Extract `http(s)://` URLs from free text. Issue refs like `owner/repo#123`
/// are normalized to a GitHub API/issue URL when they look like a GitHub ref.
fn extract_urls(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for raw in text.split_whitespace() {
        // Strip common trailing punctuation.
        let tok = raw.trim_matches(|c: char| {
            matches!(
                c,
                ',' | '.' | ';' | ')' | '(' | '"' | '\'' | '<' | '>' | ']' | '['
            )
        });

        if (tok.starts_with("http://") || tok.starts_with("https://"))
            && seen.insert(tok.to_string())
        {
            out.push(tok.to_string());
            continue;
        }

        // owner/repo#123 → GitHub issue URL.
        if let Some(hash) = tok.find('#') {
            let (repo, num) = tok.split_at(hash);
            let num = &num[1..];
            if !num.is_empty()
                && num.chars().all(|c| c.is_ascii_digit())
                && repo.matches('/').count() == 1
                && repo.split('/').all(|p| !p.is_empty())
            {
                let url = format!("https://github.com/{}/issues/{}", repo, num);
                if seen.insert(url.clone()) {
                    out.push(url);
                }
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_plain_urls() {
        let urls = extract_urls("see https://example.com/foo and http://bar.test/x.");
        assert_eq!(
            urls,
            vec![
                "https://example.com/foo".to_string(),
                "http://bar.test/x".to_string()
            ]
        );
    }

    #[test]
    fn test_extract_issue_ref() {
        let urls = extract_urls("fix qontinui/runner#318 please");
        assert_eq!(
            urls,
            vec!["https://github.com/qontinui/runner/issues/318".to_string()]
        );
    }

    #[test]
    fn test_extract_dedup_and_none() {
        let urls = extract_urls("no links here, just words and a#b (not a ref)");
        assert!(urls.is_empty());
        let urls = extract_urls("https://a.test https://a.test");
        assert_eq!(urls.len(), 1);
    }
}
