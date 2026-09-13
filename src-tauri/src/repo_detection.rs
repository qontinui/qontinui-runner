//! Tauri command surface over [`qontinui_runner_lib::repo_tenant`].
//!
//! The lookup itself, its caches and its wire parsing live in the LIB crate,
//! because its principal consumer — the plan → work-unit adapter — is a lib
//! module and cannot reach the binary's tree. This file keeps the parts that
//! genuinely belong to the binary: the two `#[tauri::command]`s the frontend
//! calls, the unregistered-repo event emit, and the binary-side scope
//! conversion below.

use std::path::Path;

use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;
use serde_json::json;
use tauri::Emitter;
use tracing::warn;

use qontinui_runner_lib::repo_tenant;

pub use qontinui_runner_lib::repo_tenant::is_repo_registered;

/// Re-badge the LIB crate's [`TenantScope`] as the BINARY's.
///
/// `src/auth.rs` is compiled into both crates (`lib.rs` `pub mod auth`,
/// `main.rs` `mod auth`), so the two `TenantScope`s are structurally identical
/// and nominally distinct — the same documented duplication that makes
/// `auth`'s data-plane counters per-crate. The binary's coord writers attach
/// through `crate::auth`, so a lib-resolved scope has to cross the boundary
/// exactly once, here, rather than at every call site.
///
/// Exhaustive on purpose: a new variant must be considered here, not silently
/// folded into an existing one — collapsing variants is the defect
/// `TenantScope` exists to prevent.
fn rebadge(scope: qontinui_runner_lib::auth::TenantScope) -> crate::auth::TenantScope {
    match scope {
        qontinui_runner_lib::auth::TenantScope::Owned(t) => crate::auth::TenantScope::Owned(t),
        qontinui_runner_lib::auth::TenantScope::Device => crate::auth::TenantScope::Device,
        qontinui_runner_lib::auth::TenantScope::Unresolved => crate::auth::TenantScope::Unresolved,
    }
}

/// Binary-side [`repo_tenant::tenant_scope_for_path`]: the tenant that owns
/// whatever repo `path` lives in, as a scope the binary's `crate::auth` seam
/// accepts.
pub async fn tenant_scope_for_path(path: &Path) -> crate::auth::TenantScope {
    rebadge(repo_tenant::tenant_scope_for_path(path).await)
}

/// F2 — repo→tenant inference for the spawn picker's DEFAULT selection.
///
/// Accepts either an explicit `owner/name` slug or a `working_dir` to derive
/// one from (`git remote get-url origin`), so the caller can pass the active
/// tab's cwd without re-implementing slug parsing in TypeScript. `repo` wins
/// when both are supplied.
///
/// Returns the tenant id coord associates with the repo, or `None` when the
/// repo is unknown / not tenant-scoped / coord is unreachable. The frontend
/// treats `None` as "keep the active pin" and shows no error: inference is a
/// smart default, never a hard lock, so an unreachable coord must degrade
/// silently rather than block a spawn.
///
/// **Phase 6:** the lookup now lives in
/// [`repo_tenant::tenant_scope_for_repo_slug`], which returns a `TenantScope`
/// and so keeps "coord said no tenant" apart from "coord did not answer".
/// This command deliberately collapses both back to `None` — that IS the right
/// answer for a spawn-picker default — and is a thin wrapper so the two can
/// never drift.
#[tauri::command]
pub async fn tenant_for_repo(
    repo: Option<String>,
    working_dir: Option<String>,
) -> Result<Option<String>, String> {
    let explicit = repo
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);

    let slug = match explicit {
        Some(s) => Some(s),
        None => match working_dir.filter(|d| !d.trim().is_empty()) {
            // `git remote get-url` shells out — keep it off the async runtime.
            Some(dir) => spawn_blocking_tracked(move || repo_tenant::detect_repo_slug(&dir))
                .await
                .ok()
                .flatten(),
            None => None,
        },
    };

    let slug = match slug {
        Some(s) => s,
        None => return Ok(None),
    };

    Ok(repo_tenant::tenant_scope_for_repo_slug(&slug)
        .await
        .declared_tenant()
        .map(|t| t.to_string()))
}

pub async fn check_and_emit_unregistered(
    app_handle: tauri::AppHandle,
    working_dir: Option<String>,
) {
    let dir = match working_dir {
        Some(d) if !d.is_empty() => d,
        _ => return,
    };

    let slug = match spawn_blocking_tracked(move || repo_tenant::detect_repo_slug(&dir)).await {
        Ok(Some(s)) => s,
        _ => return,
    };

    if is_repo_registered(&slug).await {
        return;
    }

    let payload = json!({ "repo": slug });
    if let Err(e) = app_handle.emit("repo-not-registered", &payload) {
        warn!("repo_detection: emit repo-not-registered failed: {e}");
    }
}

#[tauri::command]
pub async fn register_repo_with_coord(repo: String) -> Result<serde_json::Value, String> {
    let (base, _coord_base_source) = qontinui_runner_lib::profiles::coord_base_with_source();
    let base = base.trim_end_matches('/');
    let url = format!("{base}/coord/canonical-repos");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;

    // Phase 6 — the one work-scoped site where the DEVICE's declared default is
    // the right tenant, and the only one that does not derive from the repo.
    //
    // Deriving here would be circular: a repo is registered precisely because
    // coord does not know it, so a repo→tenant lookup necessarily answers
    // `Unresolved`, which on a multi-bound device degrades to unauthenticated —
    // leaving coord with no principal and the row with `tenant_id = NULL`,
    // which is exactly the state Phase 1 measured on all five live rows.
    //
    // What this call actually asserts is "the operator, working as the tenant
    // this box is currently pinned to, claims this repo". `machine.json`'s
    // `active_tenant_id` IS that assertion — written by the
    // `commands/tenant.rs` command, from the same picker that invokes THIS
    // command — so it is a declaration, not the machine-global inference D3
    // rejects for artifact-owned rows. A machine naming no default is
    // `Unpinned`, not broken, hence `for_device_default`.
    //
    // Coord derives `canonical_repos.tenant_id` from the verified principal, so
    // presenting the right bearer is the whole fix; the body needs no tenant
    // field and deliberately keeps none.
    let scope = crate::auth::TenantScope::for_device_default(
        crate::session::dual_write::resolve_active_tenant_id(),
    );
    let resp = crate::auth::attach_device_auth_for(
        client.post(&url).json(&json!({ "repo": repo })),
        scope,
    )
    .send()
    .await
    .map_err(|e| format!("POST {url}: {e}"))?;

    let status = resp.status();
    let body_text = resp
        .text()
        .await
        .map_err(|e| format!("read register body: {e}"))?;

    if !status.is_success() {
        return Err(format!(
            "POST /coord/canonical-repos returned {} — body: {body_text}",
            status.as_u16()
        ));
    }

    // Invalidate the cache so the next check sees the new registration.
    repo_tenant::invalidate_canonical_repos().await;

    serde_json::from_str::<serde_json::Value>(&body_text)
        .map_err(|e| format!("parse register body: {e} (raw: {body_text})"))
}
