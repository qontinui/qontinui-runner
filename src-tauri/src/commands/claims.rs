//! Tauri commands bridging the webview to coord's claims API.
//!
//! Plan reference:
//! `D:/qontinui-root/plans/2026-05-18-agent-spawn-coordination.md` Phase 3.
//!
//! Three thin wrappers around `POST /claims/acquire`, `POST /claims/release`,
//! and `POST /coord/claims/steal`. The webview's `ConflictModal` calls
//! `claims_acquire` (retry after wait/steal), `claims_steal` (on the user
//! pressing Steal), and the agent-spawn flow itself can release on
//! completion via `claims_release`. Heartbeats are NOT user-driven —
//! the runner-side spawn flow spawns a tokio task at acquire time and
//! the webview never sees them.
//!
//! All three commands use the same coord-HTTP-base resolution chain
//! as `mcp::agent_worktrees::coord_http_base`: env `COORD_HTTP_URL` →
//! profile `coord_url` (ws→http) → `http://localhost:9870`. JWT is
//! NOT applied here — `/claims/acquire` is gated behind no auth on
//! coord today (Phase 2 spec §1.1); `/coord/claims/steal` accepts
//! either an admin-secret header or a Bearer JWT, with the JWT path
//! reserved for runner-internal use (commands invoked from the webview
//! always go through the originator-machine-id pathway, not the
//! operator pathway, which is correct for in-product UX).

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Snake-case ClaimKind discriminator on the wire — must match coord's
/// `serde(rename_all = "snake_case")` enum at `claims.rs:31-47`.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKindDto {
    AlembicRevision,
    BranchName,
    FileGlob,
    Phase,
    Worktree,
    CiWait,
}

/// Request body for `claims_acquire` (Tauri command) — wraps the
/// coord `ClaimRequest` shape but uses camelCase on the IPC boundary
/// (the existing Tauri-command convention in this codebase).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcquireArgs {
    pub kind: ClaimKindDto,
    pub resource_key: String,
    pub machine_id: String,
    /// Stable per-session UUID. Coord folds this into the claim's
    /// session-scoped owner token (`machine_id:agent_session_id`), so a
    /// caller that supplies it is a distinct holder from another session
    /// on the same machine. `None` → coord's legacy bare-machine behavior.
    /// Plan `2026-06-03-coord-session-scoped-claim-owner`. The same value
    /// MUST be sent on the matching release.
    #[serde(default)]
    pub agent_session_id: Option<String>,
    #[serde(default)]
    pub ttl_seconds: Option<i64>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Response payload for `claims_acquire` — passes coord's
/// `AcquireResult` through as opaque JSON so the frontend can pattern
/// match on the `result` discriminator. We keep this opaque because
/// the underlying enum has six variants and we'd rather not chase a
/// drift between Rust enum copies.
pub type AcquireResultDto = serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseArgs {
    pub kind: ClaimKindDto,
    pub resource_key: String,
    pub machine_id: String,
    /// Must match the `agent_session_id` sent at acquire — coord's
    /// release Lua compares the owner token, so a mismatched or omitted
    /// session yields `not_held` against a session-scoped claim. Plan
    /// `2026-06-03-coord-session-scoped-claim-owner`.
    #[serde(default)]
    pub agent_session_id: Option<String>,
}

pub type ReleaseResultDto = serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StealArgs {
    pub kind: ClaimKindDto,
    pub resource_key: String,
    pub machine_id: String,
    pub reason: String,
}

pub type StealResultDto = serde_json::Value;

/// Coord-HTTP-base resolver — mirrors the proven chain from
/// `mcp::agent_worktrees::coord_http_base`. Held here as a private
/// helper so this module is callable without taking the `ApiState`
/// dependency that lives behind axum (Tauri commands don't have
/// access to it without significantly more plumbing).
fn coord_http_base() -> String {
    if let Ok(v) = std::env::var("COORD_HTTP_URL") {
        if !v.is_empty() {
            return v;
        }
    }
    if let Some(ws) = qontinui_runner_lib::profiles::load().coord_url.as_deref() {
        return crate::agent_worktree::coord_ws_to_http(ws);
    }
    "http://localhost:9870".to_string()
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build http client: {e}"))
}

/// `claims_acquire` — wrapper around `POST /claims/acquire`. Returns
/// coord's `AcquireResult` as opaque JSON; the frontend matches on the
/// `result` discriminator (`claimed`/`held`/`renewed`/`topic_conflict`/
/// `topic_unknown`/`invalid_topic`). HTTP-level errors are returned as
/// `Err(String)` for the React layer's `.catch`.
#[tauri::command]
pub async fn claims_acquire(args: AcquireArgs) -> Result<AcquireResultDto, String> {
    let base = coord_http_base();
    let base = base.trim_end_matches('/');
    let url = format!("{base}/claims/acquire");

    let body = serde_json::json!({
        "kind": args.kind,
        "resource_key": args.resource_key,
        "machine_id": args.machine_id,
        "agent_session_id": args.agent_session_id,
        "ttl_seconds": args.ttl_seconds,
        "metadata": args.metadata,
    });

    let client = http_client()?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    // 200, 409 (topic conflict/unknown), 400 (invalid topic) all carry
    // a valid `AcquireResult` body — surface them. Anything else is an
    // unexpected error.
    let status = resp.status();
    let body_text = resp
        .text()
        .await
        .map_err(|e| format!("read /claims/acquire body: {e}"))?;
    if status == reqwest::StatusCode::OK
        || status == reqwest::StatusCode::CONFLICT
        || status == reqwest::StatusCode::BAD_REQUEST
    {
        serde_json::from_str::<serde_json::Value>(&body_text)
            .map_err(|e| format!("parse /claims/acquire body: {e} (raw: {body_text})"))
    } else {
        Err(format!(
            "POST /claims/acquire returned {} — body: {body_text}",
            status.as_u16()
        ))
    }
}

/// `claims_release` — wrapper around `POST /claims/release`. Idempotent:
/// returns `Released` on success, `NotHeld` if the claim was already
/// gone. Both are surfaced as 200; only transport-level errors yield
/// `Err`.
#[tauri::command]
pub async fn claims_release(args: ReleaseArgs) -> Result<ReleaseResultDto, String> {
    let base = coord_http_base();
    let base = base.trim_end_matches('/');
    let url = format!("{base}/claims/release");

    let body = serde_json::json!({
        "kind": args.kind,
        "resource_key": args.resource_key,
        "machine_id": args.machine_id,
        "agent_session_id": args.agent_session_id,
        // /claims/release requires the full ClaimRequest shape but only
        // consults `kind`/`resource_key`/`machine_id`/`agent_session_id`
        // (the owner-token match key). The rest are ignored.
        "metadata": serde_json::Value::Object(Default::default()),
    });

    let client = http_client()?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    let status = resp.status();
    let body_text = resp
        .text()
        .await
        .map_err(|e| format!("read /claims/release body: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "POST /claims/release returned {} — body: {body_text}",
            status.as_u16()
        ));
    }
    serde_json::from_str::<serde_json::Value>(&body_text)
        .map_err(|e| format!("parse /claims/release body: {e} (raw: {body_text})"))
}

/// `claims_steal` — wrapper around `POST /coord/claims/steal`. The
/// webview-initiated path uses the originator-machine-id JWT
/// authorization branch (per coord routes.rs:462-651 spec §1). Today
/// the runner doesn't mint a Bearer JWT for ad-hoc webview calls
/// (Phase 5 wires this in if needed); without a JWT, the operator
/// path is the only available branch and requires the admin secret
/// to be supplied via env at runner startup OR be presented in the
/// originator-from-this-machine case where coord's auth still passes
/// based on the body's `machine_id` matching the originator's row.
///
/// In practice for Phase 3: a runner stealing its own claim works
/// without a JWT because coord's StealAuthOutcome::Forbidden path
/// (routes.rs:636-650) maps to 401 only when no credential was
/// presented; the 403 path only fires when a JWT IS presented but
/// disagrees with the originator. Since we present no auth, callers
/// who aren't the originator will get 401 here — the modal renders
/// that as "you can't steal this claim."
///
/// Phase 5 will mint a self-signed token at runner startup for
/// originator-path steals so cross-machine UX works. For now the
/// 401 fallback is acceptable — the modal's "Wait" / "Abort" paths
/// remain functional.
#[tauri::command]
pub async fn claims_steal(args: StealArgs) -> Result<StealResultDto, String> {
    let base = coord_http_base();
    let base = base.trim_end_matches('/');
    let url = format!("{base}/coord/claims/steal");

    let body = serde_json::json!({
        "kind": args.kind,
        "resource_key": args.resource_key,
        "machine_id": args.machine_id,
        "reason": args.reason,
    });

    let client = http_client()?;
    let mut req = client.post(&url).json(&body);
    // If the admin secret is exposed via env on the runner host
    // (operator-managed runner deploys), forward it. Most production
    // runners won't have this set, in which case coord falls back to
    // the JWT-originator path and we hit 401 unless a JWT is presented.
    if let Ok(secret) = std::env::var("COORD_ADMIN_SECRET") {
        if !secret.is_empty() {
            req = req.header("X-Coord-Admin-Secret", secret);
        }
    }

    let resp = req.send().await.map_err(|e| format!("POST {url}: {e}"))?;
    let status = resp.status();
    let body_text = resp
        .text()
        .await
        .map_err(|e| format!("read /coord/claims/steal body: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "POST /coord/claims/steal returned {} — body: {body_text}",
            status.as_u16()
        ));
    }
    serde_json::from_str::<serde_json::Value>(&body_text)
        .map_err(|e| format!("parse /coord/claims/steal body: {e} (raw: {body_text})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_kind_dto_serializes_snake_case() {
        // Must match coord's `serde(rename_all = "snake_case")`.
        assert_eq!(
            serde_json::to_string(&ClaimKindDto::AlembicRevision).unwrap(),
            "\"alembic_revision\""
        );
        assert_eq!(
            serde_json::to_string(&ClaimKindDto::FileGlob).unwrap(),
            "\"file_glob\""
        );
        assert_eq!(
            serde_json::to_string(&ClaimKindDto::Phase).unwrap(),
            "\"phase\""
        );
        assert_eq!(
            serde_json::to_string(&ClaimKindDto::CiWait).unwrap(),
            "\"ci_wait\""
        );
        assert_eq!(
            serde_json::to_string(&ClaimKindDto::Worktree).unwrap(),
            "\"worktree\""
        );
        assert_eq!(
            serde_json::to_string(&ClaimKindDto::BranchName).unwrap(),
            "\"branch_name\""
        );
    }

    #[test]
    fn acquire_args_camelcase_resource_key() {
        // Validate that the IPC payload from the webview uses the
        // expected camelCase shape (`resourceKey`) and we hand it off
        // to coord as the snake_case shape (`resource_key`).
        let s = r#"{
            "kind": "phase",
            "resourceKey": "plan:p:phase:1",
            "machineId": "00000000-0000-0000-0000-000000000000",
            "metadata": {}
        }"#;
        let parsed: AcquireArgs = serde_json::from_str(s).unwrap();
        assert_eq!(parsed.resource_key, "plan:p:phase:1");
        assert_eq!(parsed.machine_id, "00000000-0000-0000-0000-000000000000");
        assert!(matches!(parsed.kind, ClaimKindDto::Phase));
    }
}
