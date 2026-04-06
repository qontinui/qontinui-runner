//! Immutable resource version snapshots.
//!
//! Inspired by agent-lightning's resource versioning: every prompt, config, or
//! rule change creates a new immutable version with a monotonically increasing
//! version number. This guarantees reproducibility — you can always trace which
//! exact resource version produced a given outcome.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::runtime::Handle;
use tracing::debug;

use crate::database::pg::PgDb;

/// An immutable versioned snapshot of a resource (prompt, config, rule).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceVersion {
    pub id: String,
    pub resource_type: String,
    pub resource_key: String,
    pub version: i64,
    pub content_hash: String,
    pub content: String,
    pub metadata_json: Option<String>,
    pub created_at: String,
}

/// Compute SHA-256 hash of content for deduplication and drift detection.
pub fn content_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Create a new immutable version snapshot.
///
/// The version number is auto-incremented based on the latest version for
/// the (resource_type, resource_key) pair. Returns the new version number.
pub async fn create_version(
    pg_db: &PgDb,
    resource_type: &str,
    resource_key: &str,
    content: &str,
    metadata: Option<&str>,
) -> Result<i64, String> {
    let hash = content_hash(content);

    // Get next version number
    let latest = pg_db
        .get_latest_resource_version(resource_type, resource_key)
        .await?;
    let next_version = latest.map(|v| v.version + 1).unwrap_or(1);

    let id = format!("rv-{}", uuid::Uuid::new_v4());

    pg_db
        .create_resource_version(&id, resource_type, resource_key, next_version, &hash, content, metadata)
        .await?;

    debug!(
        resource_type,
        resource_key,
        version = next_version,
        "Created resource version"
    );

    Ok(next_version)
}

/// Create a version snapshot synchronously (for use in sync contexts like prompt_registry).
pub fn create_version_sync(
    pg_db: &Arc<PgDb>,
    resource_type: &str,
    resource_key: &str,
    content: &str,
    metadata: Option<&str>,
) -> Result<i64, String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(create_version(pg_db, resource_type, resource_key, content, metadata))
    })
}

/// Get a specific version of a resource.
pub async fn get_version(
    pg_db: &PgDb,
    resource_type: &str,
    resource_key: &str,
    version: i64,
) -> Result<Option<ResourceVersion>, String> {
    pg_db
        .get_resource_version(resource_type, resource_key, version)
        .await
}

/// Get the latest version of a resource.
pub async fn get_latest_version(
    pg_db: &PgDb,
    resource_type: &str,
    resource_key: &str,
) -> Result<Option<ResourceVersion>, String> {
    pg_db
        .get_latest_resource_version(resource_type, resource_key)
        .await
}

/// Compute a simple line-based diff between two versions.
///
/// Returns a human-readable string showing additions and removals.
pub fn diff_versions(a: &ResourceVersion, b: &ResourceVersion) -> String {
    let lines_a: Vec<&str> = a.content.lines().collect();
    let lines_b: Vec<&str> = b.content.lines().collect();

    let mut diff = format!(
        "--- v{} ({})\n+++ v{} ({})\n",
        a.version, a.content_hash.get(..8).unwrap_or("?"),
        b.version, b.content_hash.get(..8).unwrap_or("?"),
    );

    // Simple LCS-based diff (adequate for prompt comparison)
    let max_len = lines_a.len().max(lines_b.len());
    let mut i = 0;
    let mut j = 0;

    while i < lines_a.len() || j < lines_b.len() {
        if i < lines_a.len() && j < lines_b.len() && lines_a[i] == lines_b[j] {
            diff.push_str(&format!(" {}\n", lines_a[i]));
            i += 1;
            j += 1;
        } else if j < lines_b.len()
            && (i >= lines_a.len()
                || lines_a[i..].iter().any(|&l| l == lines_b[j]))
        {
            diff.push_str(&format!("+{}\n", lines_b[j]));
            j += 1;
        } else if i < lines_a.len() {
            diff.push_str(&format!("-{}\n", lines_a[i]));
            i += 1;
        }

        // Safety: prevent infinite loops on pathological input
        if i + j > max_len * 3 {
            diff.push_str("... (diff truncated)\n");
            break;
        }
    }

    diff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_hash_deterministic() {
        let h1 = content_hash("Hello, world!");
        let h2 = content_hash("Hello, world!");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // SHA-256 hex length
    }

    #[test]
    fn test_content_hash_different_inputs() {
        let h1 = content_hash("prompt v1");
        let h2 = content_hash("prompt v2");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_diff_versions_identical() {
        let a = ResourceVersion {
            id: "rv-1".to_string(),
            resource_type: "prompt".to_string(),
            resource_key: "verifier".to_string(),
            version: 1,
            content_hash: content_hash("line1\nline2"),
            content: "line1\nline2".to_string(),
            metadata_json: None,
            created_at: "2026-01-01".to_string(),
        };
        let b = a.clone();
        let diff = diff_versions(&a, &b);
        assert!(diff.contains(" line1"));
        assert!(diff.contains(" line2"));
        assert!(!diff.contains("+"));
        assert!(!diff.contains("-line"));
    }

    #[test]
    fn test_diff_versions_additions() {
        let a = ResourceVersion {
            id: "rv-1".to_string(),
            resource_type: "prompt".to_string(),
            resource_key: "verifier".to_string(),
            version: 1,
            content_hash: content_hash("line1\nline2"),
            content: "line1\nline2".to_string(),
            metadata_json: None,
            created_at: "2026-01-01".to_string(),
        };
        let b = ResourceVersion {
            version: 2,
            content_hash: content_hash("line1\nline2\nline3"),
            content: "line1\nline2\nline3".to_string(),
            ..a.clone()
        };
        let diff = diff_versions(&a, &b);
        assert!(diff.contains("+line3"));
    }

    #[test]
    fn test_diff_versions_removals() {
        let a = ResourceVersion {
            id: "rv-1".to_string(),
            resource_type: "prompt".to_string(),
            resource_key: "verifier".to_string(),
            version: 1,
            content_hash: content_hash("line1\nline2\nline3"),
            content: "line1\nline2\nline3".to_string(),
            metadata_json: None,
            created_at: "2026-01-01".to_string(),
        };
        let b = ResourceVersion {
            version: 2,
            content_hash: content_hash("line1\nline3"),
            content: "line1\nline3".to_string(),
            ..a.clone()
        };
        let diff = diff_versions(&a, &b);
        assert!(diff.contains("-line2"));
    }
}
