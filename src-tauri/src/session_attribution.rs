//! Hook-free, runner-side WIP-attribution capture loop.
//!
//! ## Why this lives in the runner
//!
//! The original WIP-attribution capture was a `claude-config` `PostToolUse`
//! hook. But `claude-config` is operator-specific (it isn't deployed
//! fleet-wide), so the hook only ever captured the ONE operator's machine.
//! The runner is the only component present on EVERY fleet host, so we
//! re-home the capture here — no hooks. The runner reads each hosted
//! session's Claude transcript and POSTs file-edit attribution to coord,
//! exactly mirroring [`crate::fleet`]'s `tree_publisher`.
//!
//! This loop is a sibling of [`crate::fleet::spawn_tree_publisher`]:
//! - Reuses [`crate::fleet::load_device_file`] for identity (`device_id`).
//! - Reuses [`qontinui_runner_lib::profiles::connected_coord_base`] for the
//!   coord HTTP endpoint (the single connected-vs-isolated definition).
//! - Reuses [`crate::terminal::transcript::session_transcript_path`] for the
//!   `$CLAUDE_CONFIG_DIR/projects/<encoded-cwd>/<session-id>.jsonl` path so we
//!   never hand-roll Claude's project-path encoding.
//! - Reuses [`crate::terminal::transcript::parse_line_for_touched_files`] for
//!   the Edit/Write/MultiEdit `tool_use` extraction (NotebookEdit is correctly
//!   excluded by that helper's explicit match arm).
//! - Enumerates hosted/live sessions from
//!   [`crate::session::session_lifecycle_store::SessionLifecycleStore`].
//! - Best-effort: every error `warn!`s and the loop continues; it never panics.
//! - Same `MissedTickBehavior::Skip` posture so a system suspend doesn't blast
//!   catch-up ticks.
//!
//! ## Gate (default OFF)
//!
//! `COORD_SESSION_ATTRIBUTION_ENABLED` — default OFF. When off the spawner logs
//! "disabled" once and returns without starting the task.
//!
//! **The consumers are live, and have been since coord#630** (merged
//! 2026-06-14; both landed in the same commit, `07313763`). This header used
//! to say "the feature has no live consumer yet"; that was stale, and it also
//! named the wrong table. Two consumers read `coord.wip_attribution` — the table
//! this loop fills:
//!
//! - `stale_wip_watcher`'s `orphaned_wip` branch. `stale_wip_watcher::spawn` is
//!   called unconditionally during coord startup and `enabled()` is false only
//!   for an explicit `QONTINUI_COORD_STALE_WIP_ENABLED=0`, so it is default-ON;
//!   every cycle runs the orphan branch, leader-gated only.
//! - the `GET /coord/trees/wip-owners/:device_id` ownership join.
//!
//! `coord.primary_trees.dirty_files` is NOT the consumer — it is the *other
//! side* of both joins (the tree publisher's output, which our `file` field is
//! shaped to line up with). The old wording had the direction backwards.
//!
//! **What staying dark actually costs.** coord's `collect_orphan_inputs` reaches
//! `coord.wip_attribution` through an INNER `JOIN`, so while this loop is off the
//! join yields zero rows for every device and the `orphaned_wip` alert **cannot
//! fire at all** — it is not merely quiet. The sibling `stale_wip` branch reads
//! `coord.primary_trees` alone, is unaffected, and has been firing normally
//! throughout; its alerts are therefore not evidence that the orphan half works.
//!
//! So the reason to stay default-off is *not* an absent consumer. It is the
//! unsettled cost/privacy question about parsing every hosted session's
//! transcript on a loop on every fleet host — an operator call, tracked in
//! `qontinui-dev-notes/plans/2026-06-14-coord-wip-authorship-attribution.md`.
//!
//! (coord sites are named by symbol and route rather than by line — line numbers
//! for exactly these sites have already rotted twice. They live in
//! `crates/coord/src/{main,routes,stale_wip_watcher,wip_attribution}.rs`, a path
//! prefix that is itself newer than the citations in that plan.)
//!
//! ## Offset / dedup
//!
//! We keep an in-memory `HashMap<session_id, u64>` of the last-processed byte
//! length per transcript. **On first sight of a transcript we record its
//! current length and process nothing** — anchoring at EOF so capture goes
//! forward only (missing pre-loop history is acceptable, and this avoids
//! re-POSTing the whole transcript on every runner restart). On subsequent
//! cycles we parse only the bytes PAST the stored offset, then advance it.
//! Across cycles the offset prevents re-POSTing; within a cycle dedup is
//! implicit (the offset advances monotonically). coord's upsert is keyed
//! `(device, repo, file)` so even a duplicate POST is idempotent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::terminal::transcript;

/// Is the capture loop enabled? OFF unless `COORD_SESSION_ATTRIBUTION_ENABLED`
/// is an explicit truthy value (`1`/`true`/`yes`, case-insensitive). Mirrors
/// the OFF-by-default executor-gate posture (the inverse of
/// `fleet::pull_executor_enabled`, which is ON-by-default).
fn attribution_enabled() -> bool {
    std::env::var("COORD_SESSION_ATTRIBUTION_ENABLED")
        .ok()
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

/// Per-process map of `claude_session_id -> last-processed transcript byte
/// length`. Lives for the runner's lifetime so the offset survives across
/// cycles. Guarded by a `Mutex` — the loop is single-tasked but the static is
/// shared and a `Mutex` keeps it sound without `unsafe`.
static OFFSETS: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);

/// The wire shape of `POST /coord/wip-attribution`. Matches coord's
/// `WipAttributionIngestRequest` (do NOT change coord). Anonymous device-keyed
/// — coord resolves the tenant server-side from `device_id`, COALESCEs
/// `edited_at` to `now()`, and accumulates `contributors` from `session_id`.
/// `agent_id` / `edited_at` are omitted (coord defaults them); `session_id` is
/// a UUID (coord's field is `Option<Uuid>`) and is omitted when the Claude
/// session id doesn't parse as a UUID.
#[derive(Debug, serde::Serialize)]
struct WipAttributionPayload {
    device_id: uuid::Uuid,
    /// The Claude session UUID (the transcript file's stem). Coord's field is
    /// `Option<Uuid>`, so emit `Some` only when it parses; omit otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<uuid::Uuid>,
    /// Top-level repo dir name under the workspace root (always `qontinui-*`).
    repo: String,
    /// Repo-relative path (joins `coord.primary_trees.dirty_files`).
    file: String,
    /// Phase 8b (plan 2026-07-02-session-scoped-multi-tenant-device-binding,
    /// Phase 8 item 7 / D2 site 12): EXPLICIT tenant attribution from the
    /// owning session's binding — the lifecycle-hosted sessions this
    /// publisher walks are operator terminal sessions, whose binding is the
    /// device DEFAULT. Explicit-wins coord-side once Phase 5's resolver
    /// deploys; today's coord ignores the extra field (serde tolerant).
    /// Omitted when the runner has no resolvable default (coord then
    /// resolves device-side, exactly the pre-8b behavior).
    #[serde(skip_serializing_if = "Option::is_none")]
    tenant_id: Option<uuid::Uuid>,
}

/// The directory name to assume when the workspace root does not resolve.
///
/// Taken from the shared resolver's own last-resort probe
/// (`<home>/qontinui-root`) rather than spelled again here, so the fail-soft
/// default and the resolver can never disagree.
const DEFAULT_WORKSPACE_DIR_NAME: &str = qontinui_types::paths::HOME_WORKSPACE_DIR;

/// The directory names [`map_path_to_repo_file`] anchors on, in priority order:
/// the resolved workspace root's own final segment, then
/// [`DEFAULT_WORKSPACE_DIR_NAME`].
///
/// The marker used to be the hardcoded string `qontinui-root`, which assumed the
/// workspace directory is *named* that. The resolver no longer requires it: any
/// directory holding `qontinui-runner/.git` is a valid root, and `$QONTINUI_ROOT`
/// may point anywhere (plan
/// `2026-08-04-remove-hardcoded-machine-paths-from-product-code`, slice 5
/// Phase 8).
///
/// **Fail-soft when the root is ABSENT and when it is WRONG.** Falling back only
/// on `None` was not enough. `qontinui_types::paths` explicitly permits the root
/// to resolve to a worktree *container*, and `persist_resolved_workspace_root`
/// then makes that answer sticky in `settings.json` — so the derived marker can
/// legitimately be something like `hp`, which no transcript path contains. Every
/// path would then fail to match and the cycle would post zero rows, which reads
/// exactly like "nobody edited anything". Trying the historical default behind
/// the derived name costs one extra string search per path and cannot produce a
/// wrong repo: both markers are matched as whole `/segment/` anchors and the
/// `qontinui-` prefix check still gates the result.
fn workspace_dir_markers() -> Vec<String> {
    let root = crate::workspace_paths::workspace_root();
    markers_from(workspace_dir_name_of(root.as_deref()))
}

/// Pure core of [`workspace_dir_markers`]: the derived name first, the default
/// behind it, never the same name twice.
fn markers_from(derived: Option<String>) -> Vec<String> {
    match derived {
        Some(name) if name != DEFAULT_WORKSPACE_DIR_NAME => {
            vec![name, DEFAULT_WORKSPACE_DIR_NAME.to_string()]
        }
        _ => vec![DEFAULT_WORKSPACE_DIR_NAME.to_string()],
    }
}

/// Pure core of [`workspace_dir_name`]: the root's own final path segment.
/// `None` when there is no root, or when it has no ordinary final component
/// (a filesystem root such as `C:\`, which is never a workspace root anyway).
fn workspace_dir_name_of(root: Option<&Path>) -> Option<String> {
    root.and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
}

/// Map an absolute (or backslash) file path to `(repo, file)` IFF it lives
/// under a `/<workspace_dir>/` segment AND its top-level dir starts with
/// `qontinui-` (the exact set the tree publisher reports, so `file` lines up
/// with `coord.primary_trees.dirty_files`).
///
/// `workspace_dirs` is tried in order and the first marker that yields a match
/// wins; see [`workspace_dir_markers`] for why there is more than one. `None`
/// only when NO marker matches.
///
/// `workspace_dirs` is injected rather than read here, so this stays pure — no
/// IO, no settings read — and unit-testable against a root named anything at
/// all.
pub(crate) fn map_path_to_repo_file(
    file_path: &str,
    workspace_dirs: &[&str],
) -> Option<(String, String)> {
    let normalized = file_path.replace('\\', "/");
    workspace_dirs
        .iter()
        .find_map(|dir| map_normalized_under(&normalized, dir))
}

/// The single-marker rule, against an already slash-normalized path:
///
/// - Finds the `/<workspace_dir>/` segment and takes the remainder.
/// - `repo` = up to the first `/`; `file` = the rest.
/// - Returns `None` when there's no `/<workspace_dir>/` segment, no remainder
///   after the repo, or `repo` doesn't start with `qontinui-` (e.g. a
///   `multistate` repo, or a path outside the monorepo).
fn map_normalized_under(normalized: &str, workspace_dir: &str) -> Option<(String, String)> {
    // Anchor on the `/<workspace_dir>/` segment; take everything after it. The
    // leading `/` ensures we match a path SEGMENT, not a substring inside a dir
    // name. A nested worktree path (e.g.
    // `.../<workspace_dir>/qontinui-runner-wt-foo/src/x.rs`) maps to its own
    // top-level dir (`qontinui-runner-wt-foo`) — that dir is still `qontinui-*`.
    let marker = format!("/{workspace_dir}/");
    let idx = normalized.find(&marker)?;
    let rest = &normalized[idx + marker.len()..];

    let slash = rest.find('/')?;
    let repo = &rest[..slash];
    let file = &rest[slash + 1..];

    if repo.is_empty() || file.is_empty() {
        return None;
    }
    // Only emit for the repos the tree publisher reports.
    if !repo.starts_with("qontinui-") {
        return None;
    }
    Some((repo.to_string(), file.to_string()))
}

/// Parse a slice of newly-appended transcript bytes and extract every
/// Edit/Write/MultiEdit `tool_use` `file_path` that maps to a `qontinui-*`
/// repo. Returns `(repo, file)` pairs in document order.
///
/// Reuses [`transcript::parse_line_for_touched_files`] (which already filters
/// to Edit/Write/MultiEdit and excludes NotebookEdit, and tolerates malformed
/// JSON / non-assistant lines by yielding nothing) then applies
/// [`map_path_to_repo_file`]. Pure (no IO) so it's unit-testable —
/// `workspace_dirs` is injected for the same reason.
pub(crate) fn extract_attributions_from_new_bytes(
    session_id: &str,
    new_bytes: &str,
    workspace_dirs: &[&str],
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in new_bytes.lines() {
        // `parse_line_for_touched_files` returns Err only on malformed JSON;
        // skip those silently (defensive — never fail the cycle).
        let touched = match transcript::parse_line_for_touched_files(session_id, line) {
            Ok(t) => t,
            Err(_) => continue,
        };
        for tf in touched {
            if let Some(pair) = map_path_to_repo_file(&tf.file_path, workspace_dirs) {
                out.push(pair);
            }
        }
    }
    out
}

/// Locate a hosted session's transcript jsonl, given its `config_dir`
/// (`CLAUDE_CONFIG_DIR`) and `working_dir` (the cwd Claude was launched from =
/// the encoded project path). Returns the first existing path:
///
/// - If `config_dir` is known, try that dir directly.
/// - Otherwise (or if that file doesn't exist), scan every known Claude config
///   dir and return the first hit — mirrors `transcript_read_session`'s
///   "try each config dir" fallback.
///
/// Returns `None` if no transcript exists for the session in any config dir.
fn resolve_transcript_path(
    config_dir: Option<&str>,
    working_dir: &str,
    session_id: &str,
) -> Option<PathBuf> {
    if let Some(cfg) = config_dir {
        let p = transcript::session_transcript_path(Path::new(cfg), working_dir, session_id);
        if p.exists() {
            return Some(p);
        }
    }
    // Fallback: scan all known config dirs (same posture as
    // `transcript_read_session` when no config_dir is supplied / matches).
    for dir in transcript::find_claude_config_dirs() {
        let p = transcript::session_transcript_path(&dir, working_dir, session_id);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// One capture pass. Best-effort: per-session errors `warn!`/`debug!` and the
/// pass continues. Returns `Err` only for the terminal "nothing to do this
/// tick" conditions (no device identity, no coord URL) so the caller can log +
/// retry next tick — same shape as `fleet::publish_tree_state`.
pub async fn run_attribution_cycle() -> Result<(), String> {
    // 1. Identity + coord base (skip the cycle when either is unavailable,
    //    exactly like `publish_tree_state`).
    let device = match crate::fleet::load_device_file() {
        Some(d) => d,
        None => {
            info!(
                "session_attribution: ~/.qontinui/machine.json missing — \
                 run `qontinui_profile device init` to enable WIP-attribution \
                 capture. Skipping."
            );
            return Ok(());
        }
    };
    let device_id = match uuid::Uuid::parse_str(&device.device_id) {
        Ok(id) => id,
        Err(e) => {
            warn!(
                "session_attribution: machine.json device_id is not a valid UUID ({e}). Skipping."
            );
            return Ok(());
        }
    };
    let base = match qontinui_runner_lib::profiles::connected_coord_base() {
        Some(b) => b,
        None => {
            info!(
                "session_attribution: runner is ISOLATED (nothing configured and not a \
                 hosted tier) — no coord to attribute to. Skipping."
            );
            return Ok(());
        }
    };

    // 2. Enumerate the sessions this runner hosts. Re-open the durable store
    //    fresh each cycle so we read the latest atomically-persisted state.
    let store = match crate::session::session_lifecycle_store::SessionLifecycleStore::open(
        crate::session::session_lifecycle_store::store_path(),
    ) {
        Ok(s) => s,
        Err(e) => {
            // No store on disk yet (no sessions ever hosted) — nothing to do.
            debug!("session_attribution: lifecycle store open failed ({e}); skipping cycle");
            return Ok(());
        }
    };
    let open = store.open_records();
    if open.is_empty() {
        return Ok(());
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| format!("reqwest builder: {e}"))?;
    let url = format!("{base}/coord/wip-attribution");

    // Phase 8b item 7 — explicit tenant attribution. Resolved once per
    // cycle: every lifecycle-hosted session is an operator terminal session
    // running under the device's DEFAULT binding (coord-spawned agent
    // sessions publish through their own agent identity, not this walker).
    let tenant_id = crate::fleet::resolve_tenant_id();

    // The workspace directory names every transcript path is anchored on.
    // Resolved ONCE per cycle, next to the tenant, rather than per file path:
    // it is a cycle-level property, and `workspace_paths` is deliberately not
    // memoized (the setting is operator-editable at runtime).
    let workspace_dirs = workspace_dir_markers();
    let workspace_dirs: Vec<&str> = workspace_dirs.iter().map(String::as_str).collect();

    let mut posted = 0usize;
    for rec in open {
        let session_id = rec.claude_session_id;
        // A hosted session must know its cwd to locate the transcript.
        let working_dir = match rec.working_dir.as_deref() {
            Some(w) if !w.is_empty() => w,
            _ => {
                debug!("session_attribution: session {session_id} has no working_dir; skipping");
                continue;
            }
        };

        let transcript_path =
            match resolve_transcript_path(rec.config_dir.as_deref(), working_dir, &session_id) {
                Some(p) => p,
                None => {
                    debug!(
                    "session_attribution: no transcript found for session {session_id}; skipping"
                );
                    continue;
                }
            };

        // Current transcript length.
        let len = match std::fs::metadata(&transcript_path) {
            Ok(m) => m.len(),
            Err(e) => {
                debug!(
                    "session_attribution: stat {} failed ({e}); skipping",
                    transcript_path.display()
                );
                continue;
            }
        };

        // Read + update the offset for this session.
        let prev_offset = {
            let mut guard = match OFFSETS.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            let map = guard.get_or_insert_with(HashMap::new);
            match map.get(&session_id).copied() {
                // First sight: anchor at EOF and process nothing this cycle.
                None => {
                    map.insert(session_id.clone(), len);
                    None
                }
                Some(off) => Some(off),
            }
        };
        let prev_offset = match prev_offset {
            Some(o) => o,
            None => continue, // first sight — anchored, nothing to process
        };

        // Transcript shrank (rotation / truncation / re-pin) — re-anchor at the
        // new EOF and skip; don't try to read a negative/garbage range.
        if len < prev_offset {
            let mut guard = match OFFSETS.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            if let Some(map) = guard.as_mut() {
                map.insert(session_id.clone(), len);
            }
            continue;
        }
        if len == prev_offset {
            continue; // no new bytes
        }

        // Read only the newly-appended bytes [prev_offset, len).
        let new_bytes = match read_range(&transcript_path, prev_offset, len) {
            Ok(b) => b,
            Err(e) => {
                debug!(
                    "session_attribution: read range for {} failed ({e}); skipping",
                    transcript_path.display()
                );
                continue;
            }
        };

        // Advance the offset BEFORE POSTing so a POST failure can't cause a
        // re-read of the same bytes next cycle (coord's upsert is idempotent;
        // forward-only is the contract).
        {
            let mut guard = match OFFSETS.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            if let Some(map) = guard.as_mut() {
                map.insert(session_id.clone(), len);
            }
        }

        // coord's `session_id` is `Option<Uuid>`; the Claude session id is a
        // UUID. Parse once — omit when it doesn't (older/odd ids still attribute
        // the file by device, just without a last-writer session).
        let session_uuid = uuid::Uuid::parse_str(&session_id).ok();

        for (repo, file) in
            extract_attributions_from_new_bytes(&session_id, &new_bytes, &workspace_dirs)
        {
            let payload = WipAttributionPayload {
                device_id,
                session_id: session_uuid,
                repo,
                file,
                tenant_id,
            };
            // Tenant-scoped: `payload.tenant_id` is this device's resolved
            // binding and coord attributes the row under it, so the bearer is
            // selected from the same tenant's slot. A slot miss sends
            // unauthenticated (never another tenant's JWT) — see
            // `auth::select_device_bearer`.
            match crate::auth::attach_device_auth_for(
                client.post(&url).json(&payload),
                tenant_id.as_ref(),
            )
            .send()
            .await
            {
                Ok(resp) if resp.status().is_success() => {
                    posted += 1;
                }
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let excerpt: String = body.chars().take(200).collect();
                    warn!(
                        "session_attribution: coord returned {status} for {}/{}: {excerpt}",
                        payload.repo, payload.file
                    );
                }
                Err(e) => {
                    warn!(
                        "session_attribution: POST {url} for {}/{} failed: {e}",
                        payload.repo, payload.file
                    );
                }
            }
        }
    }

    if posted > 0 {
        debug!("session_attribution: posted {posted} attribution(s) device_id={device_id}");
    }
    Ok(())
}

/// Read the byte range `[start, end)` of `path` as a lossy UTF-8 string. The
/// transcript is line-delimited JSON; `start`/`end` are always whole-file
/// offsets we recorded (EOF lengths), so the slice begins/ends on line
/// boundaries in practice. Lossy conversion tolerates any mid-character seek
/// defensively.
fn read_range(path: &Path, start: u64, end: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(start))?;
    let to_read = (end - start) as usize;
    let mut buf = vec![0u8; to_read];
    // Read up to `to_read` bytes; the file may have grown further since `stat`
    // (a concurrent append) — that's fine, we only take what we sized for.
    let n = read_full_or_eof(&mut f, &mut buf)?;
    buf.truncate(n);
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Read into `buf` until it's full or EOF. Returns the number of bytes read.
fn read_full_or_eof(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    use std::io::Read;
    let mut filled = 0usize;
    while filled < buf.len() {
        match f.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Spawn the periodic WIP-attribution capture task on the ambient tokio
/// runtime. Wired next to [`crate::fleet::spawn_tree_publisher`].
///
/// Gated OFF by default (`COORD_SESSION_ATTRIBUTION_ENABLED`): logs "disabled"
/// once and returns without spawning when off.
///
/// Interval read from `COORD_SESSION_ATTRIBUTION_INTERVAL_SECS` (default 45s,
/// floored at 5s). `MissedTickBehavior::Skip` mirrors `spawn_tree_publisher`.
/// Failures `warn!` and retry on the next tick; the loop never panics.
pub fn spawn_session_attribution() {
    if !attribution_enabled() {
        info!(
            "session_attribution: disabled (set COORD_SESSION_ATTRIBUTION_ENABLED=1 to enable). \
             Not spawning capture loop."
        );
        return;
    }

    let secs: u64 = std::env::var("COORD_SESSION_ATTRIBUTION_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(45)
        .max(5);

    info!(
        "session_attribution: starting periodic WIP-attribution capture, interval={}s",
        secs
    );

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Err(e) = run_attribution_cycle().await {
                warn!("session_attribution: {e}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- WipAttributionPayload wire shape (Phase 8b item 7) -------------------

    #[test]
    fn wip_payload_serializes_explicit_tenant_and_omits_none() {
        let t = uuid::Uuid::from_bytes([0x77; 16]);
        let with_tenant = WipAttributionPayload {
            device_id: uuid::Uuid::nil(),
            session_id: None,
            repo: "qontinui-runner".into(),
            file: "src/main.rs".into(),
            tenant_id: Some(t),
        };
        let v = serde_json::to_value(&with_tenant).unwrap();
        assert_eq!(
            v.get("tenant_id").and_then(|x| x.as_str()),
            Some(t.to_string().as_str()),
            "explicit tenant must appear on the WIP wire"
        );

        let without = WipAttributionPayload {
            device_id: uuid::Uuid::nil(),
            session_id: None,
            repo: "qontinui-runner".into(),
            file: "src/main.rs".into(),
            tenant_id: None,
        };
        let v = serde_json::to_value(&without).unwrap();
        assert!(
            v.get("tenant_id").is_none(),
            "None tenant must be omitted (pre-8b wire shape preserved)"
        );
    }

    // ---- map_path_to_repo_file ------------------------------------------------
    //
    // The workspace directory name is INJECTED into every case below. These
    // assert a string transformation, so they read the same as before the
    // marker stopped being hardcoded (plan
    // `2026-08-04-remove-hardcoded-machine-paths-from-product-code`, slice 5
    // Phase 8) — but they no longer depend on how the machine running the suite
    // is configured.

    /// The historical directory name, still the fail-soft default.
    const WS: &[&str] = &["qontinui-root"];

    #[test]
    fn maps_forward_slash_qontinui_path() {
        assert_eq!(
            map_path_to_repo_file("D:/qontinui-root/qontinui-coord/src/routes.rs", WS),
            Some(("qontinui-coord".to_string(), "src/routes.rs".to_string()))
        );
    }

    #[test]
    fn maps_backslash_variant() {
        assert_eq!(
            map_path_to_repo_file("D:\\qontinui-root\\qontinui-coord\\src\\routes.rs", WS),
            Some(("qontinui-coord".to_string(), "src/routes.rs".to_string()))
        );
    }

    #[test]
    fn skips_path_outside_qontinui_root() {
        assert_eq!(
            map_path_to_repo_file("C:/Users/jspin/Documents/other/file.rs", WS),
            None
        );
    }

    #[test]
    fn skips_non_qontinui_repo() {
        // A repo under qontinui-root that doesn't start with `qontinui-`
        // (e.g. `multistate`) is NOT reported by the tree publisher → skip.
        assert_eq!(
            map_path_to_repo_file("D:/qontinui-root/multistate/src/lib.rs", WS),
            None
        );
    }

    #[test]
    fn nested_worktree_maps_to_its_top_level_dir() {
        // A nested worktree path maps to its own top-level dir name (which is
        // still `qontinui-*`), not the inner repo.
        assert_eq!(
            map_path_to_repo_file(
                "D:/qontinui-root/qontinui-runner-wt-attrcap/src-tauri/src/fleet.rs",
                WS
            ),
            Some((
                "qontinui-runner-wt-attrcap".to_string(),
                "src-tauri/src/fleet.rs".to_string()
            ))
        );
    }

    #[test]
    fn skips_repo_with_no_file_remainder() {
        // `/qontinui-root/qontinui-coord` with no trailing file → None.
        assert_eq!(
            map_path_to_repo_file("D:/qontinui-root/qontinui-coord", WS),
            None
        );
        assert_eq!(
            map_path_to_repo_file("D:/qontinui-root/qontinui-coord/", WS),
            None
        );
    }

    // ---- the marker is derived, not assumed -----------------------------------

    /// The whole point of the change: a workspace root that is NOT named
    /// `qontinui-root` still attributes. Before this, every edit on such a box
    /// mapped to `None` and the machine published nothing — a silent, total
    /// loss of attribution that looked exactly like "this session edited
    /// nothing".
    #[test]
    fn maps_a_workspace_root_named_something_other_than_qontinui_root() {
        assert_eq!(
            map_path_to_repo_file(
                "/srv/checkouts/qontinui-coord/src/routes.rs",
                &["checkouts"]
            ),
            Some(("qontinui-coord".to_string(), "src/routes.rs".to_string()))
        );
        assert_eq!(
            map_path_to_repo_file(
                "E:\\work\\qontinui-runner\\src-tauri\\src\\fleet.rs",
                &["work"]
            ),
            Some((
                "qontinui-runner".to_string(),
                "src-tauri/src/fleet.rs".to_string()
            ))
        );
        // ...and the old hardcoded name is no longer privileged: given ONLY a
        // root named `work`, a `qontinui-root` segment is just another directory.
        assert_eq!(
            map_path_to_repo_file("/srv/checkouts/qontinui-coord/src/routes.rs", &["work"]),
            None
        );
    }

    /// The marker is the root's own final segment.
    #[test]
    fn workspace_dir_name_is_the_roots_final_segment() {
        assert_eq!(
            workspace_dir_name_of(Some(Path::new("/srv/checkouts"))).as_deref(),
            Some("checkouts")
        );
        assert_eq!(
            workspace_dir_name_of(Some(Path::new("/srv/checkouts/"))).as_deref(),
            Some("checkouts"),
            "a trailing separator must not change the answer"
        );
    }

    /// Fail-soft: no resolved root (and no usable final segment) yields no
    /// derived name, so the caller keeps the historical default rather than
    /// dropping every attribution row.
    #[test]
    fn workspace_dir_name_degrades_rather_than_inventing_one() {
        assert_eq!(workspace_dir_name_of(None), None);
        // A filesystem root has no ordinary final component.
        assert_eq!(workspace_dir_name_of(Some(Path::new("/"))), None);
        assert_eq!(DEFAULT_WORKSPACE_DIR_NAME, "qontinui-root");
        // …and the marker list an unresolved root produces is exactly the
        // historical default, so the pre-Phase-8 behaviour is reproduced.
        assert_eq!(markers_from(None), vec!["qontinui-root".to_string()]);
    }

    /// The MIS-resolved case, which falling back only on `None` did not cover.
    ///
    /// `qontinui_types::paths` may legitimately resolve the root to a worktree
    /// container, and `persist_resolved_workspace_root` then makes that answer
    /// sticky in `settings.json`. The derived marker is then a segment no
    /// transcript path contains, every path fails to match, and the cycle posts
    /// zero rows — indistinguishable from "nobody edited anything". Trying the
    /// default behind the derived name is what keeps that box attributing.
    #[test]
    fn a_misresolved_root_falls_back_to_the_default_marker() {
        let markers = markers_from(Some("hp".to_string()));
        assert_eq!(
            markers,
            vec!["hp".to_string(), "qontinui-root".to_string()],
            "the derived name is tried first, the default behind it"
        );
        let markers: Vec<&str> = markers.iter().map(String::as_str).collect();

        // The derived marker alone matches nothing…
        let edited = "/srv/qontinui-root/qontinui-coord/src/routes.rs";
        assert_eq!(map_path_to_repo_file(edited, &["hp"]), None);
        // …and with the default behind it the row is attributed after all.
        assert_eq!(
            map_path_to_repo_file(edited, &markers),
            Some(("qontinui-coord".to_string(), "src/routes.rs".to_string()))
        );
    }

    /// Order is priority order: when BOTH markers match a path, the derived one
    /// decides, so a correctly-resolved root is never overridden by the default.
    #[test]
    fn the_derived_marker_wins_when_both_match() {
        assert_eq!(
            map_path_to_repo_file(
                "/srv/qontinui-root/checkouts/qontinui-coord/src/routes.rs",
                &["checkouts", "qontinui-root"]
            ),
            Some(("qontinui-coord".to_string(), "src/routes.rs".to_string())),
            "the derived marker `checkouts` must decide; `qontinui-root` would \
             have yielded the non-`qontinui-` dir `checkouts` and dropped the row"
        );
    }

    /// A root that IS named `qontinui-root` must not list the same marker twice.
    #[test]
    fn the_default_is_not_duplicated_when_the_root_is_named_that() {
        assert_eq!(
            markers_from(Some("qontinui-root".to_string())),
            vec!["qontinui-root".to_string()]
        );
    }

    // ---- extract_attributions_from_new_bytes ----------------------------------

    fn assistant_edit_line(file_path: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "uuid": "u1",
            "timestamp": "2026-06-14T00:00:00Z",
            "message": {
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "editing"},
                    {"type": "tool_use", "id": "t1", "name": "Edit",
                     "input": {"file_path": file_path}}
                ]
            }
        })
        .to_string()
    }

    #[test]
    fn extract_yields_edit_file_path() {
        let line = assistant_edit_line("D:/qontinui-root/qontinui-coord/src/routes.rs");
        let got = extract_attributions_from_new_bytes("sess-1", &line, WS);
        assert_eq!(
            got,
            vec![("qontinui-coord".to_string(), "src/routes.rs".to_string())]
        );
    }

    #[test]
    fn extract_skips_notebook_edit() {
        // NotebookEdit is NOT one of Edit/Write/MultiEdit → yields nothing.
        let line = serde_json::json!({
            "type": "assistant",
            "uuid": "u1",
            "timestamp": "2026-06-14T00:00:00Z",
            "message": {
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "t1", "name": "NotebookEdit",
                     "input": {"file_path": "D:/qontinui-root/qontinui-coord/nb.ipynb"}}
                ]
            }
        })
        .to_string();
        assert!(extract_attributions_from_new_bytes("sess-1", &line, WS).is_empty());
    }

    #[test]
    fn extract_skips_tool_result_and_user_lines() {
        // A user/tool_result line yields nothing.
        let user = serde_json::json!({
            "type": "user",
            "uuid": "u1",
            "timestamp": "2026-06-14T00:00:00Z",
            "message": {
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "ok"}
                ]
            }
        })
        .to_string();
        assert!(extract_attributions_from_new_bytes("sess-1", &user, WS).is_empty());
    }

    #[test]
    fn extract_skips_malformed_json_without_panicking() {
        assert!(extract_attributions_from_new_bytes("sess-1", "{not valid json", WS).is_empty());
        // Mixed: one malformed line + one good Edit line → only the good one.
        let good = assistant_edit_line("D:/qontinui-root/qontinui-runner/x.rs");
        let mixed = format!("{{garbage\n{good}\n");
        assert_eq!(
            extract_attributions_from_new_bytes("sess-1", &mixed, WS),
            vec![("qontinui-runner".to_string(), "x.rs".to_string())]
        );
    }

    #[test]
    fn extract_skips_edit_outside_qontinui_root() {
        let line = assistant_edit_line("C:/tmp/scratch.rs");
        assert!(extract_attributions_from_new_bytes("sess-1", &line, WS).is_empty());
    }

    #[test]
    fn extract_handles_write_and_multiedit() {
        for tool in ["Write", "MultiEdit"] {
            let line = serde_json::json!({
                "type": "assistant",
                "uuid": "u1",
                "timestamp": "2026-06-14T00:00:00Z",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "tool_use", "id": "t1", "name": tool,
                         "input": {"file_path": "D:/qontinui-root/qontinui-web/app.tsx"}}
                    ]
                }
            })
            .to_string();
            assert_eq!(
                extract_attributions_from_new_bytes("sess-1", &line, WS),
                vec![("qontinui-web".to_string(), "app.tsx".to_string())],
                "tool {tool} should be captured"
            );
        }
    }
}
