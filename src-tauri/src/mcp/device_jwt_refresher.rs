//! Background task that keeps the device-JWT fresh (Phase 2 of the
//! runner unified-devices migration).
//!
//! Coord mints 4-hour device-JWTs. To avoid the user being signed out
//! while the runner is idle, this loop checks the stored JWT's `exp`
//! every [`REFRESH_CHECK_INTERVAL`] and re-pairs (via
//! `qontinui_runner_lib::pair::pair_with_auth_token`) once we're within
//! TTL/3 of expiry. The new JWT is persisted to the same encrypted
//! `auth_tokens.enc` slot the backend relay reads, and the relay is
//! kicked so it reconnects with the fresh credential.
//!
//! ## Lifecycle parallel to backend_relay
//!
//! The refresher mirrors `mcp::backend_relay`'s shape:
//!
//! - `RefresherState { shutdown_tx, kick_tx, task_handle }` — same
//!   `tokio::sync::watch` channels for shutdown + kick.
//! - `auto_start_device_jwt_refresher(api_state)` — idempotent start;
//!   re-kicks instead of spawning a duplicate if a live task already
//!   exists.
//! - `commands::kick_device_jwt_refresher()` — public API consumed from
//!   `commands::auth::set_runner_tier` (and any future code that needs
//!   to wake the refresher; e.g. apply_web_integration_settings).
//!
//! ## Why is the decision predicate factored out?
//!
//! The async loop body is wrapped around blocking-thread spawns, watch
//! channels, and tracing — none of which fit a pure-function unit test.
//! [`next_action`] is the inner predicate (tier + token + needs-refresh
//! → [`Decision`]) so the spec-check tests can exercise the branching
//! without spinning a tokio runtime.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{watch, Mutex};
use tracing::{info, warn};

use crate::mcp::types::ApiState;
use crate::settings::{self, RunnerTier};

/// Outcome of a single refresh attempt. Tests assert on this variant
/// directly; the runtime loop in [`refresher_loop`] consumes it via
/// pattern-match (Replaced → kick the relay; KeptExisting/PersistFailed →
/// log + back off).
///
/// CRITICAL INVARIANT (Phase 5.2): a non-2xx coord response MUST map to
/// [`RefreshOutcome::KeptExisting`], NEVER to a code path that clears the
/// JWT. A 401 from coord means "this runner_token is stale"; if we
/// cleared the access_token slot in response, the relay would lose its
/// valid (just-not-yet-expired) device-JWT and the next user-flow would
/// be forced into a fresh browser-pair. The refresher's job is to
/// REPLACE, not REVOKE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RefreshOutcome {
    /// Coord returned 2xx with a fresh JWT, and we persisted it to the
    /// access_token slot. Carries the new JWT so the runtime loop can
    /// log its jti/exp without re-reading from disk.
    Replaced { new_jwt: String },
    /// Coord returned a non-2xx (401, 503, anything else), OR the
    /// network call failed, OR the spawn_blocking handle joined with
    /// an error. The existing JWT in the access_token slot is left
    /// untouched.
    KeptExisting,
    /// Coord returned a fresh JWT but persistence to AuthManager
    /// failed. The existing JWT in the access_token slot is left
    /// untouched (store_tokens is atomic; a failure aborts before
    /// rewriting the slot).
    PersistFailed(String),
}

/// How often the loop wakes to check whether the JWT is approaching
/// expiry. 5 minutes is plenty given the refresh threshold is 80 min.
const REFRESH_CHECK_INTERVAL: Duration = Duration::from_secs(300);

/// What action the loop should take this iteration. Factored out of
/// [`refresher_loop`] so unit tests can exercise the branching without
/// running the async loop body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    /// JWT is fresh enough — sleep until next check or kick.
    Idle,
    /// Tier is not `QontinuiAccount` — refresher has nothing to do.
    /// Wait for a tier-change kick before re-checking.
    IdleWrongTier,
    /// JWT needs refresh but no `runner_token` is available to present
    /// to coord. Wait for a settings-change kick before re-checking.
    IdleNoToken,
    /// JWT needs refresh and a bearer token is available — call
    /// `pair_with_auth_token`.
    Pair,
}

/// Pure decision predicate: given the current tier + bearer token +
/// "does the JWT need refresh?" answer, what should the loop do?
pub(crate) fn next_action(tier: RunnerTier, runner_token: &str, needs_refresh: bool) -> Decision {
    if tier != RunnerTier::QontinuiAccount {
        return Decision::IdleWrongTier;
    }
    if !needs_refresh {
        return Decision::Idle;
    }
    if runner_token.trim().is_empty() {
        return Decision::IdleNoToken;
    }
    Decision::Pair
}

/// State for the device-JWT refresher task. Owns the watch channels for
/// shutdown + kick and the join handle so callers can stop / re-kick.
pub struct RefresherState {
    shutdown_tx: watch::Sender<bool>,
    kick_tx: watch::Sender<u64>,
    task_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl RefresherState {
    /// Stop the refresher task, giving it a chance to shut down
    /// gracefully (up to 3 seconds before we drop the handle).
    pub async fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(handle) = self.task_handle.lock().await.take() {
            match tokio::time::timeout(Duration::from_secs(3), handle).await {
                Ok(_) => info!("Device-JWT refresher stopped gracefully"),
                Err(_) => warn!(
                    "Device-JWT refresher did not stop in 3s; shutdown signal sent, moving on"
                ),
            }
        }
    }

    /// Kick the refresher: interrupt any in-progress sleep so the next
    /// iteration runs immediately, re-reading settings + tokens.
    pub fn kick(&self) {
        let current = *self.kick_tx.borrow();
        let _ = self.kick_tx.send(current.wrapping_add(1));
    }
}

/// Spawn the refresher task. Returns the state handle so the caller can
/// stop / kick it. Used by `auto_start_device_jwt_refresher`.
pub fn start_refresher(api_state: Arc<ApiState>) -> Arc<RefresherState> {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (kick_tx, kick_rx) = watch::channel(0u64);

    let task_handle = tokio::spawn(async move {
        refresher_loop(api_state, shutdown_rx, kick_rx).await;
    });

    Arc::new(RefresherState {
        shutdown_tx,
        kick_tx,
        task_handle: Mutex::new(Some(task_handle)),
    })
}

/// Attempt one refresh against `pair_base` using `runner_token` as the
/// bearer + `device_id` / `user_id` as the wire body / header fields.
/// Factored out of the inline Pair-arm body so the Phase 5.2 tests can
/// drive it directly against an in-process mock backend.
///
/// `pair_base` is the web-backend URL (e.g. `http://127.0.0.1:8000`);
/// the underlying [`pair_with_auth_token_with_ids`] hits
/// `{pair_base}/api/v1/devices/pair-cli`, which the backend proxies to
/// coord with `tenant_id` resolved from the authenticated user.
///
/// Invariant: a non-2xx HTTP response, a network error, or a
/// spawn_blocking join failure all collapse to
/// [`RefreshOutcome::KeptExisting`]. The caller MUST NOT clear the JWT
/// in response to that variant — see the doc-comment on
/// [`RefreshOutcome`].
pub(crate) async fn try_refresh_once(
    auth_manager: &crate::auth::AuthManager,
    pair_base: &str,
    runner_token: &str,
    device_id: &str,
    user_id: &str,
) -> RefreshOutcome {
    let base = pair_base.to_string();
    let token = runner_token.to_string();
    let did = device_id.to_string();
    let uid = user_id.to_string();

    // pair_with_auth_token_with_ids is reqwest::blocking — must run via
    // spawn_blocking or it stalls the tokio runtime.
    let pair_join = tokio::task::spawn_blocking(move || {
        qontinui_runner_lib::pair::pair_with_auth_token_with_ids(&base, &token, &did, &uid)
    })
    .await;

    let pair_result = match pair_join {
        Ok(inner) => inner,
        Err(join_err) => {
            warn!("device_jwt_refresher: pair task join failed: {join_err}");
            return RefreshOutcome::KeptExisting;
        }
    };

    let resp = match pair_result {
        Ok(r) => r,
        Err(e) => {
            // Non-2xx, network error, decode error — ALL leave the JWT
            // slot alone. The relay keeps presenting the existing (not-
            // yet-expired) JWT until coord's exp ticks past or the
            // operator pairs again.
            warn!("device_jwt_refresher: pair_with_auth_token_with_ids failed: {e}");
            return RefreshOutcome::KeptExisting;
        }
    };

    // Coord returned 2xx → persist the new JWT into the access_token
    // slot. The refresh-token slot stays empty (device-JWT lifecycle is
    // owned by coord, not by an OAuth refresh chain).
    match auth_manager.store_tokens(&resp.token, "") {
        Ok(()) => RefreshOutcome::Replaced {
            new_jwt: resp.token,
        },
        Err(e) => {
            warn!("device_jwt_refresher: persist new JWT failed: {e}");
            RefreshOutcome::PersistFailed(e.to_string())
        }
    }
}

async fn refresher_loop(
    _api_state: Arc<ApiState>,
    mut shutdown_rx: watch::Receiver<bool>,
    mut kick_rx: watch::Receiver<u64>,
) {
    let auth_manager = crate::auth::AuthManager::new();
    info!("Device-JWT refresher started (check interval = 5m, threshold = 80m)");

    loop {
        if *shutdown_rx.borrow() {
            info!("Device-JWT refresher shutting down");
            return;
        }

        // Snapshot settings + needs-refresh decision once per iteration.
        let settings_snapshot = settings::load_settings();
        let needs_refresh_result = auth_manager.device_jwt_needs_refresh();
        let needs_refresh = match needs_refresh_result {
            Ok(b) => b,
            Err(e) => {
                warn!("device_jwt_refresher: device_jwt_needs_refresh failed: {e}");
                // Sleep before retrying to avoid a hot-loop on persistent error.
                if wait_with_signals(REFRESH_CHECK_INTERVAL, &mut shutdown_rx, &mut kick_rx).await {
                    return;
                }
                continue;
            }
        };

        let decision = next_action(
            settings_snapshot.tier,
            settings_snapshot.web_integration.runner_token.trim(),
            needs_refresh,
        );

        match decision {
            Decision::IdleWrongTier => {
                // Nothing to do until tier changes. Block on shutdown or
                // kick (set_runner_tier kicks us on every transition).
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        info!("Device-JWT refresher shutting down (was idle on non-Tier2)");
                        return;
                    }
                    _ = kick_rx.changed() => continue,
                }
            }
            Decision::IdleNoToken => {
                // Need to refresh but have no bearer to present. This is
                // the "first-pair" state — the user must complete a
                // browser pair before we can refresh. Block on
                // shutdown/kick to avoid a hot-loop.
                tracing::debug!(
                    "device_jwt_refresher: needs refresh but runner_token is empty — \
                     awaiting first browser-pair (kick on settings save)"
                );
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        info!("Device-JWT refresher shutting down (was idle on no-token)");
                        return;
                    }
                    _ = kick_rx.changed() => continue,
                }
            }
            Decision::Idle => {
                // JWT is fresh — sleep until next check or wake on kick.
                if wait_with_signals(REFRESH_CHECK_INTERVAL, &mut shutdown_rx, &mut kick_rx).await {
                    return;
                }
                continue;
            }
            Decision::Pair => {
                // Post-2026-05-22: pair-cli now goes through the web
                // backend (`/api/v1/devices/pair-cli`), not coord directly,
                // so the backend can resolve `tenant_id` from the
                // authenticated user. The backend gates on the FastAPI
                // user-JWT (`Authorization: Bearer <access_token>`) — not
                // the legacy `runner_token`, which only coord ever
                // accepted. Pull the user JWT that the runner stashed at
                // sign-in time (`commands::auth::login` →
                // `AuthManager::store_tokens`).
                let bearer_token = match auth_manager.get_access_token() {
                    Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
                    _ => {
                        warn!(
                            "device_jwt_refresher: access_token slot empty — user must \
                             sign in to Qontinui before the refresher can pair"
                        );
                        if wait_with_signals(REFRESH_CHECK_INTERVAL, &mut shutdown_rx, &mut kick_rx)
                            .await
                        {
                            return;
                        }
                        continue;
                    }
                };
                let pair_base = settings_snapshot
                    .web_integration
                    .backend_url
                    .trim()
                    .trim_end_matches('/')
                    .to_string();
                if pair_base.is_empty() {
                    warn!("device_jwt_refresher: backend_url empty — cannot pair");
                    if wait_with_signals(REFRESH_CHECK_INTERVAL, &mut shutdown_rx, &mut kick_rx)
                        .await
                    {
                        return;
                    }
                    continue;
                }

                // Resolve device_id + user_id from disk. Phase 5 split:
                // these reads live here (operator-facing files) so the
                // factored `try_refresh_once` stays hermetic + testable.
                let device_id = match qontinui_runner_lib::pair::read_device_id_from_disk() {
                    Ok(d) => d,
                    Err(e) => {
                        warn!(
                            "device_jwt_refresher: machine.json unreadable: {e} \
                             — run `qontinui_profile device init` to create it"
                        );
                        if wait_with_signals(REFRESH_CHECK_INTERVAL, &mut shutdown_rx, &mut kick_rx)
                            .await
                        {
                            return;
                        }
                        continue;
                    }
                };
                let user_id = match qontinui_runner_lib::pair::read_paired_user_id_from_disk() {
                    Some(u) => u,
                    None => {
                        // Should not happen — `Decision::IdleNoToken`
                        // arm already gates on runner_token presence,
                        // and a paired install always has both. Log + back off.
                        warn!(
                            "device_jwt_refresher: paired_user.json missing — \
                             needs first browser-pair (refresher idling until kick)"
                        );
                        if wait_with_signals(REFRESH_CHECK_INTERVAL, &mut shutdown_rx, &mut kick_rx)
                            .await
                        {
                            return;
                        }
                        continue;
                    }
                };

                // Phase 5.2: try_refresh_once encapsulates the
                // pair-cli HTTP call + JWT persistence. It preserves
                // the existing JWT on any non-2xx outcome.
                let outcome = try_refresh_once(
                    &auth_manager,
                    &pair_base,
                    &bearer_token,
                    &device_id,
                    &user_id,
                )
                .await;
                match outcome {
                    RefreshOutcome::Replaced { new_jwt } => {
                        info!(
                            "device_jwt_refresher: device-JWT refreshed (len={})",
                            new_jwt.len()
                        );
                        // Wake the relay so it reconnects with the new JWT.
                        crate::mcp::backend_relay::commands::kick_cloud_relay().await;
                    }
                    RefreshOutcome::KeptExisting => {
                        // Coord error (401 / 503 / etc.) or transport failure —
                        // existing JWT preserved. Will retry next tick.
                    }
                    RefreshOutcome::PersistFailed(e) => {
                        warn!("device_jwt_refresher: persist new JWT failed: {e}");
                    }
                }

                // Brief sleep before next iteration (success or failure)
                // so we don't hammer coord on persistent errors.
                if wait_with_signals(REFRESH_CHECK_INTERVAL, &mut shutdown_rx, &mut kick_rx).await {
                    return;
                }
                continue;
            }
        }
    }
}

/// Sleep `dur`, but wake early on shutdown or kick. Returns `true` iff
/// the loop should `return` (shutdown received).
async fn wait_with_signals(
    dur: Duration,
    shutdown_rx: &mut watch::Receiver<bool>,
    kick_rx: &mut watch::Receiver<u64>,
) -> bool {
    tokio::select! {
        _ = shutdown_rx.changed() => true,
        _ = kick_rx.changed() => false,
        _ = tokio::time::sleep(dur) => false,
    }
}

/// Global state holder + public surface. Same shape as
/// `backend_relay::commands` so the call sites read consistently.
pub mod commands {
    use super::*;
    use std::sync::OnceLock;

    static REFRESHER_STATE: OnceLock<tokio::sync::Mutex<Option<Arc<RefresherState>>>> =
        OnceLock::new();

    fn get_holder() -> &'static tokio::sync::Mutex<Option<Arc<RefresherState>>> {
        REFRESHER_STATE.get_or_init(|| tokio::sync::Mutex::new(None))
    }

    /// Idempotent start. If a live task already exists, kick it instead
    /// of spawning a duplicate. Called from `mcp_api::start_server` once
    /// `Arc<ApiState>` is available.
    pub async fn auto_start_device_jwt_refresher(api_state: Arc<ApiState>) {
        let mut guard = get_holder().lock().await;

        if let Some(ref existing) = *guard {
            let handle_guard = existing.task_handle.lock().await;
            let is_alive = handle_guard.as_ref().is_some_and(|h| !h.is_finished());
            drop(handle_guard);
            if is_alive {
                info!("Device-JWT refresher already running; kicking to re-read state");
                existing.kick();
                return;
            }
            info!("Device-JWT refresher task has ended, restarting...");
            existing.stop().await;
            *guard = None;
        }

        info!("Starting device-JWT refresher");
        let state = start_refresher(api_state);
        *guard = Some(state);
    }

    /// Kick the refresher: interrupt any in-progress sleep so the next
    /// iteration runs immediately. Used by `set_runner_tier` on
    /// transition into Tier 2, and by any future code that updates the
    /// runner_token or coord_url.
    pub async fn kick_device_jwt_refresher() {
        let guard = get_holder().lock().await;
        if let Some(ref state) = *guard {
            state.kick();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decides_no_refresh_when_jwt_fresh() {
        // Tier 2 + token present + needs_refresh=false → Idle (no work).
        let d = next_action(RunnerTier::QontinuiAccount, "some-runner-token", false);
        assert_eq!(d, Decision::Idle);
    }

    #[test]
    fn decides_pair_when_jwt_stale_and_token_present() {
        let d = next_action(RunnerTier::QontinuiAccount, "some-runner-token", true);
        assert_eq!(d, Decision::Pair);
    }

    #[test]
    fn decides_idle_when_runner_token_empty() {
        let d = next_action(RunnerTier::QontinuiAccount, "", true);
        assert_eq!(d, Decision::IdleNoToken);
        // Whitespace-only should still count as empty.
        let d2 = next_action(RunnerTier::QontinuiAccount, "   ", true);
        assert_eq!(d2, Decision::IdleNoToken);
    }

    #[test]
    fn decides_idle_when_tier_not_qontinui_account() {
        // LocalProvider: not Tier 2 — refresher idles regardless of
        // whether the JWT is stale or a token is present.
        let d = next_action(RunnerTier::LocalProvider, "some-runner-token", true);
        assert_eq!(d, Decision::IdleWrongTier);
        let d2 = next_action(RunnerTier::Local, "some-runner-token", true);
        assert_eq!(d2, Decision::IdleWrongTier);
        // Even with no needs-refresh signal, wrong-tier still wins.
        let d3 = next_action(RunnerTier::LocalProvider, "", false);
        assert_eq!(d3, Decision::IdleWrongTier);
    }

    #[test]
    fn refresh_check_interval_is_five_minutes() {
        // Pin the constant so a future refactor that "tunes" it has to
        // update this test (and explain why in review).
        assert_eq!(REFRESH_CHECK_INTERVAL, Duration::from_secs(300));
    }
}

// ============================================================================
// PHASE 5 DEFERRED — live-stack scenarios outside this PR's reach
// ============================================================================
//
// PHASE 5 DEFERRED: the following acceptance-criteria scenarios are NOT
// exercised by the in-process tests above. They require a live web +
// live coord stack (or coord-side state coordination that the runner has
// no hooks into), and are out of scope for the calibrated Phase 5 PR.
// They MUST be exercised manually before the unified-devices migration
// is tagged as fully shipped:
//
//   1. Fresh-pair browser flow E2E. Operator opens the runner with no
//      paired_user.json on disk, clicks the Connection Wizard's
//      "browser pair" button, completes the web /connect-runner flow,
//      and verifies (a) the runner_token round-trips back through the
//      localhost callback, (b) coord mints a device-JWT, (c) the relay
//      reconnects with the new JWT, (d) the runner appears on the
//      qontinui-web "Connected runners" list. Needs a live web
//      /connect-runner and a live coord — driven by `manual-test-loop`.
//
//   2. Clock-skew across runner/web. The runner's local-exp check
//      (auth::device_jwt_needs_refresh) trusts the JWT's `exp` claim
//      without verifying signature, but web's JWKS verifier on the WS
//      handshake DOES verify the signature against coord's public key.
//      A skewed runner clock can produce a verdict mismatch (local says
//      "fresh," remote says "expired" → 401 spam). Operator changes
//      system clock by +/-15 minutes and verifies the relay still
//      reconnects cleanly via the refresher's 401-handler kick path.
//      Needs JWKS verifier on web; not a runner concern.
//
//   3. JWKS rotation. Coord rotates its signing key; in-flight JWTs
//      minted under the old key continue to verify for the JWKS TTL,
//      after which web rejects them with 401. Verifies coord-side
//      key-rotation deployment — out of scope for the runner.
//
//   4. runner_token revocation. Operator revokes the runner_token via
//      the web UI; the refresher's next pair-cli call must return
//      401 (verified by Phase 5.1 above), the relay must NOT clear the
//      existing JWT until it naturally expires (verified by Phase 5.2's
//      `refresher_handles_coord_401_without_clearing_jwt`), and after
//      JWT expiry the runner must surface "Re-pair required" in the
//      Settings UI instead of 401-spinning. Needs web + coord state
//      coordination.
//
// Do NOT delete this block — it's the migration acceptance criteria and
// the discoverable record of what live-stack work still owes the user.
//
// ============================================================================

#[cfg(test)]
mod try_refresh_once_tests {
    //! Phase 5.2 integration tests — `try_refresh_once` against an
    //! in-process mock coord. These exercise the JWT-preservation
    //! invariant: a non-2xx coord response MUST NOT clear the existing
    //! access_token slot.
    //!
    //! Mock coord: inline axum server on `127.0.0.1:0`, same pattern as
    //! `pair::pair_e2e_tests`. AuthManager: `with_storage(...)` +
    //! `SecureStorage::with_path(<temp_file>)` so each test has its own
    //! isolated tokens file.

    use super::*;
    use axum::{
        body::Bytes,
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::post,
        Router,
    };
    use std::sync::{Arc, Mutex};

    fn b64url(b: &[u8]) -> String {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
    }

    /// Mint a synthetic JWT carrying `exp` so `device_jwt_needs_refresh`
    /// can decode it. Signature isn't verified by the runner, so a
    /// placeholder is fine.
    fn synth_jwt(exp: i64) -> String {
        let header = b64url(b"{\"alg\":\"EdDSA\",\"typ\":\"JWT\"}");
        let payload = b64url(format!("{{\"exp\":{}}}", exp).as_bytes());
        let sig = b64url(b"fake-sig");
        format!("{header}.{payload}.{sig}")
    }

    /// Per-test isolated AuthManager. Each test name maps to its own
    /// `.enc` file under the OS temp dir.
    fn test_auth_manager(name: &str) -> crate::auth::AuthManager {
        let dir = std::env::temp_dir().join("qontinui_test_refresher");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{name}.enc"));
        let _ = std::fs::remove_file(&path);
        let storage = crate::secure_storage::SecureStorage::with_path(path).expect("storage");
        crate::auth::AuthManager::with_storage(storage)
    }

    #[derive(Clone)]
    struct MockState {
        status: StatusCode,
        body: String,
        hits: Arc<Mutex<u32>>,
    }

    async fn handler(State(s): State<MockState>, _h: HeaderMap, _b: Bytes) -> (StatusCode, String) {
        *s.hits.lock().unwrap() += 1;
        (s.status, s.body.clone())
    }

    fn spawn_mock(
        status: StatusCode,
        body: String,
    ) -> (String, Arc<Mutex<u32>>, tokio::sync::oneshot::Sender<()>) {
        let hits = Arc::new(Mutex::new(0u32));
        let hits_for_handler = hits.clone();
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = std_listener.local_addr().expect("addr").port();
        std_listener.set_nonblocking(true).expect("nb");
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("rt");
            rt.block_on(async move {
                let state = MockState {
                    status,
                    body,
                    hits: hits_for_handler,
                };
                let app: Router = Router::new()
                    .route("/coord/devices/pair-cli", post(handler))
                    .with_state(state);
                let listener =
                    tokio::net::TcpListener::from_std(std_listener).expect("tokio listener");
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = rx.await;
                    })
                    .await;
            });
        });
        std::thread::sleep(Duration::from_millis(50));
        (format!("http://127.0.0.1:{port}"), hits, tx)
    }

    const DID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const UID: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    const TOK: &str = "test-runner-token";

    #[tokio::test]
    async fn refresher_handles_coord_401_without_clearing_jwt() {
        // Setup: AuthManager holds a valid-shape (not-yet-expired) JWT.
        // Run: mock coord returns 401 on pair-cli.
        // Assert: access_token slot STILL holds the original JWT (not
        // cleared) AND the outcome is KeptExisting.
        let mgr = test_auth_manager("handles_coord_401_without_clearing");
        let existing_jwt = synth_jwt(chrono::Utc::now().timestamp() + 30 * 60);
        mgr.store_tokens(&existing_jwt, "").expect("store");

        let (base, hits, _shutdown) = spawn_mock(
            StatusCode::UNAUTHORIZED,
            r#"{"error":"token expired"}"#.to_string(),
        );

        let outcome = try_refresh_once(&mgr, &base, TOK, DID, UID).await;
        assert_eq!(
            outcome,
            RefreshOutcome::KeptExisting,
            "401 must yield KeptExisting (not Replaced, not PersistFailed)"
        );
        assert_eq!(*hits.lock().unwrap(), 1, "coord should be hit exactly once");

        let still = mgr.get_access_token().expect("token still present");
        assert_eq!(
            still, existing_jwt,
            "JWT in access_token slot must be UNCHANGED after a 401"
        );
    }

    #[tokio::test]
    async fn refresher_handles_coord_503_without_clearing_jwt() {
        // Same as 401 but with a 503 — coord overloaded / down. We
        // MUST NOT punish the runner by clearing its JWT for a
        // transient server error.
        let mgr = test_auth_manager("handles_coord_503_without_clearing");
        let existing_jwt = synth_jwt(chrono::Utc::now().timestamp() + 30 * 60);
        mgr.store_tokens(&existing_jwt, "").expect("store");

        let (base, _hits, _shutdown) = spawn_mock(
            StatusCode::SERVICE_UNAVAILABLE,
            r#"{"error":"coord overloaded"}"#.to_string(),
        );

        let outcome = try_refresh_once(&mgr, &base, TOK, DID, UID).await;
        assert_eq!(outcome, RefreshOutcome::KeptExisting);

        let still = mgr.get_access_token().expect("token still present");
        assert_eq!(
            still, existing_jwt,
            "JWT in access_token slot must be UNCHANGED after a 503"
        );
    }

    #[tokio::test]
    async fn refresher_handles_coord_200_replaces_jwt() {
        // Setup: AuthManager holds the OLD JWT.
        // Run: mock coord returns canonical 200 with a NEW JWT.
        // Assert: outcome is Replaced with the NEW JWT, and the
        // access_token slot now holds the NEW JWT.
        let mgr = test_auth_manager("handles_coord_200_replaces");
        let old_jwt = synth_jwt(chrono::Utc::now().timestamp() + 30 * 60);
        mgr.store_tokens(&old_jwt, "").expect("store");

        let new_jwt = synth_jwt(chrono::Utc::now().timestamp() + 4 * 60 * 60);
        let body = serde_json::json!({
            "token": new_jwt,
            "device_id": "11111111-1111-4111-8111-111111111111",
            "user_id":   "22222222-2222-4222-8222-222222222222",
            "jti":       "33333333-3333-4333-8333-333333333333",
            "exp":       chrono::Utc::now().timestamp() + 4 * 60 * 60,
        })
        .to_string();

        let (base, _hits, _shutdown) = spawn_mock(StatusCode::OK, body);

        let outcome = try_refresh_once(&mgr, &base, TOK, DID, UID).await;
        match outcome {
            RefreshOutcome::Replaced { new_jwt: got } => {
                assert_eq!(got, new_jwt, "Replaced must carry the new JWT");
            }
            other => panic!("expected Replaced, got {other:?}"),
        }
        let stored = mgr.get_access_token().expect("token present");
        assert_eq!(
            stored, new_jwt,
            "access_token slot must now hold the NEW JWT (not the old one)"
        );
        assert_ne!(
            stored, old_jwt,
            "access_token slot must NOT still hold the old JWT"
        );
    }

    /// Phase 5.4 migration guard: a legacy opaque token in the
    /// access_token slot must report `Ok(true)` from
    /// `device_jwt_needs_refresh` so the refresher heals it on the next
    /// tick. This re-verifies the Phase 2 invariant
    /// (`auth::device_jwt_tests::needs_refresh_when_legacy_opaque_token`)
    /// from the refresher-side perspective: if this ever silently
    /// flips, every pre-Phase-3 paired install will be wedged on the
    /// opaque bearer forever, the relay 401-spinning every reconnect.
    #[test]
    fn refresher_treats_legacy_opaque_token_as_needs_refresh() {
        let mgr = test_auth_manager("treats_legacy_opaque_token");
        mgr.store_tokens("qontinui_runner_legacy_abc123", "")
            .expect("store");
        let needs = mgr
            .device_jwt_needs_refresh()
            .expect("needs_refresh check should not error");
        assert!(
            needs,
            "MIGRATION GUARD: a legacy opaque `qontinui_runner_*` token in \
             the access_token slot MUST be treated as needs-refresh so the \
             refresher replaces it with a real device-JWT. Without this, \
             pre-Phase-3 paired installs are permanently wedged on the \
             opaque bearer and the relay 401-spins every reconnect."
        );
    }
}
