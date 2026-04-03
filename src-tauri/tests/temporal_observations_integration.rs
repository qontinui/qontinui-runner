//! Integration tests for temporal observation retrieval.
//!
//! These tests require a running PostgreSQL instance with the runner schema.
//!
//! Run with: cargo test --test temporal_observations_integration -- --ignored
//!
//! Requirements:
//! - PostgreSQL running on localhost:5432
//! - Database `qontinui_runner` with schema applied
//! - Environment variable DATABASE_URL set

use reqwest::Client;
use serde_json::json;
use std::time::Duration;

const BASE: &str = "http://localhost:9876";

fn client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

// ============================================================================
// Temporal search endpoint
// ============================================================================

#[tokio::test]
#[ignore]
async fn test_temporal_search_returns_results() {
    let resp = client()
        .get(format!(
            "{BASE}/observations/temporal-search?max_results=10"
        ))
        .send()
        .await
        .expect("Temporal search should respond");

    assert!(resp.status().is_success(), "Should return 200");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["success"].as_bool().unwrap_or(false),
        "Should be successful"
    );
}

#[tokio::test]
#[ignore]
async fn test_temporal_search_with_fts_query() {
    let resp = client()
        .get(format!(
            "{BASE}/observations/temporal-search?q=architecture&max_results=5"
        ))
        .send()
        .await
        .expect("Temporal FTS search should respond");

    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["success"].as_bool().unwrap_or(false));
}

#[tokio::test]
#[ignore]
async fn test_temporal_search_with_date_range() {
    let resp = client()
        .get(format!(
            "{BASE}/observations/temporal-search?from=2026-01-01T00:00:00Z&to=2026-12-31T23:59:59Z&max_results=20"
        ))
        .send()
        .await
        .expect("Date-range search should respond");

    assert!(resp.status().is_success());
}

// ============================================================================
// History endpoint
// ============================================================================

#[tokio::test]
#[ignore]
async fn test_observation_history_empty_for_nonexistent() {
    let resp = client()
        .get(format!("{BASE}/observations/999999/history"))
        .send()
        .await
        .expect("History endpoint should respond");

    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    let data = body["data"].as_array().unwrap();
    assert!(
        data.is_empty(),
        "Non-existent observation should have empty history"
    );
}

// ============================================================================
// Trends endpoint
// ============================================================================

#[tokio::test]
#[ignore]
async fn test_observation_trends() {
    let resp = client()
        .get(format!("{BASE}/observations/trends?weeks=4"))
        .send()
        .await
        .expect("Trends endpoint should respond");

    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["success"].as_bool().unwrap_or(false));
}

// ============================================================================
// Supersede endpoint
// ============================================================================

#[tokio::test]
#[ignore]
async fn test_supersede_nonexistent_returns_not_found() {
    let resp = client()
        .post(format!("{BASE}/observations/999999/supersede"))
        .json(&json!({ "new_observation_id": 1 }))
        .send()
        .await
        .expect("Supersede endpoint should respond");

    assert_eq!(
        resp.status().as_u16(),
        404,
        "Should return 404 for non-existent observation"
    );
}

// ============================================================================
// Full lifecycle: create -> update (history) -> supersede -> temporal query
// ============================================================================

#[tokio::test]
#[ignore]
async fn test_observation_temporal_lifecycle() {
    let c = client();

    // 1. Create an observation with a topic key
    let topic_key = format!("test/temporal-{}", chrono::Utc::now().timestamp_millis());
    let create_resp = c
        .post(format!("{BASE}/observations"))
        .json(&json!({
            "title": "Initial version",
            "content": "First version of this observation",
            "observationType": "architecture",
            "scope": "project",
            "topicKey": topic_key
        }))
        .send()
        .await
        .expect("Create should work");
    assert!(create_resp.status().is_success());
    let create_body: serde_json::Value = create_resp.json().await.unwrap();
    let obs_id = create_body["data"]["id"].as_i64().unwrap();

    // 2. Update via upsert (should create history entry)
    let upsert_resp = c
        .post(format!("{BASE}/observations"))
        .json(&json!({
            "title": "Updated version",
            "content": "Second version with new content",
            "observationType": "architecture",
            "scope": "project",
            "topicKey": topic_key
        }))
        .send()
        .await
        .expect("Upsert should work");
    assert!(upsert_resp.status().is_success());

    // 3. Check history has an entry
    let history_resp = c
        .get(format!("{BASE}/observations/{obs_id}/history"))
        .send()
        .await
        .expect("History should work");
    let history_body: serde_json::Value = history_resp.json().await.unwrap();
    let history = history_body["data"].as_array().unwrap();
    assert!(
        !history.is_empty(),
        "Should have at least one history entry after upsert"
    );
    assert_eq!(
        history[0]["title"].as_str().unwrap(),
        "Initial version",
        "History should contain the old version"
    );

    // 4. Get the observation — should have temporal fields
    let get_resp = c
        .get(format!("{BASE}/observations/{obs_id}"))
        .send()
        .await
        .expect("Get should work");
    let get_body: serde_json::Value = get_resp.json().await.unwrap();
    let obs = &get_body["data"];
    assert!(obs["validFrom"].is_string(), "Should have validFrom field");
    assert!(
        obs["validUntil"].is_null(),
        "Current observation should have null validUntil"
    );
    assert!(
        obs["supersededBy"].is_null(),
        "Should not be superseded yet"
    );

    // 5. Create a new observation and supersede the old one
    let new_resp = c
        .post(format!("{BASE}/observations"))
        .json(&json!({
            "title": "Replacement observation",
            "content": "This replaces the old one",
            "observationType": "architecture",
            "scope": "project"
        }))
        .send()
        .await
        .expect("Create replacement should work");
    let new_body: serde_json::Value = new_resp.json().await.unwrap();
    let new_id = new_body["data"]["id"].as_i64().unwrap();

    // 6. Supersede
    let supersede_resp = c
        .post(format!("{BASE}/observations/{obs_id}/supersede"))
        .json(&json!({ "new_observation_id": new_id }))
        .send()
        .await
        .expect("Supersede should work");
    assert!(supersede_resp.status().is_success());

    // 7. Verify the superseded observation has valid_until set
    let get_resp2 = c
        .get(format!("{BASE}/observations/{obs_id}"))
        .send()
        .await
        .expect("Get superseded should work");
    let get_body2: serde_json::Value = get_resp2.json().await.unwrap();
    let obs2 = &get_body2["data"];
    assert!(
        obs2["validUntil"].is_string(),
        "Superseded observation should have validUntil"
    );
    assert_eq!(
        obs2["supersededBy"].as_i64().unwrap(),
        new_id,
        "Should reference the new observation"
    );

    // 8. Temporal search should deprioritize the superseded observation
    let search_resp = c
        .get(format!(
            "{BASE}/observations/temporal-search?q=architecture&max_results=50"
        ))
        .send()
        .await
        .expect("Temporal search should work");
    let search_body: serde_json::Value = search_resp.json().await.unwrap();
    let results = search_body["data"].as_array().unwrap();

    // Find our observations in results
    let old_pos = results
        .iter()
        .position(|r| r["id"].as_i64() == Some(obs_id));
    let new_pos = results
        .iter()
        .position(|r| r["id"].as_i64() == Some(new_id));

    if let (Some(old_idx), Some(new_idx)) = (old_pos, new_pos) {
        assert!(
            new_idx < old_idx,
            "Current observation should rank before superseded one"
        );
    }

    // Cleanup: soft-delete both
    let _ = c
        .delete(format!("{BASE}/observations/{obs_id}"))
        .send()
        .await;
    let _ = c
        .delete(format!("{BASE}/observations/{new_id}"))
        .send()
        .await;
}
