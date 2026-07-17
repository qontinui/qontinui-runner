//! Runner API-port breadcrumb — the out-of-process "which runner is live, and
//! on what port?" discovery seam (plan
//! `2026-07-17-universal-coord-device-identity-for-any-session` §4).
//!
//! # Why this exists
//!
//! Both of the runner's port resolvers are **in-process only**:
//! `install_effects_producer::intercept::bound_port()` reads a process-global
//! `AtomicU16`, and `coord_mcp::resolve_bound_api_port()` needs a live Tauri
//! `AppHandle`. Neither is reachable from a separate process — yet the identity
//! shim (`bin/qontinui_shim.rs`), a *parent* of `claude`, must find a live
//! runner to ask it for coord identity. This file is the only out-of-process
//! answer; nothing else advertises the BOUND port (`settings.json`'s
//! `runner_instances[].port` is the *configured* port, the supervisor's
//! `GET /runners` reports the port it *assigned*, and `coord.runner_instances`
//! is fleet state reachable only via SQL — none is the port a runner confirmed
//! it bound).
//!
//! # Why it lives in the lib crate
//!
//! Exactly the reason `intercept_core` was lifted here: the WRITER is the
//! runner bin (`set_bound_port`) and a READER is the standalone
//! `qontinui-shim` `.exe`, and a second bin cannot import from the runner bin's
//! module tree. One module ⇒ one schema ⇒ the reader and writer can never
//! drift.
//!
//! # Liveness is load-bearing, not defensive padding
//!
//! A breadcrumb file outlives a crashed runner. During this plan's Phase-0
//! probe a real, stale coord-mcp config pointed at port **9879** — a long-dead
//! temp runner — and connect was refused. So the record carries both
//! [`PortBreadcrumb::pid`] and [`PortBreadcrumb::started_at_ms`], and every
//! reader is expected to go through [`select_live`] / [`resolve_live_runner`],
//! which verify the pid is alive rather than discovering the corpse via a
//! connection error (or, worse, a 401 from a DIFFERENT runner that has since
//! taken the port).
//!
//! # Never scan ports
//!
//! `bin/qontinui_cli.rs:434-437` documents the load-bearing rule: a nonce and a
//! port must come from the SAME runner, because a nonce paired with a scanned
//! port 401s. This module upholds it by construction — it returns a *record*
//! (port + pid), and the caller then asks THAT runner to mint, using the config
//! that runner returns. There is deliberately no port-range scan here.
//!
//! # Selection rule
//!
//! Prefer the primary ([`preferred_primary_port`] — `QONTINUI_PRIMARY_PORT`,
//! else `9876`), else ANY live breadcrumb. Temp runners (9877-9899) are
//! explicitly NOT preferred: they are ephemeral and rebuild-spawned, so a bare
//! session must not bind its identity to one that will be torn down mid-session.
//! Prefer-primary handles that; there is no temp-runner affinity.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tracing::warn;

/// Schema version of the on-disk record. Bump only on a BREAKING shape change —
/// an out-of-tree reader (the shim) is expected to ignore a record whose
/// `schema` it does not know rather than misparse it.
pub const BREADCRUMB_SCHEMA: u32 = 1;

/// The conventional primary runner port. Used both for the un-suffixed file
/// name and as the default preferred port when `QONTINUI_PRIMARY_PORT` is unset.
/// Mirrors `mcp::types::MCP_API_PORT` (which lives in the bin crate and so is
/// not importable here).
pub const PRIMARY_PORT: u16 = 9876;

/// Env var naming the primary runner's port for a process that is not itself
/// the primary (set by the supervisor for spawned instances). Read by
/// [`preferred_primary_port`].
pub const PRIMARY_PORT_ENV: &str = "QONTINUI_PRIMARY_PORT";

/// One runner's advertisement of the API port it ACTUALLY bound.
///
/// Written on bind, removed on graceful shutdown — so its presence is a claim,
/// never a guarantee. `pid` + `started_at_ms` are what let a reader disprove the
/// claim cheaply (see the module doc's staleness note).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortBreadcrumb {
    /// [`BREADCRUMB_SCHEMA`] at write time.
    pub schema: u32,
    /// The port the runner's API listener is ACTUALLY bound to (not the
    /// configured/requested one — the runner falls back when a port is taken).
    pub port: u16,
    /// OS pid of the runner process that bound `port`. A reader verifies this is
    /// alive ([`pid_is_live`]) before trusting `port`.
    pub pid: u32,
    /// Unix epoch millis when this process first published a breadcrumb (≈ its
    /// bind instant). Lets a reader break ties / age out a record whose pid was
    /// recycled onto an unrelated process.
    pub started_at_ms: i64,
    /// `true` iff `port` is the conventional primary port ([`PRIMARY_PORT`]).
    /// Denormalized for out-of-tree readers that do not want to re-derive it.
    pub primary: bool,
}

/// `~/.qontinui/runner/` — the established runner app-data dir (the lifecycle
/// store, `session-restore/`, and `last-shutdown.json` all live here). `None`
/// when the home dir is unresolvable.
pub fn breadcrumb_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join("runner"))
}

/// File name for `port`'s breadcrumb, namespaced exactly like the sibling
/// shutdown marker: the primary gets the bare name, every other port gets a
/// `-<port>` suffix, so a temp runner (9877+) can never clobber the primary's
/// record. The bare name is ALSO what makes "prefer the primary" a single read
/// rather than a directory sweep.
///
/// "Primary" is [`preferred_primary_port`] — the SAME notion the reader
/// ([`select_live`]) uses, NOT the hardcoded [`PRIMARY_PORT`]. A primary that
/// bound a `QONTINUI_PRIMARY_PORT`-configured port must get the bare name (and
/// the [`PortBreadcrumb::primary`] flag) the reader is looking for, or a bare
/// session would skip it and mint against a temp runner instead.
pub fn breadcrumb_file_name(port: u16) -> String {
    file_name_for(port, preferred_primary_port())
}

/// Inner, env-free helper: the file name `port` gets when `primary` is the
/// primary port. Split out so writer/reader agreement is unit-testable with an
/// explicit primary rather than through the process-global env var.
fn file_name_for(port: u16, primary: u16) -> String {
    if port == primary {
        "api-port.json".to_string()
    } else {
        format!("api-port-{port}.json")
    }
}

/// Absolute path of `port`'s breadcrumb. `None` when the home dir is
/// unresolvable.
pub fn breadcrumb_path(port: u16) -> Option<PathBuf> {
    breadcrumb_dir().map(|d| d.join(breadcrumb_file_name(port)))
}

/// This process's publish instant (epoch millis), captured on the FIRST publish
/// so a re-publish (a port change mid-life) keeps reporting when the process
/// actually came up rather than resetting its age.
static STARTED_AT_MS: OnceLock<i64> = OnceLock::new();

/// The path this process last published to, so [`remove_published`] can delete
/// exactly the file we wrote — immune to the port drifting between publish and
/// shutdown (the bound port is NOT always the configured port, which is the bug
/// the sibling shutdown marker's `get_mcp_api_port()`-keyed path can hit).
static PUBLISHED_PATH: OnceLock<std::sync::Mutex<Option<PathBuf>>> = OnceLock::new();

fn published_path() -> &'static std::sync::Mutex<Option<PathBuf>> {
    PUBLISHED_PATH.get_or_init(|| std::sync::Mutex::new(None))
}

/// Build the record this process would publish for `port`. The `primary` flag is
/// derived from [`preferred_primary_port`] — the SAME notion the reader uses — so
/// a `QONTINUI_PRIMARY_PORT`-configured primary advertises itself as primary
/// rather than being mislabeled a temp runner (see [`breadcrumb_file_name`]).
pub fn breadcrumb_for(port: u16) -> PortBreadcrumb {
    breadcrumb_for_with_primary(port, preferred_primary_port())
}

/// Inner, env-free helper behind [`breadcrumb_for`]: build the record treating
/// `primary` as the primary port. Split out so the writer's primary-labeling is
/// unit-testable with an explicit primary rather than the process-global env.
fn breadcrumb_for_with_primary(port: u16, primary: u16) -> PortBreadcrumb {
    PortBreadcrumb {
        schema: BREADCRUMB_SCHEMA,
        port,
        pid: std::process::id(),
        started_at_ms: *STARTED_AT_MS.get_or_init(|| chrono::Utc::now().timestamp_millis()),
        primary: port == primary,
    }
}

/// Publish this process's breadcrumb for `port` into [`breadcrumb_dir`].
/// Best-effort: every failure only warns — an unpublished breadcrumb degrades a
/// bare session to "no coord identity" (fail-open), never to a broken runner.
/// Idempotent; a re-publish overwrites in place.
pub fn publish(port: u16) {
    let Some(path) = breadcrumb_path(port) else {
        warn!("runner_breadcrumb: home dir unresolvable — API port :{port} not advertised");
        return;
    };
    if let Err(e) = write_at(&path, &breadcrumb_for(port)) {
        warn!(
            error = %e,
            path = %path.display(),
            "runner_breadcrumb: publish failed — out-of-process port discovery off for this runner"
        );
        return;
    }
    *published_path().lock().expect("breadcrumb path poisoned") = Some(path);
}

/// Remove the breadcrumb this process published, if any. Call from the graceful
/// shutdown paths — a record left behind by a clean exit would make a reader
/// pay a pid probe (and, on a pid recycle, an outright wrong answer) for a
/// runner that deliberately went away. Idempotent; a crashed runner's stale
/// record is what [`pid_is_live`] exists to catch.
pub fn remove_published() {
    let path = published_path()
        .lock()
        .expect("breadcrumb path poisoned")
        .take();
    if let Some(path) = path {
        let _ = std::fs::remove_file(path);
    }
}

/// Atomic write (temp + rename) so a reader never observes a half-written
/// record.
fn write_at(path: &Path, record: &PortBreadcrumb) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(record).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// Read one breadcrumb. `None` for absent / unreadable / unparseable / a schema
/// this build does not know — a record we cannot fully understand is treated as
/// absent rather than half-trusted.
pub fn read_at(path: &Path) -> Option<PortBreadcrumb> {
    let bytes = std::fs::read(path).ok()?;
    let record: PortBreadcrumb = serde_json::from_slice(&bytes).ok()?;
    (record.schema == BREADCRUMB_SCHEMA).then_some(record)
}

/// Every breadcrumb in `dir`, in no particular order (selection is
/// [`select_live`]'s job). Unreadable entries are skipped silently — a foreign
/// file in the runner's app-data dir is not an error.
pub fn list_breadcrumbs_in(dir: &Path) -> Vec<PortBreadcrumb> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with("api-port") && name.ends_with(".json")
        })
        .filter_map(|e| read_at(&e.path()))
        .collect()
}

/// Is `pid` a currently-running process? Backs the staleness check — a
/// breadcrumb whose pid is gone is a corpse, and a reader that trusts it pairs a
/// nonce with a dead (or recycled) port.
///
/// Pid recycling is possible in principle; it is contained by construction
/// rather than defended against here: the caller's next step is to ASK that
/// runner to mint, so a wrong-but-live pid costs one refused/refusing HTTP call,
/// never a mis-paired nonce.
pub fn pid_is_live(pid: u32) -> bool {
    // pid 0 is never a runner: on Windows it is the System Idle Process (which
    // sysinfo DOES report as live), on Unix it is the swapper/"any process in
    // caller's group" sentinel. A breadcrumb whose pid is 0 is malformed, and
    // trusting it would hand a bare session a dead port — so reject it outright,
    // before the process-table refresh.
    if pid == 0 {
        return false;
    }
    let pid = sysinfo::Pid::from_u32(pid);
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    sys.process(pid).is_some()
}

/// The port a reader should PREFER: `QONTINUI_PRIMARY_PORT` when set and
/// parseable, else [`PRIMARY_PORT`].
pub fn preferred_primary_port() -> u16 {
    std::env::var(PRIMARY_PORT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .unwrap_or(PRIMARY_PORT)
}

/// Pick the runner a reader should talk to: the `primary` port's record if it is
/// live, else the live record with the most recent `started_at_ms` (the newest
/// runner is the one an operator most likely just started). `None` when nothing
/// live is advertised.
///
/// `is_live` is injected so the selection rule is unit-testable without spawning
/// processes; production callers pass [`pid_is_live`] via [`resolve_live_runner`].
///
/// # Residual pid-reuse risk (accepted, not fixed here)
///
/// The primary preference is applied WITHIN the already-liveness-filtered set, so
/// a plainly-dead primary corpse is skipped for the newest live runner. One edge
/// survives: a crashed primary that skipped [`remove_published`] leaves
/// `api-port.json`, and on Windows its pid can be recycled onto an unrelated live
/// process, so [`pid_is_live`] returns true and the stale record is preferred
/// over a fresh temp runner. This is deliberately NOT worked around by "prefer
/// the newest live record when the primary looks older": the primary is NORMALLY
/// older than a freshly-spawned temp runner (it comes up at boot; temp runners
/// spawn later), so that heuristic would invert this module's load-bearing
/// invariant — "prefer the primary, NEVER bind a bare session to a temp runner
/// that will be torn down mid-session" — in the common case, to defend a rare
/// pid-recycle. The cost of the residual edge is bounded by construction: the
/// caller's next step is to ASK the selected runner to mint, so a wrong-but-live
/// pid costs one refused HTTP call and a fail-open "no identity", never a
/// mis-paired nonce (same containment the module doc describes for pid reuse).
pub fn select_live(
    all: &[PortBreadcrumb],
    primary: u16,
    is_live: impl Fn(u32) -> bool,
) -> Option<PortBreadcrumb> {
    let live: Vec<&PortBreadcrumb> = all.iter().filter(|b| is_live(b.pid)).collect();
    live.iter()
        .find(|b| b.port == primary)
        .or_else(|| live.iter().max_by_key(|b| b.started_at_ms))
        .map(|b| (*b).clone())
}

/// The live runner to talk to, resolved from the real app-data dir. `None` when
/// the home dir is unresolvable, nothing is advertised, or every advertised
/// runner is dead — all of which a caller must treat as "no coord identity"
/// (fail-open), never as an error worth failing a launch over.
pub fn resolve_live_runner() -> Option<PortBreadcrumb> {
    let dir = breadcrumb_dir()?;
    select_live(
        &list_breadcrumbs_in(&dir),
        preferred_primary_port(),
        pid_is_live,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(port: u16, pid: u32, started_at_ms: i64) -> PortBreadcrumb {
        PortBreadcrumb {
            schema: BREADCRUMB_SCHEMA,
            port,
            pid,
            started_at_ms,
            primary: port == PRIMARY_PORT,
        }
    }

    /// The primary gets the bare file name; every other port is suffixed — so a
    /// temp runner can never clobber the primary's record. (With
    /// `QONTINUI_PRIMARY_PORT` unset, the primary defaults to [`PRIMARY_PORT`].)
    #[test]
    fn file_name_namespaces_non_primary_ports() {
        assert_eq!(breadcrumb_file_name(PRIMARY_PORT), "api-port.json");
        assert_eq!(breadcrumb_file_name(9879), "api-port-9879.json");
    }

    /// FIX 4 — writer/reader agree on which port is "primary". When the primary
    /// binds a `QONTINUI_PRIMARY_PORT`-configured fallback (9878), the WRITER must
    /// mark that record primary + give it the bare name, and the READER must
    /// select it over a live 9877 temp runner. Driven through the env-free inner
    /// helpers with an explicit primary so the agreement is deterministic (no
    /// process-global env), then cross-checked against `select_live`.
    #[test]
    fn writer_and_reader_agree_on_a_configured_primary_port() {
        let configured_primary = 9878;

        // Writer side: the configured-primary record is labeled primary and gets
        // the bare (un-suffixed) file name the reader looks for first.
        let primary_rec = breadcrumb_for_with_primary(configured_primary, configured_primary);
        assert!(
            primary_rec.primary,
            "a record on the configured primary port must be marked primary"
        );
        assert_eq!(file_name_for(configured_primary, configured_primary), "api-port.json");
        // A temp runner (9877) under the same configured primary stays non-primary.
        let temp_rec = breadcrumb_for_with_primary(9877, configured_primary);
        assert!(!temp_rec.primary);
        assert_eq!(file_name_for(9877, configured_primary), "api-port-9877.json");

        // Reader side: with the SAME primary notion, the primary wins over the
        // newer temp runner even though the temp runner started later.
        let primary_live = rec(configured_primary, 10, 100);
        let temp_live = rec(9877, 11, 999); // newer, but a temp runner
        let selected = select_live(&[temp_live, primary_live.clone()], configured_primary, |_| true);
        assert_eq!(
            selected,
            Some(primary_live),
            "the reader must prefer the configured-primary record over a newer temp runner"
        );
    }

    /// FIX 4 — the PUBLIC (env-reading) writer entry points honor
    /// `QONTINUI_PRIMARY_PORT`, proving the wiring, not just the inner helpers.
    #[test]
    fn public_writer_honors_primary_port_env() {
        // Save/restore so this never leaks into a sibling test.
        let prev = std::env::var(PRIMARY_PORT_ENV).ok();
        std::env::set_var(PRIMARY_PORT_ENV, "9878");

        assert_eq!(breadcrumb_file_name(9878), "api-port.json");
        assert!(breadcrumb_for(9878).primary);
        // The hardcoded default port is NOT primary once the env names a different one.
        assert_eq!(breadcrumb_file_name(PRIMARY_PORT), "api-port-9876.json");
        assert!(!breadcrumb_for(PRIMARY_PORT).primary);

        match prev {
            Some(v) => std::env::set_var(PRIMARY_PORT_ENV, v),
            None => std::env::remove_var(PRIMARY_PORT_ENV),
        }
    }

    /// Round-trip through the atomic writer + reader, and reject a record whose
    /// schema this build does not know (treated as absent, never half-trusted).
    #[test]
    fn write_read_roundtrips_and_rejects_unknown_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api-port.json");

        let record = rec(9876, 4242, 1_752_768_000_000);
        write_at(&path, &record).unwrap();
        assert_eq!(read_at(&path), Some(record));

        std::fs::write(
            &path,
            br#"{"schema":999,"port":9876,"pid":1,"started_at_ms":0,"primary":true}"#,
        )
        .unwrap();
        assert_eq!(read_at(&path), None, "an unknown schema reads as absent");

        std::fs::write(&path, b"not json {").unwrap();
        assert_eq!(read_at(&path), None, "a corrupt record reads as absent");
    }

    /// The listing picks up the primary + temp records and ignores the app-data
    /// dir's unrelated neighbours (the shutdown marker lives right beside it).
    #[test]
    fn listing_finds_every_breadcrumb_and_ignores_neighbours() {
        let dir = tempfile::tempdir().unwrap();
        write_at(&dir.path().join("api-port.json"), &rec(9876, 1, 10)).unwrap();
        write_at(&dir.path().join("api-port-9877.json"), &rec(9877, 2, 20)).unwrap();
        std::fs::write(dir.path().join("last-shutdown.json"), b"{}").unwrap();

        let mut ports: Vec<u16> = list_breadcrumbs_in(dir.path())
            .into_iter()
            .map(|b| b.port)
            .collect();
        ports.sort_unstable();
        assert_eq!(ports, vec![9876, 9877]);
    }

    /// Selection: prefer the primary when live; fall back to the NEWEST live
    /// runner; never return a dead one — the staleness failure this record's
    /// `pid` exists to catch (a real config pointed at a dead :9879).
    #[test]
    fn select_live_prefers_primary_then_newest_and_never_a_corpse() {
        let primary = rec(9876, 1, 100);
        let temp_old = rec(9877, 2, 200);
        let temp_new = rec(9878, 3, 300);
        let all = vec![primary.clone(), temp_old.clone(), temp_new.clone()];

        // Everything live → the primary wins even though it is the oldest.
        assert_eq!(
            select_live(&all, PRIMARY_PORT, |_| true),
            Some(primary.clone())
        );

        // Primary dead → the NEWEST live temp runner wins.
        assert_eq!(
            select_live(&all, PRIMARY_PORT, |pid| pid != 1),
            Some(temp_new)
        );

        // Nothing live → None (fail-open: the caller simply gets no identity,
        // and never pairs a nonce with a dead port).
        assert_eq!(select_live(&all, PRIMARY_PORT, |_| false), None);

        // An explicit non-9876 primary (QONTINUI_PRIMARY_PORT) is honored.
        assert_eq!(select_live(&all, 9877, |_| true), Some(temp_old));
    }

    /// Our own pid is live; pid 0 is not a live user process on any supported
    /// platform (it is the Windows System Idle Process / an invalid Unix pid).
    #[test]
    fn pid_liveness_detects_this_process() {
        assert!(pid_is_live(std::process::id()));
        assert!(!pid_is_live(0));
    }

    /// `remove_published` deletes exactly the file publish wrote, and is
    /// idempotent (a second call, or one with nothing published, is a no-op).
    #[test]
    fn remove_published_is_idempotent() {
        remove_published();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api-port-9899.json");
        write_at(&path, &rec(9899, 7, 1)).unwrap();
        *published_path().lock().unwrap() = Some(path.clone());

        remove_published();
        assert!(!path.exists(), "the published record is removed on shutdown");
        remove_published(); // no-op, no panic
    }
}
