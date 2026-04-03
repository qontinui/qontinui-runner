//! Integration tests for knowledge acquisition providers.
//!
//! These tests hit real external APIs (DuckDuckGo, OSV.dev, Sploitus) directly
//! via HTTP without importing the knowledge_acquisition module (which is in the
//! bin crate, not the lib crate).
//!
//! Run with: cargo test --test knowledge_acquisition_integration -- --ignored
//!
//! Requirements:
//! - Network access
//! - No API keys needed (all free providers)

use reqwest::Client;
use std::time::Duration;

fn client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap()
}

// ============================================================================
// DuckDuckGo — HTML scraping
// ============================================================================

#[tokio::test]
#[ignore]
async fn test_duckduckgo_returns_html() {
    let resp = client()
        .post("https://html.duckduckgo.com/html/")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        )
        .form(&[("q", "rust reqwest async example")])
        .send()
        .await
        .expect("DuckDuckGo should respond");

    assert!(resp.status().is_success(), "Should return 200");

    let html = resp.text().await.unwrap();
    assert!(
        html.contains("result__a"),
        "HTML should contain result links"
    );
    assert!(
        html.contains("result__snippet"),
        "HTML should contain snippets"
    );

    println!("DuckDuckGo returned {} bytes of HTML", html.len());
}

#[tokio::test]
#[ignore]
async fn test_duckduckgo_error_message_search() {
    let resp = client()
        .post("https://html.duckduckgo.com/html/")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        )
        .form(&[(
            "q",
            "cannot borrow as mutable because it is also borrowed as immutable rust",
        )])
        .send()
        .await
        .expect("DuckDuckGo should respond");

    assert!(resp.status().is_success());
    let html = resp.text().await.unwrap();
    assert!(
        html.contains("result__a"),
        "Error message search should return results"
    );
    println!("Error search returned {} bytes", html.len());
}

// ============================================================================
// OSV.dev — JSON API
// ============================================================================

#[tokio::test]
#[ignore]
async fn test_osv_known_vulnerability() {
    let resp = client()
        .post("https://api.osv.dev/v1/query")
        .json(&serde_json::json!({
            "package": {
                "name": "lodash",
                "ecosystem": "npm"
            },
            "version": "4.17.20"
        }))
        .send()
        .await
        .expect("OSV should respond");

    assert!(resp.status().is_success(), "OSV should return 200");

    let body: serde_json::Value = resp.json().await.unwrap();
    let vulns = body["vulns"].as_array().expect("Should have vulns array");

    assert!(
        !vulns.is_empty(),
        "lodash 4.17.20 should have known vulnerabilities"
    );

    println!(
        "OSV found {} vulnerabilities for lodash@4.17.20",
        vulns.len()
    );
    for v in vulns.iter().take(3) {
        println!(
            "  - {} — {}",
            v["id"].as_str().unwrap_or("?"),
            v["summary"].as_str().unwrap_or("no summary")
        );
    }
}

#[tokio::test]
#[ignore]
async fn test_osv_batch_query() {
    let resp = client()
        .post("https://api.osv.dev/v1/querybatch")
        .json(&serde_json::json!({
            "queries": [
                { "package": { "name": "lodash", "ecosystem": "npm" }, "version": "4.17.20" },
                { "package": { "name": "express", "ecosystem": "npm" }, "version": "4.17.1" }
            ]
        }))
        .send()
        .await
        .expect("OSV batch should respond");

    assert!(resp.status().is_success());

    let body: serde_json::Value = resp.json().await.unwrap();
    let results = body["results"]
        .as_array()
        .expect("Should have results array");

    assert_eq!(results.len(), 2, "Should return results for both packages");
    println!("OSV batch results:");
    for (i, r) in results.iter().enumerate() {
        let count = r["vulns"].as_array().map(|a| a.len()).unwrap_or(0);
        println!("  Package {} — {} vulnerabilities", i, count);
    }
}

#[tokio::test]
#[ignore]
async fn test_osv_safe_package() {
    let resp = client()
        .post("https://api.osv.dev/v1/query")
        .json(&serde_json::json!({
            "package": {
                "name": "serde",
                "ecosystem": "crates.io"
            },
            "version": "1.0.219"
        }))
        .send()
        .await
        .expect("OSV should respond");

    assert!(resp.status().is_success());

    let body: serde_json::Value = resp.json().await.unwrap();
    let vulns = body
        .get("vulns")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    println!("OSV found {} vulnerabilities for serde@1.0.219", vulns);
}

// ============================================================================
// Sploitus — Exploit search
// ============================================================================

#[tokio::test]
#[ignore]
async fn test_sploitus_cve_search() {
    let resp = client()
        .post("https://sploitus.com/search")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        )
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "type": "exploits",
            "sort": "default",
            "query": "CVE-2021-44228",
            "title": false,
            "offset": 0
        }))
        .send()
        .await
        .expect("Sploitus should respond");

    assert!(resp.status().is_success(), "Sploitus should return 200");

    let body: serde_json::Value = resp.json().await.unwrap();
    let exploits = body["exploits"]
        .as_array()
        .expect("Should have exploits array");

    assert!(
        !exploits.is_empty(),
        "Log4Shell (CVE-2021-44228) should have exploit results"
    );

    println!(
        "Sploitus found {} results for CVE-2021-44228:",
        exploits.len()
    );
    for e in exploits.iter().take(3) {
        println!(
            "  - {} (score: {})",
            e["title"].as_str().unwrap_or("?"),
            e["score"].as_f64().unwrap_or(0.0)
        );
    }
}
