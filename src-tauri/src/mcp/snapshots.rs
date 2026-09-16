//! `/sessions/<id>/snapshots` and `/sessions/<id>/rewind` HTTP endpoints
//! — Phase 4 of the productivity stack. Backs the `/rewind-session` slash
//! command (see `D:/qontinui-root/.claude/commands/rewind-session.md`).
//!
//! `GET /sessions/<id>/snapshots` lists every `session_file_snapshots`
//! row for the given session — exposed so an external caller can inspect
//! the rollback set before triggering a restore.
//!
//! `GET /sessions/<id>/file-changes` pairs every file the session touched
//! with its pre-edit snapshot text and the file's CURRENT text, so a reader
//! (the Terminal grid's `WorkerSessionCell`) can render a true
//! snapshot-vs-now diff whatever tool made the edit. The diff itself is
//! computed client-side; this route only reads and reports, and it reports a
//! read failure per file rather than dropping the file.
//!
//! `POST /sessions/<id>/rewind` performs the actual restore: for each
//! pre-edit snapshot, verify the on-disk blob's sha256 matches the
//! recorded `blob_sha256`, then copy the blob over the original
//! `file_path`. This avoids the slash command needing to orchestrate
//! `cp` calls inside the LLM tool-call context.

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;
use tracing::{info, warn};

use crate::database::pg::session_file_snapshots::SnapshotRow;
use crate::mcp::types::ApiState;

/// Per-side byte cap for the text returned by `GET /sessions/<id>/file-changes`.
/// A side above the cap is reported by size only (`truncated: true`, text
/// `None`) — a diff of a truncated file would be a lie, so none is offered.
pub const FILE_CHANGE_TEXT_CAP_BYTES: usize = 256 * 1024;

/// One file a session touched, paired with what it looked like BEFORE the
/// session's first edit and what it looks like NOW.
///
/// `status` is one of:
/// - `modified` — snapshot and current text differ;
/// - `unchanged` — same sha on both sides;
/// - `deleted` — a snapshot exists but the file is gone;
/// - `created` — the session touched a path that had no pre-edit snapshot
///   (the file did not exist when it was first edited) and exists now;
/// - `binary` — at least one side is not valid UTF-8;
/// - `unreadable` — a side could not be read; `detail` names why.
///
/// Both text sides are `None` whenever they cannot honestly be diffed
/// (`binary`, `unreadable`, or a side over [`FILE_CHANGE_TEXT_CAP_BYTES`]).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionFileChange {
    pub file_path: String,
    pub status: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub before_bytes: Option<usize>,
    pub after_bytes: Option<usize>,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
    pub truncated: bool,
    /// `taken_at` of the pre-edit snapshot; `None` for a `created` entry.
    pub taken_at: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFileChangesResponse {
    pub session_id: String,
    pub files: Vec<SessionFileChange>,
    /// Epoch millis the report was assembled, so a reader can label its age.
    pub read_at_ms: i64,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// One side of a file change as read from disk.
enum Side {
    Missing,
    Unreadable(String),
    Present(Vec<u8>),
}

fn read_side(read: &dyn Fn(&str) -> std::io::Result<Vec<u8>>, path: &str) -> Side {
    match read(path) {
        Ok(bytes) => Side::Present(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Side::Missing,
        Err(e) => Side::Unreadable(e.to_string()),
    }
}

/// Build the change list for a session from its snapshot rows and touched
/// paths. Pure over `read` so the pairing/status logic is unit-testable
/// without a filesystem.
///
/// - The FIRST `captured_before` snapshot per path is the "before" side
///   (the same rule `rewind_session_handler` applies); later rows are
///   ignored.
/// - A touched path with no snapshot is reported as `created` when it
///   exists now, and omitted when it does not (nothing to show on either
///   side — the session never left a file there).
/// - Order: snapshot rows in `taken_at` order, then snapshot-less touched
///   paths in touch order.
pub fn assemble_file_changes(
    snapshots: &[SnapshotRow],
    touched: &[String],
    read: &dyn Fn(&str) -> std::io::Result<Vec<u8>>,
) -> Vec<SessionFileChange> {
    let mut out: Vec<SessionFileChange> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for snap in snapshots.iter().filter(|s| s.captured_before) {
        if !seen.insert(snap.file_path.clone()) {
            continue;
        }
        let before = read_side(read, &snap.snapshot_blob_path);
        let after = read_side(read, &snap.file_path);
        out.push(pair_sides(
            &snap.file_path,
            Some(snap.taken_at.clone()),
            Some(snap.blob_sha256.as_str()),
            before,
            after,
            true,
        ));
    }

    for path in touched {
        if !seen.insert(path.clone()) {
            continue;
        }
        match read_side(read, path) {
            Side::Missing => continue,
            after => out.push(pair_sides(path, None, None, Side::Missing, after, false)),
        }
    }

    out
}

fn pair_sides(
    file_path: &str,
    taken_at: Option<String>,
    recorded_before_sha: Option<&str>,
    before: Side,
    after: Side,
    had_snapshot: bool,
) -> SessionFileChange {
    let mut change = SessionFileChange {
        file_path: file_path.to_string(),
        status: String::new(),
        before: None,
        after: None,
        before_bytes: None,
        after_bytes: None,
        before_sha256: None,
        after_sha256: None,
        truncated: false,
        taken_at,
        detail: None,
    };

    let before_bytes = match before {
        Side::Unreadable(why) => {
            change.status = "unreadable".to_string();
            change.detail = Some(format!("pre-edit snapshot unreadable: {why}"));
            return change;
        }
        Side::Missing if had_snapshot => {
            change.status = "unreadable".to_string();
            change.detail = Some("pre-edit snapshot blob is missing on disk".to_string());
            return change;
        }
        Side::Missing => None,
        Side::Present(b) => Some(b),
    };
    if let Some(b) = &before_bytes {
        let sha = sha256_hex(b);
        if let Some(recorded) = recorded_before_sha {
            if recorded != sha {
                change.status = "unreadable".to_string();
                change.detail = Some(format!(
                    "pre-edit snapshot blob sha256 mismatch: recorded={recorded}, actual={sha}"
                ));
                return change;
            }
        }
        change.before_bytes = Some(b.len());
        change.before_sha256 = Some(sha);
    }

    let after_bytes = match after {
        Side::Unreadable(why) => {
            change.status = "unreadable".to_string();
            change.detail = Some(format!("current file unreadable: {why}"));
            return change;
        }
        Side::Missing => None,
        Side::Present(b) => Some(b),
    };
    if let Some(b) = &after_bytes {
        change.after_bytes = Some(b.len());
        change.after_sha256 = Some(sha256_hex(b));
    }

    change.status = match (&before_bytes, &after_bytes) {
        (Some(_), None) => "deleted",
        (None, Some(_)) => "created",
        (Some(_), Some(_)) if change.before_sha256 == change.after_sha256 => "unchanged",
        (Some(_), Some(_)) => "modified",
        (None, None) => "unreadable",
    }
    .to_string();
    if change.status == "unreadable" {
        change.detail = Some("neither side exists".to_string());
        return change;
    }

    let before_text = before_bytes.as_deref().map(std::str::from_utf8);
    let after_text = after_bytes.as_deref().map(std::str::from_utf8);
    if matches!(before_text, Some(Err(_))) || matches!(after_text, Some(Err(_))) {
        change.status = "binary".to_string();
        return change;
    }
    let over_cap = before_bytes
        .as_ref()
        .is_some_and(|b| b.len() > FILE_CHANGE_TEXT_CAP_BYTES)
        || after_bytes
            .as_ref()
            .is_some_and(|b| b.len() > FILE_CHANGE_TEXT_CAP_BYTES);
    if over_cap {
        change.truncated = true;
        return change;
    }
    change.before = before_text.and_then(|r| r.ok()).map(str::to_string);
    change.after = after_text.and_then(|r| r.ok()).map(str::to_string);
    change
}

async fn file_changes_handler(
    State(state): State<Arc<ApiState>>,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<SessionFileChangesResponse>, (StatusCode, String)> {
    let pg = &state.app_state.pg_db;
    let snapshots = pg
        .get_snapshots_for_session(&session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let touched = pg
        .get_files_touched(&session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Disk reads are blocking; keep them off the async executor.
    let files = tokio::task::spawn_blocking(move || {
        assemble_file_changes(&snapshots, &touched, &|p| std::fs::read(p))
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("file-changes assembly panicked: {e}"),
        )
    })?;

    Ok(Json(SessionFileChangesResponse {
        session_id,
        files,
        read_at_ms: chrono::Utc::now().timestamp_millis(),
    }))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetSnapshotsResponse {
    pub session_id: String,
    pub snapshots: Vec<SnapshotRow>,
}

async fn get_snapshots_handler(
    State(state): State<Arc<ApiState>>,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<GetSnapshotsResponse>, (StatusCode, String)> {
    let snapshots = state
        .app_state
        .pg_db
        .get_snapshots_for_session(&session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(GetSnapshotsResponse {
        session_id,
        snapshots,
    }))
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RewindSessionRequest {
    /// Reserved for future use (e.g. dry-run, scope-by-path). The body
    /// is currently empty `{}` per the slash command's contract; we
    /// accept extra fields liberally.
    #[serde(default)]
    pub _placeholder: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindError {
    pub file_path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindSessionResponse {
    pub session_id: String,
    pub files_restored: usize,
    pub files_skipped: usize,
    pub errors: Vec<RewindError>,
}

/// Compute the sha256 of `path`'s contents as a lowercase hex string.
/// Returns `None` if the file cannot be read.
fn sha256_of_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Some(format!("{:x}", hasher.finalize()))
}

async fn rewind_session_handler(
    State(state): State<Arc<ApiState>>,
    AxumPath(session_id): AxumPath<String>,
    body: Option<Json<RewindSessionRequest>>,
) -> Result<Json<RewindSessionResponse>, (StatusCode, String)> {
    let _ = body; // body fields are reserved for future use

    let snapshots = state
        .app_state
        .pg_db
        .get_snapshots_for_session(&session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut files_restored = 0usize;
    let mut files_skipped = 0usize;
    let mut errors: Vec<RewindError> = Vec::new();

    // Only the FIRST captured-before snapshot per file_path is the
    // rollback target. Subsequent rows are informational. Walk in
    // taken_at ASC order (the SELECT already does this) and skip a
    // file_path once we've handled it.
    let mut handled: std::collections::HashSet<String> = std::collections::HashSet::new();

    for snap in &snapshots {
        if !snap.captured_before {
            continue;
        }
        if !handled.insert(snap.file_path.clone()) {
            // already restored from the first snapshot for this path
            files_skipped += 1;
            continue;
        }

        let blob_path = Path::new(&snap.snapshot_blob_path);
        if !blob_path.exists() {
            errors.push(RewindError {
                file_path: snap.file_path.clone(),
                reason: format!("blob missing: {}", snap.snapshot_blob_path),
            });
            continue;
        }

        let actual_sha = match sha256_of_file(blob_path) {
            Some(h) => h,
            None => {
                errors.push(RewindError {
                    file_path: snap.file_path.clone(),
                    reason: format!("blob unreadable: {}", snap.snapshot_blob_path),
                });
                continue;
            }
        };
        if actual_sha != snap.blob_sha256 {
            errors.push(RewindError {
                file_path: snap.file_path.clone(),
                reason: format!(
                    "blob sha256 mismatch: stored={}, actual={}",
                    snap.blob_sha256, actual_sha
                ),
            });
            continue;
        }

        // Ensure the destination's parent dir exists (the file may
        // have been deleted by the failed worker).
        if let Some(parent) = Path::new(&snap.file_path).parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                errors.push(RewindError {
                    file_path: snap.file_path.clone(),
                    reason: format!("create parent dir failed: {}", e),
                });
                continue;
            }
        }

        match std::fs::copy(blob_path, Path::new(&snap.file_path)) {
            Ok(_) => {
                files_restored += 1;
                info!(
                    "rewind_session: restored {} from blob {} (session={})",
                    snap.file_path, snap.snapshot_blob_path, session_id
                );
            }
            Err(e) => {
                errors.push(RewindError {
                    file_path: snap.file_path.clone(),
                    reason: format!("copy failed: {}", e),
                });
            }
        }
    }

    if !errors.is_empty() {
        warn!(
            "rewind_session: {} restored, {} errored, session={}",
            files_restored,
            errors.len(),
            session_id
        );
    }

    Ok(Json(RewindSessionResponse {
        session_id,
        files_restored,
        files_skipped,
        errors,
    }))
}

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        // Avoid the v0.7-syntax false positive on `{name}` captures, mirroring
        // the workaround in `mcp/ai_session.rs::routes`.
        .without_v07_checks()
        .route(
            "/sessions/{session_id}/snapshots",
            get(get_snapshots_handler),
        )
        .route(
            "/sessions/{session_id}/rewind",
            post(rewind_session_handler),
        )
        .route(
            "/sessions/{session_id}/file-changes",
            get(file_changes_handler),
        )
}

#[cfg(test)]
mod file_changes_tests {
    use super::*;
    use std::collections::HashMap;

    fn snap(path: &str, blob: &str, sha: &str, before: bool) -> SnapshotRow {
        SnapshotRow {
            id: format!("id-{path}"),
            session_id: "s1".to_string(),
            file_path: path.to_string(),
            snapshot_blob_path: blob.to_string(),
            blob_sha256: sha.to_string(),
            captured_before: before,
            taken_at: "2026-09-15T00:00:00Z".to_string(),
        }
    }

    fn fs(entries: &[(&str, &[u8])]) -> HashMap<String, Vec<u8>> {
        entries
            .iter()
            .map(|(p, b)| (p.to_string(), b.to_vec()))
            .collect()
    }

    fn reader(map: HashMap<String, Vec<u8>>) -> impl Fn(&str) -> std::io::Result<Vec<u8>> {
        move |p: &str| match map.get(p) {
            Some(b) => Ok(b.clone()),
            None if p.starts_with("EIO:") => Err(std::io::Error::other("disk on fire")),
            None => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
        }
    }

    #[test]
    fn modified_file_carries_both_sides_and_shas() {
        let before = b"a\nb\n";
        let sha = sha256_hex(before);
        let read = reader(fs(&[("/blob/1", before), ("/src/x.rs", b"a\nc\n")]));
        let out = assemble_file_changes(&[snap("/src/x.rs", "/blob/1", &sha, true)], &[], &read);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].status, "modified");
        assert_eq!(out[0].before.as_deref(), Some("a\nb\n"));
        assert_eq!(out[0].after.as_deref(), Some("a\nc\n"));
        assert_eq!(out[0].before_sha256.as_deref(), Some(sha.as_str()));
        assert!(!out[0].truncated);
        assert_eq!(out[0].taken_at.as_deref(), Some("2026-09-15T00:00:00Z"));
    }

    #[test]
    fn unchanged_deleted_and_created_are_told_apart() {
        let text = b"same\n";
        let sha = sha256_hex(text);
        let read = reader(fs(&[
            ("/blob/same", text),
            ("/src/same.rs", text),
            ("/blob/gone", text),
            ("/src/new.rs", b"fresh\n"),
        ]));
        let snaps = [
            snap("/src/same.rs", "/blob/same", &sha, true),
            snap("/src/gone.rs", "/blob/gone", &sha, true),
        ];
        let touched = [
            "/src/same.rs".to_string(),
            "/src/new.rs".to_string(),
            "/src/never-landed.rs".to_string(),
        ];
        let out = assemble_file_changes(&snaps, &touched, &read);
        let by_path: HashMap<_, _> = out.iter().map(|c| (c.file_path.as_str(), c)).collect();
        assert_eq!(by_path["/src/same.rs"].status, "unchanged");
        assert_eq!(by_path["/src/gone.rs"].status, "deleted");
        assert_eq!(by_path["/src/gone.rs"].after, None);
        assert_eq!(by_path["/src/new.rs"].status, "created");
        assert_eq!(by_path["/src/new.rs"].before, None);
        assert_eq!(by_path["/src/new.rs"].after.as_deref(), Some("fresh\n"));
        // A touched path that exists on neither side is not a change.
        assert!(!by_path.contains_key("/src/never-landed.rs"));
        // Snapshot rows come first, in row order; touched-only paths follow.
        assert_eq!(out[0].file_path, "/src/same.rs");
        assert_eq!(out[1].file_path, "/src/gone.rs");
        assert_eq!(out[2].file_path, "/src/new.rs");
    }

    #[test]
    fn a_failed_read_is_reported_not_dropped() {
        let sha = sha256_hex(b"x");
        let read = reader(fs(&[("/blob/ok", b"x"), ("/blob/x", b"x")]));
        let snaps = [
            // blob missing on disk
            snap("/src/a.rs", "/blob/missing", &sha, true),
            // current file unreadable (not ENOENT)
            snap("EIO:/src/b.rs", "/blob/ok", &sha, true),
            // recorded sha disagrees with the blob's bytes
            snap("/src/c.rs", "/blob/x", "deadbeef", true),
        ];
        let out = assemble_file_changes(&snaps, &[], &read);
        assert_eq!(out.len(), 3);
        for c in &out {
            assert_eq!(c.status, "unreadable", "{c:?}");
            assert!(c.detail.is_some(), "{c:?}");
            assert_eq!(c.before, None);
            assert_eq!(c.after, None);
        }
        assert!(out[0].detail.as_deref().unwrap().contains("missing"));
        assert!(out[1].detail.as_deref().unwrap().contains("disk on fire"));
        assert!(out[2].detail.as_deref().unwrap().contains("mismatch"));
    }

    #[test]
    fn binary_and_oversized_sides_carry_no_text() {
        let bin = [0xff_u8, 0xfe, 0x00];
        let sha_bin = sha256_hex(&bin);
        let big = vec![b'a'; FILE_CHANGE_TEXT_CAP_BYTES + 1];
        let sha_big = sha256_hex(&big);
        let read = reader(fs(&[
            ("/blob/bin", &bin),
            ("/src/bin", b"text now"),
            ("/blob/big", &big),
            ("/src/big", b"small now"),
        ]));
        let snaps = [
            snap("/src/bin", "/blob/bin", &sha_bin, true),
            snap("/src/big", "/blob/big", &sha_big, true),
        ];
        let out = assemble_file_changes(&snaps, &[], &read);
        assert_eq!(out[0].status, "binary");
        assert_eq!(out[0].before, None);
        assert_eq!(out[1].status, "modified");
        assert!(out[1].truncated);
        assert_eq!(out[1].before, None);
        assert_eq!(out[1].before_bytes, Some(FILE_CHANGE_TEXT_CAP_BYTES + 1));
    }

    #[test]
    fn only_the_first_pre_edit_snapshot_per_path_counts() {
        let first = b"first\n";
        let second = b"second\n";
        let read = reader(fs(&[
            ("/blob/first", first),
            ("/blob/second", second),
            ("/src/x", b"now\n"),
        ]));
        let snaps = [
            snap("/src/x", "/blob/first", &sha256_hex(first), true),
            snap("/src/x", "/blob/second", &sha256_hex(second), true),
            snap("/src/x", "/blob/second", &sha256_hex(second), false),
        ];
        let out = assemble_file_changes(&snaps, &["/src/x".to_string()], &read);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].before.as_deref(), Some("first\n"));
    }
}
