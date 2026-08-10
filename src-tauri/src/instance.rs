//! Runner instance identity helpers.
//!
//! The supervisor sets `QONTINUI_INSTANCE_NAME` when spawning non-primary
//! runners (test runners, themed runners, etc.). This module centralizes the
//! detection and provides a path-segment helper so per-runner on-disk state
//! can be isolated without touching shared state (settings.json,
//! auth_tokens.enc, PostgreSQL).
//!
//! Primary runner: `data_subdir()` returns `None` — existing paths unchanged.
//! Secondary:      `data_subdir()` returns `Some("instance-<sanitized>")`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use tracing::{debug, error, info};

/// The raw instance name from the env, if set and non-empty.
pub fn instance_name() -> Option<String> {
    std::env::var("QONTINUI_INSTANCE_NAME")
        .ok()
        .filter(|s| !s.is_empty())
}

/// True when this runner was launched as a non-primary instance.
///
/// Note: this is a weaker check than `process_capture::primary_proxy::is_secondary`
/// — it only requires the instance name, not a primary port — because path
/// isolation should kick in even when the secondary has no primary to proxy to.
pub fn is_secondary() -> bool {
    instance_name().is_some()
}

/// True iff this runner is the canonical instance — the one allowed to own
/// SHARED, machine-wide state that is not path-isolated per instance.
///
/// The umbrella-root `.mcp.json` (`D:/qontinui-root/.mcp.json`) is exactly that
/// kind of state: every Claude session opened at the workspace root reads it,
/// and it is the sole path to coord-mcp (and therefore to the policy system) for
/// those sessions. A secondary that writes its own ephemeral port + nonce there
/// and then exits leaves the file naming a corpse, with nothing to heal it until
/// the primary next boots — which, for a protected primary, can be days.
///
/// **Deliberately `resolve_data_subdir`-based, not [`is_secondary`].**
/// `is_secondary()` keys on `QONTINUI_INSTANCE_NAME` alone, so a secondary the
/// supervisor spawned without that env var reads as PRIMARY and would be handed
/// the shared root config — the exact fail-open this guard exists to prevent.
/// `resolve_data_subdir` additionally detects a secondary by `primary_port` or
/// a non-default API port, so a nameless secondary fails CLOSED (quarantined,
/// hence `false` here). Same isolation boundary [`scope_path`] already enforces
/// for on-disk state, applied to the one shared file it never covered.
///
/// Unlike [`data_subdir`] this does NOT emit the nameless-secondary `error!` —
/// it is consulted on write-decision paths that can run many times per session,
/// and `data_subdir` already logs that supervisor bug loudly on the boot path.
pub fn owns_shared_root_state() -> bool {
    resolve_data_subdir(
        instance_name().as_deref(),
        primary_port(),
        crate::mcp::types::get_mcp_api_port(),
    )
    .is_none()
}

/// Classify this runner from the env it was launched with.
///
/// Asymmetry note: `QONTINUI_INSTANCE_NAME` is set for both temp and named
/// runners by the supervisor, so the runner alone cannot distinguish them.
/// This helper returns `RunnerKind::Named { name }` for any secondary; the
/// supervisor uses `RunnerKind::from_id` (with the runner id, not env) to
/// produce the precise variant. Callers in the runner that need the
/// secondary/primary split should still prefer `is_secondary()` for clarity.
pub fn runner_kind() -> qontinui_types::wire::runner_kind::RunnerKind {
    use qontinui_types::wire::runner_kind::RunnerKind;
    match instance_name() {
        Some(name) => RunnerKind::Named { name },
        None => RunnerKind::Primary,
    }
}

/// Pure decision core for [`data_subdir`] — every input injected, so it is
/// testable without touching process-global env (which races the parallel test
/// harness; `scheduler_service`'s tests mutate `QONTINUI_PORT` concurrently).
///
/// - `name`: `QONTINUI_INSTANCE_NAME`, the supervisor's explicit instance id.
/// - `primary_port`: `QONTINUI_PRIMARY_PORT` — set only on a runner that has a
///   primary to proxy to, i.e. never on the primary itself.
/// - `api_port`: this runner's API port. Only the primary owns
///   [`crate::mcp::types::MCP_API_PORT`].
///
/// Returns `None` — the unscoped, primary-owned path — ONLY for a runner that
/// presents no secondary signal at all.
fn resolve_data_subdir(
    name: Option<&str>,
    primary_port: Option<u16>,
    api_port: u16,
) -> Option<String> {
    if let Some(n) = name {
        return Some(format!("instance-{}", sanitize(n)));
    }
    if primary_port.is_some() || api_port != crate::mcp::types::MCP_API_PORT {
        // Secondary by another signal, but nameless — quarantine, never the
        // primary's path.
        return Some(format!("instance-unnamed-{api_port}"));
    }
    None
}

/// Returns the per-instance path segment, or `None` for the primary runner.
///
/// Primary:   `None`                            → callers leave paths alone
/// Secondary: `Some("instance-<sanitized>")`    → callers append to per-runner dirs
///
/// Fails CLOSED (see [`scope_path`]): a runner that is a secondary by any other
/// signal but carries no instance name is quarantined under
/// `instance-unnamed-<port>` rather than being handed the primary's `None`.
pub fn data_subdir() -> Option<String> {
    let name = instance_name();
    let primary = primary_port();
    let api_port = crate::mcp::types::get_mcp_api_port();
    let sub = resolve_data_subdir(name.as_deref(), primary, api_port);

    if name.is_none() && sub.is_some() {
        // Loud, not silent: this is a supervisor bug (it is contracted to set
        // QONTINUI_INSTANCE_NAME on every non-primary spawn) and the operator
        // needs to see it — but the runner still gets a usable, ISOLATED path.
        error!(
            port = api_port,
            primary_port = ?primary,
            subdir = ?sub,
            "instance: this runner is a SECONDARY (primary port set, or a non-default API port) \
             but QONTINUI_INSTANCE_NAME is unset — refusing primary-scoped state and quarantining \
             under an unnamed-instance dir. The supervisor must set QONTINUI_INSTANCE_NAME."
        );
    }
    sub
}

/// Append the instance subdir to `base` when this runner is a secondary.
/// Returns `base` unchanged for the primary runner.
///
/// **Fail-closed on the isolation boundary.** This used to key on
/// `QONTINUI_INSTANCE_NAME` alone, so a secondary launched without that env var
/// silently resolved to the PRIMARY's paths and could clobber the operator's
/// live pane layout / session outbox — the whole isolation depended on the
/// supervisor never forgetting the env, with no runner-side assertion. A
/// secondary detected by any other signal now gets a quarantined
/// `instance-unnamed-<port>` dir plus an `error!`, never the primary's.
///
/// Why quarantine rather than abort: the property that matters is "never open
/// primary-scoped state", and every caller here (logs, prompts, configs, the
/// pane store) is on the boot path — aborting would turn a supervisor env slip
/// into a dead runner, while quarantining keeps it working in isolation. The
/// primary itself (default port, no primary to proxy to) is untouched.
pub fn scope_path(base: &Path) -> PathBuf {
    match data_subdir() {
        Some(sub) => base.join(sub),
        None => base.to_path_buf(),
    }
}

/// The primary runner's port, if this is a secondary instance.
pub fn primary_port() -> Option<u16> {
    std::env::var("QONTINUI_PRIMARY_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
}

/// Resolve the WebView2 user-data folder for this runner.
///
/// Resolution order:
///
/// 1. `WEBVIEW2_USER_DATA_FOLDER` env var (set by qontinui-supervisor on the
///    spawn command). This is the supervisor-blessed canonical path.
/// 2. Fallback: derive from `RunnerKind` using
///    `qontinui_types::wire::webview2::webview2_data_dir`. This kicks in
///    when the runner is launched standalone (no supervisor) — typically
///    only the primary, but secondaries launched manually for debugging
///    work too.
///
/// Windows-only — returns `None` on every other platform (the env var is
/// also unused off-Windows; non-Windows webview backends ignore it).
///
/// The fallback uses `instance_name()` as the runner-id substitute because
/// the runner doesn't otherwise know its supervisor-assigned id. For named
/// secondaries that matches the on-disk folder layout exactly (the
/// supervisor's id for a named runner is `named-{port}-{uuid}`, which
/// equals the `QONTINUI_INSTANCE_NAME` env value). For temp runners
/// launched without a supervisor (an unusual case) we fall back to
/// `"primary"` because the runner has no id to differentiate itself —
/// callers should rely on the env var path in supervised setups.
#[cfg(target_os = "windows")]
pub fn webview2_data_dir() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var("WEBVIEW2_USER_DATA_FOLDER")
        .ok()
        .filter(|s| !s.is_empty())
    {
        return Some(std::path::PathBuf::from(p));
    }
    let kind = runner_kind();
    let id = instance_name().unwrap_or_else(|| "primary".into());
    qontinui_types::wire::webview2_data_dir(&kind, &id)
}

#[cfg(not(target_os = "windows"))]
pub fn webview2_data_dir() -> Option<std::path::PathBuf> {
    None
}

/// Register this secondary instance with the primary runner.
///
/// Called on startup when `QONTINUI_PRIMARY_PORT` is set. Best-effort:
/// failure is non-fatal (the runner works standalone). Returns the
/// registration ID on success.
pub async fn register_with_primary() -> Option<String> {
    let primary = primary_port()?;
    let own_name = instance_name()?;
    let own_port = crate::mcp::types::get_mcp_api_port();

    info!(
        "Registering with primary runner on port {} (name={}, port={})",
        primary, own_name, own_port
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;

    let url = format!("http://127.0.0.1:{}/instances/register", primary);
    let body = serde_json::json!({
        "name": own_name,
        "port": own_port,
        "pid": std::process::id(),
    });

    match client.post(&url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            let data: serde_json::Value = resp.json().await.ok()?;
            let id = data
                .get("data")
                .and_then(|d| d.get("id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            info!("Registered with primary (id={:?})", id);
            id
        }
        Ok(resp) => {
            debug!("Registration with primary failed: HTTP {}", resp.status());
            None
        }
        Err(e) => {
            debug!("Registration with primary failed: {}", e);
            None
        }
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::env_lock;

    #[test]
    fn sanitize_keeps_safe_chars() {
        assert_eq!(sanitize("test-runner_1"), "test-runner_1");
        assert_eq!(sanitize("abc/def"), "abc_def");
        assert_eq!(sanitize("weird name!"), "weird_name_");
    }

    /// The outbox base dir (`~/.qontinui/runner`) must scope per-instance for
    /// secondaries so spawn-test runners never race the primary on a single
    /// shared `session-outbox.jsonl`. This asserts the exact `scope_path`
    /// contract relied on by the outbox + pane-store wiring in `main.rs`.
    #[test]
    fn scope_path_isolates_outbox_dir_for_secondary() {
        let _env = env_lock();
        let base = Path::new(".qontinui").join("runner");

        // Secondary: appends `instance-<sanitized-name>`.
        std::env::set_var("QONTINUI_INSTANCE_NAME", "test-runner 7!");
        let scoped = scope_path(&base);
        assert_eq!(
            scoped,
            base.join("instance-test-runner_7_"),
            "secondary outbox dir must be instance-scoped"
        );
        let outbox = scoped.join("session-outbox.jsonl");
        assert!(outbox.to_string_lossy().contains("instance-test-runner_7_"));

        std::env::remove_var("QONTINUI_INSTANCE_NAME");
    }

    const PRIMARY: u16 = crate::mcp::types::MCP_API_PORT;

    /// The PRIMARY runner (no `QONTINUI_INSTANCE_NAME`, default port, no
    /// primary to proxy to) must keep resolving to the UNSCOPED legacy path so
    /// its pre-existing pending outbox rows are never orphaned.
    ///
    /// Asserted on the pure core rather than through the env: the fail-closed
    /// check reads `QONTINUI_PORT`, which `scheduler_service`'s tests mutate on
    /// other harness threads — an env-based assertion here would flake.
    #[test]
    fn primary_keeps_the_unscoped_path() {
        assert_eq!(resolve_data_subdir(None, None, PRIMARY), None);
    }

    /// Item 1 residual (fail-open on the isolation boundary): the isolation
    /// used to key on `QONTINUI_INSTANCE_NAME` alone, so a secondary spawned
    /// without it silently resolved to the PRIMARY's paths and could clobber
    /// the operator's live pane layout. A secondary detected by ANY other
    /// signal must refuse primary-scoped state.
    #[test]
    fn nameless_secondary_refuses_primary_scoped_state() {
        // Non-default API port ⇒ not the primary.
        assert_eq!(
            resolve_data_subdir(None, None, 9877),
            Some("instance-unnamed-9877".to_string()),
        );
        // Has a primary to proxy to ⇒ not the primary, even on the default
        // port (the belt-and-braces signal).
        assert_eq!(
            resolve_data_subdir(None, Some(PRIMARY), PRIMARY),
            Some("instance-unnamed-9876".to_string()),
        );
        // The property that actually matters: never the primary's own path.
        let base = Path::new(".qontinui").join("runner");
        for sub in [
            resolve_data_subdir(None, None, 9877),
            resolve_data_subdir(None, Some(PRIMARY), PRIMARY),
        ] {
            let scoped = base.join(sub.expect("a nameless secondary must be quarantined"));
            assert_ne!(scoped, base, "must not resolve to the primary's path");
        }
    }

    /// Regression (empty-Terminal-page incident): `window-assignments` used to
    /// be namespaced by API PORT. Temp-runner ports (9877-9899) are RECYCLED
    /// across spawns, so a fresh temp runner inherited the stale
    /// `window-assignments-9877.json` of an unrelated month-old runner —
    /// including a pop-out window bound to page "default". The frontend then
    /// hid that page from the main window (a pop-out "owns" it), so the
    /// Terminal tab rendered ZERO panes despite dozens of live PTYs, with no
    /// console error. Instance names are unique per spawn; the port is not an
    /// instance identity. This pins the wiring contract `main.rs` relies on.
    #[test]
    fn window_assignments_path_cannot_be_inherited_across_instances() {
        let base = Path::new(".qontinui").join("runner");
        let path_for = |name: &str, port: u16| {
            base.join(resolve_data_subdir(Some(name), None, port).unwrap())
                .join("window-assignments.json")
        };

        // Two runners that reuse the SAME recycled port must not share a file.
        let first = path_for("test-19f6faa3bf8-0", 9877);
        let recycled = path_for("test-19f6fd50c26-2", 9877);
        assert_ne!(
            first, recycled,
            "a recycled port must not resurrect a prior runner's pop-out windows"
        );

        // And neither may ever resolve to the primary's own file.
        let primary = base.join("window-assignments.json");
        assert_ne!(first, primary);
        assert_ne!(recycled, primary);
        assert_eq!(
            resolve_data_subdir(None, None, PRIMARY),
            None,
            "the primary keeps the legacy unscoped window-assignments path"
        );
    }

    /// Sibling to `window_assignments_path_cannot_be_inherited_across_instances`
    /// for the terminal-session lifecycle store + snapshot history (plan
    /// `2026-07-20-runner-port-keyed-state-inheritance`). The store used to be
    /// namespaced by API PORT; recycled temp-runner ports (9877-9899) let a
    /// fresh runner inherit a prior occupant's `terminal-sessions-<port>.json`
    /// (observed: 164 stale Jul-13 records → 81 phantom open rows → 27 foreign
    /// `claude --resume`s). Instance names are unique per spawn and durable per
    /// named runner, so scoping by instance identity makes that inheritance
    /// impossible. Pins the wiring contract
    /// `session_lifecycle_store::{store_path, snapshot_history_path}` rely on:
    /// both compose `scope_path(runner_dir).join(<plain filename>)`, so this
    /// pure `resolve_data_subdir` assertion covers their path composition
    /// without touching process-global env (which races the parallel harness).
    #[test]
    fn lifecycle_store_and_snapshot_paths_cannot_be_inherited_across_instances() {
        let base = Path::new(".qontinui").join("runner");
        let store_for = |name: &str, port: u16| {
            base.join(resolve_data_subdir(Some(name), None, port).unwrap())
                .join("terminal-sessions.json")
        };
        let snapshot_for = |name: &str, port: u16| {
            base.join(resolve_data_subdir(Some(name), None, port).unwrap())
                .join("session-restore")
                .join("session-snapshots.jsonl")
        };

        // (a) Two runners reusing the SAME recycled port 9877 with distinct
        // instance names must resolve to distinct store + snapshot files — no
        // recycled-port inheritance.
        assert_ne!(
            store_for("test-19f6faa3bf8-0", 9877),
            store_for("test-19f6fd50c26-2", 9877),
            "a recycled port must not resurrect a prior runner's session records"
        );
        assert_ne!(
            snapshot_for("test-19f6faa3bf8-0", 9877),
            snapshot_for("test-19f6fd50c26-2", 9877),
            "a recycled port must not resurrect a prior runner's snapshot history"
        );

        // (b) Neither secondary may ever resolve to the PRIMARY's own files —
        // this is the property that stops a secondary reading the primary's
        // store (the pre-existing latent bug the plain-unscoped call sites had).
        let primary_store = base.join("terminal-sessions.json");
        let primary_snapshot = base.join("session-restore").join("session-snapshots.jsonl");
        assert_ne!(store_for("test-19f6faa3bf8-0", 9877), primary_store);
        assert_ne!(store_for("test-19f6fd50c26-2", 9877), primary_store);
        assert_ne!(snapshot_for("test-19f6faa3bf8-0", 9877), primary_snapshot);

        // (c) The primary keeps the legacy UNSCOPED lifecycle/snapshot path
        // (`data_subdir() == None` ⇒ `scope_path` returns the base unchanged),
        // so its crash-recovery reattach is byte-for-byte preserved.
        assert_eq!(resolve_data_subdir(None, None, PRIMARY), None);
    }

    /// Sibling to `lifecycle_store_and_snapshot_paths_cannot_be_inherited_across_instances`
    /// for the clean-vs-crash SHUTDOWN MARKER (plan
    /// `2026-08-10-temp-runner-session-restore-isolation`, mechanism 3). The
    /// marker was the one piece of restore state the 2026-07-20 instance-scoping
    /// never touched: it stayed keyed by API PORT (`last-shutdown.json` for
    /// 9876, `last-shutdown-<port>.json` otherwise). Temp-runner ports
    /// (9877-9899) are RECYCLED, so a fresh temp runner read the PRIOR
    /// occupant's marker — and that marker is not cosmetic: it feeds
    /// `shutdown_marker::boot_classification()`, whose `prior_marker_at` is
    /// `terminal_session_list_open`'s cohort anchor and whose `crash_recovery`
    /// becomes `boot_was_clean`. A foreign runner's "last moment of life"
    /// therefore decided which of THIS runner's records counted as restorable.
    ///
    /// Two arms, deliberately:
    /// (a) the pure path-composition contract, asserted through
    ///     `resolve_data_subdir` rather than the env (`QONTINUI_PORT` is mutated
    ///     concurrently by `scheduler_service`'s tests, so an env-based port
    ///     assertion here would flake); and
    /// (b) that production's `marker_path()` actually routes through that
    ///     scoping. Arm (b) sets only `QONTINUI_INSTANCE_NAME`, which
    ///     `resolve_data_subdir` answers on its FIRST branch without consulting
    ///     the port at all — the same env-safe shape
    ///     `scope_path_isolates_outbox_dir_for_secondary` already uses.
    #[test]
    fn shutdown_marker_path_cannot_be_inherited_across_instances() {
        // (a) Pure composition: two runners reusing the SAME recycled port must
        // resolve to distinct markers, and the primary keeps the unscoped one.
        let base = Path::new(".qontinui").join("runner");
        let marker_for = |name: &str, port: u16| {
            base.join(resolve_data_subdir(Some(name), None, port).unwrap())
                .join("last-shutdown.json")
        };
        assert_ne!(
            marker_for("test-19f6faa3bf8-0", 9877),
            marker_for("test-19f6fd50c26-2", 9877),
            "a recycled port must not hand a fresh runner a prior occupant's boot classification"
        );
        let primary_marker = base.join("last-shutdown.json");
        assert_ne!(marker_for("test-19f6faa3bf8-0", 9877), primary_marker);
        assert_ne!(marker_for("test-19f6fd50c26-2", 9877), primary_marker);
        assert_eq!(
            resolve_data_subdir(None, None, PRIMARY),
            None,
            "the primary keeps the legacy UNSCOPED last-shutdown.json"
        );

        // (b) The production resolver honours it.
        let _env = env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&["QONTINUI_INSTANCE_NAME"]);
        use crate::session::shutdown_marker::marker_path;

        std::env::set_var("QONTINUI_INSTANCE_NAME", "test-19f6faa3bf8-0");
        let first = marker_path();
        std::env::set_var("QONTINUI_INSTANCE_NAME", "test-19f6fd50c26-2");
        let recycled = marker_path();

        assert_ne!(
            first, recycled,
            "marker_path must be keyed on the per-spawn instance identity, not the recycled port"
        );
        for (name, path) in [
            ("test-19f6faa3bf8-0", &first),
            ("test-19f6fd50c26-2", &recycled),
        ] {
            let shown = path.to_string_lossy().replace('\\', "/");
            assert!(
                shown.ends_with(&format!("instance-{name}/last-shutdown.json")),
                "expected an instance-scoped marker for {name}, got {shown}"
            );
        }
    }

    /// An explicit instance name always wins — the normal supervised path is
    /// unchanged by the fail-closed guard, on any port.
    #[test]
    fn instance_name_wins_over_the_fallback() {
        assert_eq!(
            resolve_data_subdir(Some("test-runner 7!"), Some(PRIMARY), 9877),
            Some("instance-test-runner_7_".to_string()),
        );
        assert_eq!(
            resolve_data_subdir(Some("test-9877"), None, PRIMARY),
            Some("instance-test-9877".to_string()),
        );
    }
}
