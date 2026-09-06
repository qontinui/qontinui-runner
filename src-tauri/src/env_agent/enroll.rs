//! Shared enroll code path — binds this machine to a web devenv environment via
//! a one-time enrollment code.
//!
//! This is the SINGLE implementation of the enroll POST used by every consumer:
//! - the `qontinui_profile env enroll --code <code>` CLI (`bin/qontinui_profile.rs`),
//! - the in-app Tauri command (`commands::devenv_enroll`), and
//! - (Phase 3) the dispatched `events.devenv.enroll_requested` directive consumer.
//!
//! The enroll flow mirrors the backend contract in
//! `qontinui-web` `app/api/v1/endpoints/devenv_agent.py`: POST
//! `{enrollment_code, machine_id?, hostname?}` to
//! `{backend}/api/v1/devenv/agent/enroll`; on success the response carries the
//! machine key ONCE (stored immediately in `SecureStorage`) plus the
//! `environment_id` (ALWAYS sourced from the response, never hardcoded). Nothing
//! is written on failure.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::config::EnvAgentConfig;

/// Wire shape of the enroll request body. Conforms EXACTLY to the backend
/// contract: `{ enrollment_code, machine_id?, hostname?, coord_device_id? }`.
#[derive(Debug, Serialize)]
struct EnrollRequest {
    enrollment_code: String,
    /// The **devenv** machine UUID. `None` on every locally-initiated enroll —
    /// see [`EnrollParams::machine_id`]. The backend only sanity-checks it when
    /// non-null (`payload.machine_id is not None and != machine.id`), so a null
    /// here is the correct "don't assert an identity I can't know" signal.
    machine_id: Option<String>,
    hostname: Option<String>,
    /// This machine's coord device identity (UUID). Omitted from the wire
    /// entirely when absent so the request stays compatible with backends that
    /// predate the contract extension (qontinui-web#697).
    #[serde(skip_serializing_if = "Option::is_none")]
    coord_device_id: Option<uuid::Uuid>,
}

/// Wire shape of the enroll response. The backend returns the machine key ONCE
/// — we store it immediately. `environment_id` is sourced from HERE.
#[derive(Debug, Deserialize)]
struct EnrollResponse {
    machine_id: String,
    machine_key: String,
    #[serde(default)]
    environment_id: Option<String>,
}

/// Fully-resolved enroll inputs. The CALLER resolves backend + identity so this
/// core is transport-agnostic and free of CLI-arg / profile coupling. Use
/// [`resolve_backend_base`] + [`local_machine_identity`] to populate the
/// non-`code` fields from the ambient environment.
#[derive(Debug, Clone)]
pub struct EnrollParams {
    /// The one-time enrollment code minted by the web dashboard.
    pub code: String,
    /// Web backend base URL (no trailing slash needed — trimmed here).
    pub backend: String,
    /// The **devenv** machine UUID (`devenv_machines.id`), asserted only when we
    /// already know it — i.e. a server-issued enroll directive supplied it. The
    /// agent CANNOT derive this locally: enroll is what RETURNS it, and the
    /// enrollment code already identifies the machine server-side. Leave `None`
    /// in every locally-initiated enroll.
    ///
    /// This is a DIFFERENT identity space from coord's `device_id` in
    /// `~/.qontinui/machine.json` — sending the latter here made the backend's
    /// `payload.machine_id != machine.id` sanity check fail unconditionally with
    /// `409 machine_id_mismatch` on every re-enroll. Coord's identity travels in
    /// [`EnrollParams::coord_device_id`] instead.
    pub machine_id: Option<String>,
    /// This machine's hostname, or `None`.
    pub hostname: Option<String>,
    /// This machine's coord device identity (UUID) from
    /// `~/.qontinui/machine.json::device_id`. Sent to the backend so it can
    /// persist the devenv↔coord twin bridge linkage. `None` when the local
    /// identity is absent/unparseable — enroll still works, the field is just
    /// omitted from the wire.
    pub coord_device_id: Option<uuid::Uuid>,
    /// Diagnostic override for `environment_id`; the response value wins when
    /// present. Normally `None`.
    pub environment_override: Option<String>,
}

/// Successful enroll outcome — what got persisted.
#[derive(Debug, Clone, Serialize)]
pub struct EnrollOutcome {
    pub machine_id: String,
    pub environment_id: String,
    pub backend_url: String,
}

/// The reason a caller gets when another enroll is already running IN THIS
/// PROCESS. Surfaced verbatim by the relay's `devenv_enroll_ack.reason` and by
/// the Settings-panel Enroll button (`commands::devenv_enroll`), so it is both a
/// wire-visible string and operator-facing UI text — treat a change as a
/// contract change.
///
/// It names the likely cause and a retry horizon because the interactive path
/// can genuinely hit it: an operator clicking Enroll while the relay's
/// automatic enrollment is mid-flight would otherwise see four bare words with
/// no indication that waiting fixes it. ~30s is `run_enroll`'s own POST timeout
/// (the longest the holder can legitimately run), not a guess.
pub const ENROLL_ALREADY_IN_FLIGHT: &str =
    "enroll already in flight (an automatic enrollment is running — retry in ~30s)";

/// Process-wide "an enroll is running" flag. See [`with_enroll_slot`].
static ENROLL_IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// RAII release for [`ENROLL_IN_FLIGHT`]. Drop-based so the slot is freed on
/// EVERY exit from the critical section, including the `?` early-returns
/// scattered through [`run_enroll`] and an unwinding panic — a leaked flag would
/// wedge enrollment for the life of the process, which is a worse failure than
/// the race it guards.
struct EnrollInFlightGuard;

impl Drop for EnrollInFlightGuard {
    fn drop(&mut self) {
        ENROLL_IN_FLIGHT.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Run `f` as THE enroll for this process, or refuse with
/// [`ENROLL_ALREADY_IN_FLIGHT`] when one is already running.
///
/// **Why this exists.** [`run_enroll`] performs two unguarded writes in order —
/// `store_agent_machine_key` and then `EnvAgentConfig::save`. Two overlapping
/// enrolls interleave as:
///
/// ```text
/// A: store_agent_machine_key(key_A)
/// B: store_agent_machine_key(key_B)   // overwrites A's key
/// B: cfg.save() -> machine_id_B
/// A: cfg.save() -> machine_id_A       // overwrites B's config
/// ```
///
/// leaving `env-agent.json` naming machine A while secure storage holds machine
/// B's key. Every later PUT then authenticates as the wrong machine — a box that
/// looks enrolled and reports nothing, forever, with no error anywhere. It is
/// the same silent-forever class the instance gate guards, reached by a
/// different route.
///
/// **Refuse, don't queue.** A second caller returns immediately rather than
/// blocking. The relay's `devenv_enroll` handler runs on a liveness budget
/// (`DEVENV_ENROLL_ACK_BUDGET`) because it is awaited serially in the WS read
/// loop; queueing here would push that handler back over the budget the timeout
/// exists to enforce, re-introducing the connection drop. A refused enroll is
/// also cheap to recover from — the server re-dispatches on the next connect.
///
/// # Scope: IN-PROCESS ONLY — two cross-process races remain OPEN
///
/// `ENROLL_IN_FLIGHT` is a process-global atomic, so it serializes only callers
/// that share this process image: `commands::devenv_enroll` (the Settings-panel
/// button), `directive::handle_enroll_directive` (coord's dispatch channel), and
/// `mcp::backend_relay`'s `devenv_enroll` frame. Those three are the ones that
/// fire automatically, so this closes the races that actually recur. It does NOT
/// close:
///
/// - **CLI vs. runner.** `qontinui_profile env enroll` (`profile_cli.rs`
///   `cmd_env_enroll`) runs in `src/bin/qontinui_profile.rs`, a SEPARATE binary
///   and therefore a separate process, with its own `ENROLL_IN_FLIGHT` that is
///   always `false`. An operator pasting the enroll one-liner while the relay
///   reconnects hits exactly the interleave diagrammed above.
/// - **Secondary vs. primary.** Nothing outside the relay's own refusal gate
///   checks `instance::owns_shared_root_state()` —
///   `directive::spawn_enroll_directive_subscriber()` is spawned
///   unconditionally at runner boot, so a secondary instance subscribes and
///   enrolls too, against the machine-global `~/.qontinui/env-agent.json`.
///
/// Closing either needs a MACHINE-wide lock — a lock file beside
/// `env-agent.json` — not a bigger atomic. That is deliberately deferred: stale-
/// lock recovery and Windows file-locking semantics are their own phase, and a
/// stale lock that bricks enrollment on a box would be a worse failure than the
/// race it replaced. Tracked as follow-up on plan
/// `2026-08-05-devenv-auto-enrollment-on-connection`. Until then, treat "an
/// enroll cannot overlap" as true only within one process.
pub(crate) fn with_enroll_slot<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    use std::sync::atomic::Ordering;
    if ENROLL_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(ENROLL_ALREADY_IN_FLIGHT.to_string());
    }
    let _guard = EnrollInFlightGuard;
    f()
}

/// Perform the enroll POST, store the minted machine key in secure storage, and
/// write `~/.qontinui/env-agent.json`. Blocking (`reqwest::blocking`) — call
/// directly from the sync CLI, or via `spawn_blocking` from an async Tauri
/// command. Writes NOTHING on any failure (returns `Err`).
///
/// Serialized by [`with_enroll_slot`]: a second concurrent call returns
/// [`ENROLL_ALREADY_IN_FLIGHT`] without touching secure storage or the config.
/// Guarding HERE rather than at any one call site is deliberate — every
/// IN-PROCESS caller gets it, including the two that overlap in practice
/// (coord's dispatched directive and the relay's socket-pushed frame).
///
/// **The guard is in-process only.** The `qontinui_profile` CLI is a separate
/// binary, so a CLI enroll and a runner enroll can still interleave and corrupt
/// the key/config pair. See [`with_enroll_slot`] for both open races and why
/// the machine-wide lock is deferred rather than built here.
pub fn run_enroll(params: EnrollParams) -> Result<EnrollOutcome, String> {
    with_enroll_slot(move || run_enroll_locked(params))
}

/// [`run_enroll`]'s body, running under the in-flight slot.
fn run_enroll_locked(params: EnrollParams) -> Result<EnrollOutcome, String> {
    let code = params.code.trim();
    if code.is_empty() {
        return Err("enrollment code is empty".to_string());
    }
    let backend = params.backend.trim().trim_end_matches('/').to_string();
    if backend.is_empty() {
        return Err("backend base URL is empty".to_string());
    }

    let body = EnrollRequest {
        enrollment_code: code.to_string(),
        machine_id: params.machine_id.clone(),
        hostname: params.hostname.clone(),
        coord_device_id: params.coord_device_id,
    };
    let url = format!("{backend}/api/v1/devenv/agent/enroll");

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("reqwest client build failed: {e}"))?;
    // coord-auth-exempt(not-coord): `qontinui-web` `/api/v1/devenv/agent/enroll`.
    // The enrollment CODE is the credential; this runs before any device JWT
    // exists.
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .map_err(|e| format!("POST {url} failed (backend unreachable?): {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let body_text = resp
            .text()
            .unwrap_or_else(|_| "<unable to read response body>".to_string());
        return Err(format!(
            "enroll failed — POST {url} -> HTTP {status}: {body_text}"
        ));
    }

    let parsed: EnrollResponse = resp
        .json()
        .map_err(|e| format!("enroll succeeded but decoding the response failed: {e}"))?;

    // environment_id ALWAYS comes from the response when present; the override is
    // a diagnostic fallback only.
    let environment_id = parsed
        .environment_id
        .clone()
        .or(params.environment_override)
        .filter(|e| !e.trim().is_empty())
        .ok_or_else(|| {
            "enroll response did not include an environment_id and none was supplied; \
             refusing to write a half-enrolled config"
                .to_string()
        })?;

    // Store the machine key FIRST — it is the credential.
    let storage = crate::secure_storage::SecureStorage::new()
        .map_err(|e| format!("could not open secure storage: {e}"))?;
    storage
        .store_agent_machine_key(&parsed.machine_key)
        .map_err(|e| format!("failed to store machine key: {e}"))?;

    // Then write env-agent.json with the RESPONSE environment_id.
    //
    // Loaded ONCE and borrowed by both carry-forwards: enroll rewrites the file
    // wholesale, so every operator-owned field has to be read from the same
    // snapshot — two independent `load()`s could straddle a concurrent write and
    // carry forward a mix of two configs.
    let prior = EnvAgentConfig::load();
    let cfg = EnvAgentConfig {
        backend_url: backend.clone(),
        machine_id: parsed.machine_id.clone(),
        environment_id: environment_id.clone(),
        enrolled_at: Some(chrono::Utc::now().to_rfc3339()),
        scope_root: carried_forward_scope_root(prior.as_ref()),
        repo_owner_allowlist: carried_forward_repo_owner_allowlist(prior.as_ref()),
    };
    cfg.save()
        .map_err(|e| format!("enrolled + key stored, but writing env-agent.json failed: {e}"))?;

    Ok(EnrollOutcome {
        machine_id: parsed.machine_id,
        environment_id,
        backend_url: backend,
    })
}

/// The `scope_root` an enroll must write, given whatever config was already on
/// disk: the PRIOR value, carried forward unchanged.
///
/// Enroll rewrites `env-agent.json` **wholesale**, so returning `None` here
/// would silently erase an operator's declared capture scope on every
/// re-enroll. The symptom would not be an error — it would be a box quietly
/// re-measuring a different toolchain and reporting drift nobody could explain.
///
/// Split out from [`run_enroll`] purely so this is reachable by a test: the
/// carry-forward sits AFTER the enroll POST, so no test that stops at
/// validation can observe it.
fn carried_forward_scope_root(prior: Option<&EnvAgentConfig>) -> Option<String> {
    prior.and_then(|p| p.scope_root.clone())
}

/// The `repo_owner_allowlist` an enroll must write: the PRIOR value, carried
/// forward unchanged, for exactly the reason [`carried_forward_scope_root`]
/// gives.
///
/// The silent-erasure symptom differs in shape and is worth naming: an erased
/// allowlist does not stop the `repos` section, it WIDENS it — the box starts
/// reporting every checkout under its workspace root, including the developer's
/// unrelated personal ones, each of which the server reads as an `added` delta
/// that breaks `in_sync`. A re-enroll would turn an in-sync box into a
/// permanently-drifting one with no error anywhere.
fn carried_forward_repo_owner_allowlist(prior: Option<&EnvAgentConfig>) -> Vec<String> {
    prior
        .map(|p| p.repo_owner_allowlist.clone())
        .unwrap_or_default()
}

/// Resolve the web backend base URL for the enroll POST. Order:
/// explicit arg → `QONTINUI_WEB_BASE` → [`crate::profiles::PROD_API_BASE_URL`].
/// Shared by the CLI (`resolve_env_backend_base`) and the Tauri command.
///
/// The last rung used to derive a base from the active profile's `coord_url`.
/// That was never correct — in production coord and the web backend are
/// different services, and in dev they share a host but not a port (the
/// derivation stripped the port). It is the same defect `tenant_sync`
/// documents working around. This module is in the LIB crate and so cannot
/// reach `api_config::get_api_base_url`, the canonical resolver the runner
/// binary uses; callers wanting the persisted `web_integration.backend_url`
/// weighed in must pass it as `explicit`.
pub fn resolve_backend_base(explicit: Option<&str>) -> Result<String, String> {
    if let Some(b) = explicit {
        let t = b.trim();
        if !t.is_empty() {
            return Ok(t.trim_end_matches('/').to_string());
        }
    }
    if let Ok(v) = std::env::var("QONTINUI_WEB_BASE") {
        let t = v.trim();
        if !t.is_empty() {
            return Ok(t.trim_end_matches('/').to_string());
        }
    }
    Ok(crate::profiles::PROD_API_BASE_URL.to_string())
}

/// This machine's local identity as read from `~/.qontinui/machine.json`.
///
/// Deliberately does NOT carry a devenv `machine_id`: that file holds **coord's**
/// device identity, which lives in a different identity space than the devenv
/// `devenv_machines.id`. See [`EnrollParams::machine_id`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalMachineIdentity {
    /// `true` when `~/.qontinui/machine.json` was found, readable, AND parseable.
    /// This is the explicit presence signal callers should use to decide whether
    /// to warn the operator — never infer it from a wire field.
    pub machine_json_present: bool,
    /// This machine's hostname, or `None` when absent/blank.
    pub hostname: Option<String>,
    /// Coord's device identity (`device_id`) parsed as a UUID, or `None` when the
    /// file is absent or the value is not a UUID.
    pub coord_device_id: Option<uuid::Uuid>,
}

/// Parse the bytes of a `~/.qontinui/machine.json` into a [`LocalMachineIdentity`].
/// Accepts the legacy `machine_id` key as an alias for `device_id`, matching the
/// CLI reader. Unparseable input yields the default (absent) identity. Split out
/// from [`local_machine_identity`] so it is unit-testable without a real HOME.
fn parse_machine_json(bytes: &[u8]) -> LocalMachineIdentity {
    #[derive(Deserialize)]
    struct DeviceFile {
        #[serde(alias = "machine_id")]
        device_id: String,
        #[serde(default)]
        hostname: String,
    }
    match serde_json::from_slice::<DeviceFile>(bytes) {
        Ok(f) => LocalMachineIdentity {
            machine_json_present: true,
            hostname: if f.hostname.trim().is_empty() {
                None
            } else {
                Some(f.hostname)
            },
            coord_device_id: uuid::Uuid::parse_str(f.device_id.trim()).ok(),
        },
        Err(_) => LocalMachineIdentity::default(),
    }
}

/// Read `~/.qontinui/machine.json` into a [`LocalMachineIdentity`]. A
/// missing/unreadable/unparseable file yields the default (all-absent) identity
/// — enroll tolerates a null identity, the backend identifies the machine from
/// the enrollment code.
pub fn local_machine_identity() -> LocalMachineIdentity {
    let Some(home) = dirs::home_dir() else {
        return LocalMachineIdentity::default();
    };
    let path = home.join(".qontinui").join("machine.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return LocalMachineIdentity::default();
    };
    parse_machine_json(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Re-enroll must PRESERVE a declared capture scope. Enroll replaces the
    /// whole config file, so a regression here erases the operator's
    /// `scope_root` — and because the erased state is a perfectly valid config,
    /// the only visible symptom is the box silently re-measuring a different
    /// toolchain. That is worth a test even though the helper is one line.
    #[test]
    fn enroll_carries_a_declared_scope_root_forward() {
        let prior = EnvAgentConfig {
            backend_url: "http://h:8000".to_string(),
            machine_id: "m".to_string(),
            environment_id: "e".to_string(),
            enrolled_at: Some("2026-06-22T00:00:00Z".to_string()),
            scope_root: Some("D:/qontinui-root".to_string()),
            repo_owner_allowlist: Vec::new(),
        };
        assert_eq!(
            carried_forward_scope_root(Some(&prior)).as_deref(),
            Some("D:/qontinui-root"),
        );
    }

    /// A first enroll (no config on disk) and a re-enroll of a box that never
    /// declared a scope both yield `None` — the field stays absent rather than
    /// being invented.
    #[test]
    fn enroll_leaves_scope_root_unset_when_there_was_none() {
        assert!(carried_forward_scope_root(None).is_none());
        assert!(carried_forward_scope_root(Some(&EnvAgentConfig {
            backend_url: "http://h:8000".to_string(),
            machine_id: "m".to_string(),
            environment_id: "e".to_string(),
            enrolled_at: None,
            scope_root: None,
            repo_owner_allowlist: Vec::new(),
        }))
        .is_none());
    }

    /// Re-enroll must preserve the repos owner allowlist for the same reason it
    /// preserves `scope_root`, but with a worse failure shape: an erased
    /// allowlist does not silence the `repos` section, it WIDENS it. The box
    /// starts reporting every checkout under its workspace root, each of the
    /// unrelated ones landing as an `added` delta that breaks `in_sync` — an
    /// in-sync box turned permanently-drifting, with no error anywhere.
    #[test]
    fn enroll_carries_the_repo_owner_allowlist_forward() {
        let prior = EnvAgentConfig {
            backend_url: "http://h:8000".to_string(),
            machine_id: "m".to_string(),
            environment_id: "e".to_string(),
            enrolled_at: None,
            scope_root: None,
            repo_owner_allowlist: vec!["qontinui".to_string(), "acme".to_string()],
        };
        assert_eq!(
            carried_forward_repo_owner_allowlist(Some(&prior)),
            vec!["qontinui".to_string(), "acme".to_string()],
        );
    }

    /// A first enroll leaves the allowlist empty — which the collector reads as
    /// "no filter", not as "report nothing".
    #[test]
    fn enroll_leaves_the_allowlist_empty_when_there_was_none() {
        assert!(carried_forward_repo_owner_allowlist(None).is_empty());
    }

    #[test]
    fn resolve_backend_prefers_explicit_and_trims_slash() {
        assert_eq!(
            resolve_backend_base(Some("https://qontinui.io/")).unwrap(),
            "https://qontinui.io"
        );
    }

    /// `ENROLL_IN_FLIGHT` is process-global, so any two tests that enter the
    /// slot race in the parallel harness — one would see the other's hold and
    /// get `ENROLL_ALREADY_IN_FLIGHT` instead of the error it asserts. Every
    /// test that reaches [`with_enroll_slot`] (directly or via [`run_enroll`])
    /// holds this for its whole body. Poison-recovering so a panicking test
    /// cannot cascade-fail the rest (the panic-path test panics BY DESIGN).
    fn slot_lock() -> std::sync::MutexGuard<'static, ()> {
        static SLOT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        SLOT_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn run_enroll_rejects_empty_code() {
        let _slot = slot_lock();
        let err = run_enroll(EnrollParams {
            code: "   ".to_string(),
            backend: "https://qontinui.io".to_string(),
            machine_id: None,
            hostname: None,
            coord_device_id: None,
            environment_override: None,
        })
        .unwrap_err();
        assert!(err.contains("enrollment code is empty"), "got: {err}");
    }

    #[test]
    fn run_enroll_rejects_empty_backend() {
        let _slot = slot_lock();
        let err = run_enroll(EnrollParams {
            code: "ENR-ABC".to_string(),
            backend: "  ".to_string(),
            machine_id: None,
            hostname: None,
            coord_device_id: None,
            environment_override: None,
        })
        .unwrap_err();
        assert!(err.contains("backend base URL is empty"), "got: {err}");
    }

    // ------------------------------------------------------------------
    // In-flight guard. `run_enroll` writes the machine key and then the config,
    // unguarded, from four independent callers — two of which (coord's
    // dispatched directive and the relay's socket-pushed frame) can genuinely
    // overlap. An interleave leaves the config naming machine A while storage
    // holds machine B's key, so every later PUT authenticates as the wrong
    // machine, forever, with no error anywhere.
    // ------------------------------------------------------------------

    /// Two OVERLAPPING enrolls: the second is refused, and exactly ONE body
    /// runs — so exactly one key/config pair is ever written. The overlap is
    /// deterministic (channel rendezvous), not timing-dependent: the second
    /// attempt is made while the first is provably parked inside its body.
    #[test]
    fn overlapping_enrolls_refuse_the_second_and_write_exactly_one_pair() {
        let _slot = slot_lock();
        use std::sync::mpsc;
        use std::sync::{Arc, Mutex};

        // Stands in for the (key, config) pair `run_enroll` persists.
        let written: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();

        let written_a = Arc::clone(&written);
        let first = std::thread::spawn(move || {
            with_enroll_slot(move || {
                // Park INSIDE the critical section, exactly where the real body
                // sits between `store_agent_machine_key` and `cfg.save()`.
                entered_tx.send(()).expect("test harness alive");
                release_rx.recv().expect("test harness alive");
                written_a.lock().expect("not poisoned").push("A");
                Ok(())
            })
        });

        entered_rx
            .recv()
            .expect("the first enroll must reach its body");

        // …and now, with the first still in flight, a second arrives.
        let written_b = Arc::clone(&written);
        let second = with_enroll_slot(move || {
            written_b.lock().expect("not poisoned").push("B");
            Ok(())
        });
        let err = second.expect_err("a concurrent enroll must be REFUSED, not queued");
        assert!(
            err.contains(ENROLL_ALREADY_IN_FLIGHT),
            "the refusal must name itself so the ack's reason is actionable: {err}"
        );

        release_tx
            .send(())
            .expect("the first enroll must still be parked");
        first
            .join()
            .expect("the first enroll thread must not panic")
            .expect("the FIRST enroll must succeed — the guard refuses the late one");

        assert_eq!(
            *written.lock().expect("not poisoned"),
            vec!["A"],
            "exactly one key/config pair may be written; the refused enroll must \
             not have reached its body at all"
        );
    }

    /// The slot is a guard, not a latch: it must be released on the error and
    /// panic paths too, or one failed enroll wedges enrollment for the life of
    /// the process — a worse failure than the race being guarded.
    #[test]
    fn enroll_slot_is_released_after_success_error_and_panic() {
        let _slot = slot_lock();
        assert!(with_enroll_slot(|| Ok::<_, String>(1)).is_ok());
        assert_eq!(
            with_enroll_slot(|| Err::<(), String>("boom".to_string())).unwrap_err(),
            "boom",
            "an inner error passes through untouched, not masked by the guard"
        );

        let panicked = std::panic::catch_unwind(|| {
            let _ = with_enroll_slot(|| -> Result<(), String> { panic!("inner panic") });
        });
        assert!(panicked.is_err(), "sanity: the panic must propagate");

        // Still acquirable after all three exits.
        assert!(
            with_enroll_slot(|| Ok::<_, String>(())).is_ok(),
            "the slot leaked — enrollment is now wedged for the process lifetime"
        );
    }

    /// Every caller inherits the guard because it lives in `run_enroll` itself,
    /// not at a call site. This pins that the entry point really is wrapped: the
    /// validation error can only be reached from INSIDE the slot.
    #[test]
    fn run_enroll_refuses_while_another_is_in_flight() {
        let _slot = slot_lock();
        use std::sync::mpsc;

        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let held = std::thread::spawn(move || {
            with_enroll_slot(move || {
                entered_tx.send(()).expect("test harness alive");
                release_rx.recv().expect("test harness alive");
                Ok::<(), String>(())
            })
        });
        entered_rx.recv().expect("the holder must reach its body");

        // An enroll that would otherwise fail validation instantly instead
        // reports the in-flight refusal — proof the guard wraps the whole fn.
        let err = run_enroll(EnrollParams {
            code: "   ".to_string(),
            backend: "https://qontinui.io".to_string(),
            machine_id: None,
            hostname: None,
            coord_device_id: None,
            environment_override: None,
        })
        .unwrap_err();
        assert!(
            err.contains(ENROLL_ALREADY_IN_FLIGHT),
            "run_enroll must refuse before validating: {err}"
        );

        release_tx
            .send(())
            .expect("the holder must still be parked");
        held.join().expect("holder thread").expect("holder body");
    }

    #[test]
    fn enroll_request_serializes_coord_device_id_only_when_present() {
        // Present → the `coord_device_id` key appears with the UUID string.
        let id = uuid::Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let with = EnrollRequest {
            enrollment_code: "ENR-ABC".to_string(),
            machine_id: Some("dev-1".to_string()),
            hostname: Some("host-1".to_string()),
            coord_device_id: Some(id),
        };
        let v = serde_json::to_value(&with).unwrap();
        assert_eq!(
            v.get("coord_device_id").and_then(|c| c.as_str()),
            Some("11111111-2222-3333-4444-555555555555"),
            "coord_device_id should serialize as the UUID string when Some"
        );

        // Absent → the key is OMITTED entirely (skip_serializing_if), keeping the
        // request compatible with pre-#697 backends.
        let without = EnrollRequest {
            enrollment_code: "ENR-ABC".to_string(),
            machine_id: None,
            hostname: None,
            coord_device_id: None,
        };
        let v = serde_json::to_value(&without).unwrap();
        assert!(
            v.get("coord_device_id").is_none(),
            "coord_device_id key must be absent when None, got: {v}"
        );
    }

    /// A PRESENT, readable `machine.json` must NOT put coord's `device_id` into
    /// the devenv `machine_id` wire field — that conflation is what made the
    /// backend's `payload.machine_id != machine.id` check fail unconditionally
    /// (`409 machine_id_mismatch`) on every re-enroll of a bound machine.
    #[test]
    fn present_machine_json_yields_no_devenv_machine_id_but_keeps_coord_device_id() {
        let identity = parse_machine_json(
            br#"{"device_id":"11111111-2222-3333-4444-555555555555","hostname":"box-1"}"#,
        );
        assert!(identity.machine_json_present);
        assert_eq!(identity.hostname.as_deref(), Some("box-1"));
        assert_eq!(
            identity.coord_device_id,
            Some(uuid::Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap())
        );

        // Build the wire body exactly as every locally-initiated enroll does.
        let body = EnrollRequest {
            enrollment_code: "ENR-ABC".to_string(),
            machine_id: None,
            hostname: identity.hostname.clone(),
            coord_device_id: identity.coord_device_id,
        };
        let v = serde_json::to_value(&body).unwrap();
        let wire_machine_id = v.get("machine_id");
        assert!(
            match wire_machine_id {
                None => true,
                Some(m) => m.is_null(),
            },
            "devenv machine_id must never be asserted from local identity, got: {v}"
        );
        assert_eq!(
            v.get("coord_device_id").and_then(|c| c.as_str()),
            Some("11111111-2222-3333-4444-555555555555"),
            "coord identity must still travel in coord_device_id, got: {v}"
        );
    }

    /// The legacy `machine_id` alias still parses, and still does NOT become the
    /// devenv `machine_id` — it is coord's identity under an old key name.
    #[test]
    fn legacy_machine_id_key_parses_as_coord_device_id() {
        let identity =
            parse_machine_json(br#"{"machine_id":"11111111-2222-3333-4444-555555555555"}"#);
        assert!(identity.machine_json_present);
        assert!(identity.hostname.is_none(), "blank hostname → None");
        assert!(identity.coord_device_id.is_some());
    }

    /// A non-UUID `device_id` is still a PRESENT file: the presence signal must
    /// not collapse into "is the coord UUID parseable", or the CLI would warn
    /// about a file that exists and is readable.
    #[test]
    fn non_uuid_device_id_is_still_present() {
        let identity = parse_machine_json(br#"{"device_id":"not-a-uuid","hostname":"box-1"}"#);
        assert!(identity.machine_json_present);
        assert!(identity.coord_device_id.is_none());
    }

    /// Unreadable/unparseable content ⇒ the all-absent identity, which is what
    /// drives the CLI's "no readable machine.json" note.
    #[test]
    fn unparseable_machine_json_yields_absent_identity() {
        for bad in [&b"{"[..], &b""[..], &b"{\"hostname\":\"box-1\"}"[..]] {
            let identity = parse_machine_json(bad);
            assert_eq!(
                identity,
                LocalMachineIdentity::default(),
                "unparseable input must yield the absent identity"
            );
            assert!(!identity.machine_json_present);
        }
    }
}
