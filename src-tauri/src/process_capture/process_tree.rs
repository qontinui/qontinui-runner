//! Process-tree discovery for managed processes.
//!
//! The runner spawns dev services through deep wrapper chains
//! (`cmd → poetry → python → uvicorn → uvicorn worker` on Windows).
//! `tokio::process::Child::id()` only knows the outer wrapper; the
//! service-bearing PID is several hops down. This module enumerates the
//! descendant tree of a spawned PID and identifies which descendant is the
//! "service" — the inner PID whose liveness determines whether the managed
//! process is doing its job. Phase 1 populates `ProcessRuntime.descendant_pids`
//! / `service_pid`; Phase 2 reads `service_pid` to detect inner-worker death.
//!
//! Windows uses `Get-CimInstance Win32_Process` via PowerShell to snapshot
//! `(ProcessId, ParentProcessId, CreationDate, Name)` for every running
//! process, then BFS in Rust. Unix reads `/proc/<pid>/stat` for the parent and
//! start time and `/proc/<pid>/comm` for the image name. The `Name`/`comm`
//! image powers [`claude_present_in_inclusive_subtree`], the session-liveness
//! signal.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A descendant PID + its creation time, used by Phase 4 to detect PID
/// reuse across runner restarts (the kernel recycles PIDs aggressively on
/// Windows; a long-running orphan's PID may belong to a brand-new unrelated
/// process by the next runner startup).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct PidWithSpawnedAt {
    pub pid: u32,
    /// Process creation time as Unix epoch seconds. `0` if the platform
    /// helper failed to resolve it (e.g. permission-denied on `/proc/<pid>/stat`).
    pub spawned_at_unix: i64,
}

/// Snapshot of the process table relevant to one discovery pass.
///
/// `parent_map[parent_pid] -> [child_pid, ...]` is the BFS index;
/// `creation_times[pid] -> unix_secs` carries the lookup table built from
/// the same snapshot so Phase 4 can sanity-filter persisted PIDs without
/// re-querying WMI; `names[pid] -> image_name` carries each process's image
/// (e.g. `claude.exe` on Windows, `comm` on Unix) so the session liveness
/// poll can ask "is a live Claude process present in this subtree?" — the
/// signal [`claude_present_in_inclusive_subtree`] reads.
#[derive(Debug, Default)]
pub struct ProcessSnapshot {
    pub parent_map: HashMap<u32, Vec<u32>>,
    pub creation_times: HashMap<u32, i64>,
    pub names: HashMap<u32, String>,
}

/// Skew tolerance (millis) for the PID-reuse creation-time vs the reference
/// instant comparison in [`claude_present_in_inclusive_subtree`]. Creation
/// times come from WMI at second granularity, so a beat of rounding slack is
/// always allowed before treating a tracked PID as reused.
const PID_REUSE_SKEW_MS: i64 = 5_000;

/// True iff a live Claude process is present in the **inclusive** subtree
/// rooted at `root_pid` — i.e. `root_pid`'s own image is `claude*`, OR any
/// descendant's image is `claude*`. This is the liveness signal the terminal-
/// session poll uses: a session is alive iff there is a live Claude here, which
/// is the correct question for **both** session shapes:
/// - **Operator session:** tracked PID is the shell; `claude` is a child →
///   present via a descendant.
/// - **Agent / gate-continuation session:** tracked PID *is* `claude` (it is the
///   portable-pty child) → present via the root, even when idle with zero
///   children (the bug the old `descendant_count > 0` heuristic mis-closed).
///
/// `reference_unix_millis` guards PID reuse: if the tracked `root_pid`'s
/// creation time predates the reference instant (beyond [`PID_REUSE_SKEW_MS`]),
/// the PID cannot belong to anything this reference instant's process tree
/// spawned — its image name must not be trusted, so the subtree is treated as
/// claude-absent. The comparison normalizes units: `creation_times` is epoch
/// **seconds**, `reference_unix_millis` is epoch **millis**.
///
/// **Pass the runner primary's own boot time here — NEVER a per-session-record
/// timestamp like `opened_at`.** (Regression found live 2026-07-03, Phase 1
/// verification of PR #630: a session record's `terminal_id` can legitimately
/// be reused by a *later* record against an *already-running* terminal — e.g.
/// a reconnect into an existing pane — since `record_open` dedups by
/// `claude_session_id`, never by `terminal_id`. When that happens the tracked
/// PID's real creation time predates the newer record's `opened_at` by design,
/// not because of PID recycling, and the old `opened_at`-based guard falsely
/// concluded "reused" — flipping a genuinely live, idle session to `poll-dead`
/// after `LIVE_SHELL_DEAD_TICKS`. A PID cannot predate the primary's OWN boot
/// time and still belong to a terminal this boot's `TerminalManager` spawned,
/// so the boot start is the correct, restart-scoped reference instant: it
/// still catches the genuine cross-restart PID-recycle case the guard exists
/// for, without misfiring on ordinary same-boot terminal reuse.
pub fn claude_present_in_inclusive_subtree(
    root_pid: u32,
    snapshot: &ProcessSnapshot,
    reference_unix_millis: i64,
) -> bool {
    // PID-reuse guard on the tracked root (seconds → millis before comparing).
    if let Some(&created_secs) = snapshot.creation_times.get(&root_pid) {
        if created_secs > 0 && created_secs * 1000 + PID_REUSE_SKEW_MS < reference_unix_millis {
            return false;
        }
    }
    if is_claude_image(snapshot.names.get(&root_pid)) {
        return true;
    }
    bfs_descendants_from(root_pid, &snapshot.parent_map, &snapshot.creation_times)
        .iter()
        .any(|d| is_claude_image(snapshot.names.get(&d.pid)))
}

/// Every claude-image PID in the **inclusive** subtree rooted at `root_pid`
/// (the root itself is included when its image is `claude*`). Same walk as
/// [`claude_present_in_inclusive_subtree`] but returning the PIDs instead of
/// a bool — the session-tracking health check
/// ([`crate::session::tracking_health`]) uses it to cross-reference live
/// Claude processes against the durable lifecycle records. No PID-reuse
/// guard here: this is an enumeration, not a liveness verdict — callers that
/// need the guard pair it with `claude_present_in_inclusive_subtree`.
pub fn claude_pids_in_inclusive_subtree(root_pid: u32, snapshot: &ProcessSnapshot) -> Vec<u32> {
    let mut out = Vec::new();
    if is_claude_image(snapshot.names.get(&root_pid)) {
        out.push(root_pid);
    }
    out.extend(
        bfs_descendants_from(root_pid, &snapshot.parent_map, &snapshot.creation_times)
            .iter()
            .filter(|d| is_claude_image(snapshot.names.get(&d.pid)))
            .map(|d| d.pid),
    );
    out
}

/// The claude-image PID in the **inclusive** subtree rooted at `root_pid` with
/// the EARLIEST KNOWN creation time, returned as `(pid, creation_secs)`.
///
/// "Anchor" because this pid's process-start is the correlation anchor the
/// launch-agnostic session binder uses (its `--session-id` cmdline + its start
/// time vs a transcript's first event). When several claude images live in the
/// subtree (a resume relaunched claude under the same shell), the OLDEST is the
/// session that has been running — the one whose transcript we want. Creation
/// times are second-granular and may be unknown (`0`, e.g. a WMI/`/proc` miss);
/// a KNOWN (`>0`) time always sorts before an unknown one, and unknowns sort
/// last so a resolvable anchor is preferred. Returns `None` when the subtree
/// hosts no claude image (built on [`claude_pids_in_inclusive_subtree`]).
pub fn claude_anchor_in_subtree(root_pid: u32, snapshot: &ProcessSnapshot) -> Option<(u32, i64)> {
    claude_pids_in_inclusive_subtree(root_pid, snapshot)
        .into_iter()
        .map(|pid| (pid, snapshot.creation_times.get(&pid).copied().unwrap_or(0)))
        .min_by(|(_, a), (_, b)| {
            // Prefer the earliest KNOWN creation; unknown (`<= 0`) sorts last.
            let ka = if *a > 0 { *a } else { i64::MAX };
            let kb = if *b > 0 { *b } else { i64::MAX };
            ka.cmp(&kb)
        })
}

/// Extract the `--session-id` value from a process command line, returning it
/// ONLY when it is UUID-shaped (8-4-4-4-12 hex, case-insensitive). Handles the
/// three spellings the provider CLI accepts:
///   - `--session-id <uuid>`  (space-separated)
///   - `--session-id=<uuid>`  (equals)
///   - `--session-id "<uuid>"` / `--session-id='<uuid>'` (quoted)
/// The UUID-shape gate is the safety net: a stray token that merely follows the
/// flag text (or a `--session-idX` false prefix) can never be mistaken for a
/// real id. `None` when the flag is absent or its value isn't UUID-shaped.
pub fn parse_session_id_from_cmdline(cmdline: &str) -> Option<String> {
    const NEEDLE: &str = "--session-id";
    let mut search_from = 0usize;
    while let Some(rel) = cmdline[search_from..].find(NEEDLE) {
        let idx = search_from + rel;
        let after = &cmdline[idx + NEEDLE.len()..];
        search_from = idx + NEEDLE.len();
        // Accept `=value` or ` value`; reject a glued `--session-idX` prefix.
        let value_part = if let Some(eq) = after.strip_prefix('=') {
            eq
        } else if after.starts_with(char::is_whitespace) || after.is_empty() {
            after
        } else {
            continue;
        };
        let tok = extract_first_token(value_part).trim_matches(|c| c == '"' || c == '\'');
        if is_uuid_shaped(tok) {
            return Some(tok.to_string());
        }
    }
    None
}

/// First whitespace-delimited token of `s`, leading whitespace/quote trimmed.
fn extract_first_token(s: &str) -> &str {
    let s = s.trim_start();
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    &s[..end]
}

/// True iff `s` is a canonical 8-4-4-4-12 hex UUID (case-insensitive).
fn is_uuid_shaped(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    let lens = [8usize, 4, 4, 4, 12];
    if parts.len() != lens.len() {
        return false;
    }
    parts
        .iter()
        .zip(lens.iter())
        .all(|(p, &l)| p.len() == l && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Case-insensitive basename match on `claude` / `claude.exe`. Tolerates a
/// path-qualified name (takes the basename) and a trailing `.exe`.
fn is_claude_image(name: Option<&String>) -> bool {
    let Some(name) = name else {
        return false;
    };
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();
    let stem = base.strip_suffix(".exe").unwrap_or(&base);
    stem == "claude"
}

/// Discover every descendant of `root_pid` (NOT including the root itself).
///
/// Returns `(descendants, parent_map)`. The parent map is restricted to the
/// system-wide snapshot — `identify_service_pid` only walks the descendant
/// subset, but the full map is useful for §3.5 sibling-tree reasoning.
///
/// Returns empty vectors on platform helper failure (logged at `warn!`).
pub async fn discover_descendants_with_parent_map(
    root_pid: u32,
) -> (Vec<PidWithSpawnedAt>, HashMap<u32, Vec<u32>>) {
    let snapshot = snapshot_process_table().await;
    let descendants = collect_descendants_from_snapshot(root_pid, &snapshot);
    (descendants, snapshot.parent_map)
}

/// Take a fresh process-table snapshot — exposed for `orphan_state` so the
/// startup reclaim pass can do its PID-reuse check without re-querying.
pub async fn snapshot_process_table_public() -> ProcessSnapshot {
    snapshot_process_table().await
}

/// Convenience wrapper: discover descendants only.
pub async fn discover_descendants(root_pid: u32) -> Vec<PidWithSpawnedAt> {
    discover_descendants_with_parent_map(root_pid).await.0
}

/// Identify which descendant is "the service" — port owner → leaf →
/// fallback to `spawned_pid`. See module-level docs for rules.
pub fn identify_service_pid(
    descendants: &[PidWithSpawnedAt],
    parent_map: &HashMap<u32, Vec<u32>>,
    health_port: Option<u16>,
    spawned_pid: u32,
) -> Option<u32> {
    identify_service_pid_with_lookup(descendants, parent_map, health_port, spawned_pid, |port| {
        port_owner_pid(port)
    })
}

/// Pure version with an injectable port-owner lookup; used by unit tests.
pub fn identify_service_pid_with_lookup<F>(
    descendants: &[PidWithSpawnedAt],
    parent_map: &HashMap<u32, Vec<u32>>,
    health_port: Option<u16>,
    spawned_pid: u32,
    port_owner_lookup: F,
) -> Option<u32>
where
    F: Fn(u16) -> Option<u32>,
{
    // Rule 1: port owner wins, but only if that owner is in our tracked tree
    // (otherwise some unrelated process happens to bind the port and we'd
    // incorrectly link it as ours).
    if let Some(port) = health_port {
        if let Some(owner) = port_owner_lookup(port) {
            if descendants.iter().any(|d| d.pid == owner) || owner == spawned_pid {
                return Some(owner);
            }
        }
    }

    // Rule 2: leaf wins. The deepest descendant with no children in the
    // tracked tree is the service. Walk every descendant; the candidate is
    // the one whose subtree-depth from spawned_pid is greatest among those
    // with zero children in `parent_map`.
    if !descendants.is_empty() {
        let descendant_set: std::collections::HashSet<u32> =
            descendants.iter().map(|d| d.pid).collect();
        let mut best: Option<(u32, usize)> = None;
        for d in descendants {
            let has_tracked_children = parent_map
                .get(&d.pid)
                .map(|kids| kids.iter().any(|k| descendant_set.contains(k)))
                .unwrap_or(false);
            if has_tracked_children {
                continue;
            }
            let depth = depth_from_root(d.pid, spawned_pid, parent_map);
            if best.map(|(_, bd)| depth > bd).unwrap_or(true) {
                best = Some((d.pid, depth));
            }
        }
        if let Some((leaf, _)) = best {
            return Some(leaf);
        }
    }

    // Rule 3: fallback. The outer spawned PID is "the service" by default —
    // equivalent to today's behavior before this module existed.
    Some(spawned_pid)
}

/// Compute the depth of `pid` from `root` by walking up the inverted parent
/// map. Used by the leaf-wins rule when multiple leaves exist (deepest wins).
fn depth_from_root(pid: u32, root: u32, parent_map: &HashMap<u32, Vec<u32>>) -> usize {
    // Invert parent_map: child -> parent. (Cheap because we only do this
    // for the candidate set, which is small.)
    let mut parent_of: HashMap<u32, u32> = HashMap::new();
    for (parent, kids) in parent_map.iter() {
        for k in kids {
            parent_of.insert(*k, *parent);
        }
    }
    let mut cur = pid;
    let mut depth = 0;
    while let Some(&p) = parent_of.get(&cur) {
        depth += 1;
        if p == root {
            return depth;
        }
        cur = p;
        if depth > 64 {
            // Cycle guard — process trees on real systems are shallow.
            break;
        }
    }
    depth
}

/// BFS from `root` over `parent_map`, returning every descendant (NOT the
/// root). Pure helper extracted for unit testing.
fn bfs_descendants_from(
    root: u32,
    parent_map: &HashMap<u32, Vec<u32>>,
    creation_times: &HashMap<u32, i64>,
) -> Vec<PidWithSpawnedAt> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    seen.insert(root);
    let mut queue = std::collections::VecDeque::new();
    if let Some(kids) = parent_map.get(&root) {
        for k in kids {
            queue.push_back(*k);
        }
    }
    while let Some(pid) = queue.pop_front() {
        if !seen.insert(pid) {
            continue;
        }
        out.push(PidWithSpawnedAt {
            pid,
            spawned_at_unix: creation_times.get(&pid).copied().unwrap_or(0),
        });
        if let Some(kids) = parent_map.get(&pid) {
            for k in kids {
                queue.push_back(*k);
            }
        }
    }
    out
}

fn collect_descendants_from_snapshot(
    root: u32,
    snapshot: &ProcessSnapshot,
) -> Vec<PidWithSpawnedAt> {
    bfs_descendants_from(root, &snapshot.parent_map, &snapshot.creation_times)
}

// ============================================================================
// Windows implementation
// ============================================================================

#[cfg(windows)]
async fn snapshot_process_table() -> ProcessSnapshot {
    // ConvertTo-Json on a single row drops the array; force an array with @().
    const SCRIPT: &str = "$ErrorActionPreference='SilentlyContinue'; \
        @(Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,CreationDate,Name) | \
        ConvertTo-Json -Compress -Depth 3";

    let output = match tokio::task::spawn_blocking(|| {
        crate::process_helpers::no_window("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
            .output()
    })
    .await
    {
        Ok(Ok(o)) if o.status.success() => o,
        Ok(Ok(o)) => {
            tracing::warn!(
                "process_tree: PowerShell snapshot failed (status={:?}, stderr={})",
                o.status,
                String::from_utf8_lossy(&o.stderr)
            );
            return ProcessSnapshot::default();
        }
        Ok(Err(e)) => {
            tracing::warn!("process_tree: PowerShell snapshot spawn error: {e}");
            return ProcessSnapshot::default();
        }
        Err(e) => {
            tracing::warn!("process_tree: PowerShell snapshot join error: {e}");
            return ProcessSnapshot::default();
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_powershell_snapshot(&stdout)
}

#[cfg(windows)]
#[derive(Debug, Deserialize)]
struct WmiProcessRow {
    #[serde(rename = "ProcessId")]
    process_id: u32,
    #[serde(rename = "ParentProcessId", default)]
    parent_process_id: Option<u32>,
    #[serde(rename = "CreationDate", default)]
    creation_date: Option<serde_json::Value>,
    #[serde(rename = "Name", default)]
    name: Option<String>,
}

#[cfg(windows)]
fn parse_powershell_snapshot(json: &str) -> ProcessSnapshot {
    let rows: Vec<WmiProcessRow> = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("process_tree: failed to parse WMI JSON: {e}");
            return ProcessSnapshot::default();
        }
    };

    let mut snap = ProcessSnapshot::default();
    for row in rows {
        let parent = row.parent_process_id.unwrap_or(0);
        snap.parent_map
            .entry(parent)
            .or_default()
            .push(row.process_id);
        let secs = row
            .creation_date
            .as_ref()
            .map(parse_wmi_creation_date)
            .unwrap_or(0);
        snap.creation_times.insert(row.process_id, secs);
        if let Some(name) = row.name {
            snap.names.insert(row.process_id, name);
        }
    }
    snap
}

/// CIM/WMI `CreationDate` via `ConvertTo-Json` comes through as either:
///   - a string like `/Date(1715911300000)/` (older PowerShell encoders),
///   - a string like `2026-05-16T18:50:06.123Z` (newer encoders), or
///   - an object containing `value`/`DateTime` fields.
/// Return Unix epoch seconds, or 0 on parse failure.
#[cfg(windows)]
fn parse_wmi_creation_date(v: &serde_json::Value) -> i64 {
    use chrono::{DateTime, Utc};

    if let Some(s) = v.as_str() {
        // /Date(ms)/ or /Date(ms+ZZZZ)/
        if let Some(rest) = s.strip_prefix("/Date(") {
            if let Some(num) = rest.split(['+', '-', ')']).next() {
                if let Ok(ms) = num.parse::<i64>() {
                    return ms / 1000;
                }
            }
        }
        // ISO-8601
        if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
            return dt.timestamp();
        }
        if let Ok(dt) = s.parse::<DateTime<Utc>>() {
            return dt.timestamp();
        }
    }
    0
}

/// TARGETED command-line lookup for EXACTLY the given pids — never a
/// table-wide fetch. Windows issues one WMI call filtered to the requested
/// ProcessIds (`ProcessId=A or ProcessId=B ...`) selecting only
/// `ProcessId,CommandLine`. Empty `pids` ⇒ empty map with NO PowerShell spawn.
/// Fail-open: any process not in the result (dead, access-denied, no
/// command-line) is simply absent from the map.
#[cfg(windows)]
pub async fn command_lines_for_pids(pids: &[u32]) -> HashMap<u32, String> {
    if pids.is_empty() {
        return HashMap::new();
    }
    let filter = pids
        .iter()
        .map(|p| format!("ProcessId={p}"))
        .collect::<Vec<_>>()
        .join(" or ");
    // Force an array with @() so ConvertTo-Json on a single row still parses.
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue'; \
        @(Get-CimInstance Win32_Process -Filter \"{filter}\" | Select-Object ProcessId,CommandLine) | \
        ConvertTo-Json -Compress -Depth 3"
    );

    let output = match tokio::task::spawn_blocking(move || {
        crate::process_helpers::no_window("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
    })
    .await
    {
        Ok(Ok(o)) if o.status.success() => o,
        Ok(Ok(o)) => {
            tracing::warn!(
                "process_tree: command-line query failed (status={:?}, stderr={})",
                o.status,
                String::from_utf8_lossy(&o.stderr)
            );
            return HashMap::new();
        }
        Ok(Err(e)) => {
            tracing::warn!("process_tree: command-line query spawn error: {e}");
            return HashMap::new();
        }
        Err(e) => {
            tracing::warn!("process_tree: command-line query join error: {e}");
            return HashMap::new();
        }
    };

    parse_command_lines_json(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(windows)]
#[derive(Debug, Deserialize)]
struct WmiCmdlineRow {
    #[serde(rename = "ProcessId")]
    process_id: u32,
    #[serde(rename = "CommandLine", default)]
    command_line: Option<String>,
}

#[cfg(windows)]
fn parse_command_lines_json(json: &str) -> HashMap<u32, String> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return HashMap::new();
    }
    let rows: Vec<WmiCmdlineRow> = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("process_tree: failed to parse command-line JSON: {e}");
            return HashMap::new();
        }
    };
    let mut out = HashMap::new();
    for row in rows {
        if let Some(cl) = row.command_line {
            out.insert(row.process_id, cl);
        }
    }
    out
}

#[cfg(windows)]
pub fn port_owner_pid(port: u16) -> Option<u32> {
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue'; \
        $c = Get-NetTCPConnection -LocalPort {port} -ErrorAction SilentlyContinue | \
            Where-Object {{ $_.OwningProcess -gt 0 }} | \
            Select-Object -ExpandProperty OwningProcess -First 1; \
        if ($c) {{ Write-Output $c }}"
    );

    let output = crate::process_helpers::no_window("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout);
    s.trim().lines().next().and_then(|l| l.trim().parse().ok())
}

// ============================================================================
// Unix implementation
// ============================================================================

#[cfg(unix)]
async fn snapshot_process_table() -> ProcessSnapshot {
    tokio::task::spawn_blocking(snapshot_process_table_sync)
        .await
        .unwrap_or_default()
}

#[cfg(unix)]
fn snapshot_process_table_sync() -> ProcessSnapshot {
    let mut snap = ProcessSnapshot::default();

    let btime = read_btime().unwrap_or(0);
    let ticks_per_sec = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as i64;

    let proc_root = match std::fs::read_dir("/proc") {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("process_tree: failed to read /proc: {e}");
            return snap;
        }
    };

    for entry in proc_root.flatten() {
        let name = entry.file_name();
        let s = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        let pid: u32 = match s.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let stat_path = format!("/proc/{pid}/stat");
        let stat = match std::fs::read_to_string(&stat_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        // /proc/<pid>/stat: pid (comm) state ppid ... starttime ...
        // comm can contain spaces and parens; field 4 (ppid) is the first
        // word AFTER the last ')' in comm.
        let close = match stat.rfind(')') {
            Some(i) => i,
            None => continue,
        };
        let tail = &stat[close + 1..];
        let fields: Vec<&str> = tail.split_whitespace().collect();
        // tail starts at field 3 (state); ppid is fields[1]; starttime is
        // fields[19] (22 - 3 = 19).
        let ppid: u32 = fields.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let starttime_ticks: i64 = fields.get(19).and_then(|s| s.parse().ok()).unwrap_or(0);
        let spawned_at_unix = if btime > 0 && starttime_ticks > 0 {
            btime + (starttime_ticks / ticks_per_sec)
        } else {
            0
        };
        snap.parent_map.entry(ppid).or_default().push(pid);
        snap.creation_times.insert(pid, spawned_at_unix);
        // Image name for the claude-present liveness signal. `/proc/<pid>/comm`
        // is the kernel's `TASK_COMM` (≤15 chars, no path) — `claude` fits.
        // Fall back to the `comm` field already parsed out of `stat` (the text
        // between the first '(' and last ')') if the dedicated file is gone.
        let name = std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                let open = stat.find('(')?;
                Some(stat[open + 1..close].to_string())
            });
        if let Some(name) = name {
            snap.names.insert(pid, name);
        }
    }
    snap
}

#[cfg(unix)]
fn read_btime() -> Option<i64> {
    let s = std::fs::read_to_string("/proc/stat").ok()?;
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("btime ") {
            return rest.trim().parse().ok();
        }
    }
    None
}

/// TARGETED command-line lookup for EXACTLY the given pids — never a
/// table-wide fetch. Unix reads `/proc/<pid>/cmdline` per pid (NUL-separated
/// args → single space-joined string). Empty `pids` ⇒ empty map. Fail-open:
/// an unreadable pid (dead, permission-denied) is simply absent from the map.
#[cfg(unix)]
pub async fn command_lines_for_pids(pids: &[u32]) -> HashMap<u32, String> {
    if pids.is_empty() {
        return HashMap::new();
    }
    let pids = pids.to_vec();
    tokio::task::spawn_blocking(move || {
        let mut out = HashMap::new();
        for pid in pids {
            let Ok(bytes) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
                continue;
            };
            let joined = bytes
                .split(|b| *b == 0)
                .filter(|part| !part.is_empty())
                .map(|part| String::from_utf8_lossy(part).into_owned())
                .collect::<Vec<_>>()
                .join(" ");
            let joined = joined.trim().to_string();
            if !joined.is_empty() {
                out.insert(pid, joined);
            }
        }
        out
    })
    .await
    .unwrap_or_default()
}

#[cfg(unix)]
pub fn port_owner_pid(port: u16) -> Option<u32> {
    // Reuse the lsof shape already used by health.rs::kill_port_process Unix
    // branch. `-ti :<port>` prints PIDs (one per line) of any process owning
    // a socket bound to that port.
    let output = crate::process_helpers::no_window("lsof")
        .args(["-ti", &format!(":{port}")])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .and_then(|l| l.trim().parse().ok())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn pids(items: &[(u32, i64)]) -> Vec<PidWithSpawnedAt> {
        items
            .iter()
            .map(|(p, t)| PidWithSpawnedAt {
                pid: *p,
                spawned_at_unix: *t,
            })
            .collect()
    }

    #[test]
    fn bfs_collects_full_subtree() {
        // Tree: 1 → 2 → 3 → 4, and 1 → 5
        let mut parent_map: HashMap<u32, Vec<u32>> = HashMap::new();
        parent_map.insert(1, vec![2, 5]);
        parent_map.insert(2, vec![3]);
        parent_map.insert(3, vec![4]);
        let creation_times: HashMap<u32, i64> = [(2, 100), (3, 101), (4, 102), (5, 103)]
            .into_iter()
            .collect();
        let out = bfs_descendants_from(1, &parent_map, &creation_times);
        let mut got: Vec<u32> = out.iter().map(|p| p.pid).collect();
        got.sort();
        assert_eq!(got, vec![2, 3, 4, 5]);
    }

    #[test]
    fn bfs_excludes_root_and_handles_no_children() {
        let parent_map: HashMap<u32, Vec<u32>> = HashMap::new();
        let creation_times: HashMap<u32, i64> = HashMap::new();
        let out = bfs_descendants_from(42, &parent_map, &creation_times);
        assert!(out.is_empty());
    }

    fn snap_with(
        parent_map: &[(u32, &[u32])],
        creation: &[(u32, i64)],
        names: &[(u32, &str)],
    ) -> ProcessSnapshot {
        let mut s = ProcessSnapshot::default();
        for (p, kids) in parent_map {
            s.parent_map.insert(*p, kids.to_vec());
        }
        for (pid, t) in creation {
            s.creation_times.insert(*pid, *t);
        }
        for (pid, n) in names {
            s.names.insert(*pid, n.to_string());
        }
        s
    }

    // `reference_unix_millis` (millis) models the runner primary's own boot
    // time. A non-reused PID (spawned by this boot's TerminalManager) has
    // `creation_secs * 1000 >= reference` (minus skew). The alive fixtures
    // below use creation 1_000s (== 1_000_000ms) with a reference at/just-
    // before that instant so the PID-reuse guard is a no-op and the tests
    // exercise the image-name logic, not reuse.
    const REFERENCE_FRESH: i64 = 1_000_000;

    #[test]
    fn claude_present_root_is_claude() {
        // Agent / gate-continuation session: tracked PID *is* claude, zero
        // children — the exact idle shape the old descendant-count heuristic
        // mis-closed as poll-dead.
        let snap = snap_with(&[], &[(100, 1_000)], &[(100, "claude.exe")]);
        assert!(claude_present_in_inclusive_subtree(
            100,
            &snap,
            REFERENCE_FRESH
        ));
        // Case-insensitive, no extension, path-qualified — all match.
        let snap2 = snap_with(&[], &[(100, 1_000)], &[(100, "C:/bin/Claude")]);
        assert!(claude_present_in_inclusive_subtree(
            100,
            &snap2,
            REFERENCE_FRESH
        ));
    }

    #[test]
    fn claude_present_descendant_is_claude() {
        // Operator session: tracked PID is the shell; claude is a child.
        let snap = snap_with(
            &[(200, &[201])],
            &[(200, 1_000), (201, 1_001)],
            &[(200, "powershell.exe"), (201, "claude")],
        );
        assert!(claude_present_in_inclusive_subtree(
            200,
            &snap,
            REFERENCE_FRESH
        ));
    }

    #[test]
    fn claude_present_false_when_no_claude() {
        // Bare shell, operator quit claude: no claude anywhere in the subtree
        // → genuine poll-dead (the cleanup Option B preserves). The reference
        // is fresh so this exercises genuine absence, not the reuse guard.
        let snap = snap_with(
            &[(300, &[301])],
            &[(300, 1_000), (301, 1_001)],
            &[(300, "pwsh.exe"), (301, "node.exe")],
        );
        assert!(!claude_present_in_inclusive_subtree(
            300,
            &snap,
            REFERENCE_FRESH
        ));
    }

    #[test]
    fn claude_present_false_on_pid_reuse() {
        // Tracked PID's image says claude, but its creation time predates the
        // reference instant (the primary's boot time) by more than the skew →
        // the PID cannot belong to this boot's process tree → don't trust the
        // name. (1_000s == 1_000_000ms, reference 2_000_000ms — i.e. boot
        // happened 1000s after this PID was created: a genuine cross-boot
        // PID recycle.)
        let reused = snap_with(&[], &[(400, 1_000)], &[(400, "claude.exe")]);
        assert!(!claude_present_in_inclusive_subtree(
            400, &reused, 2_000_000
        ));
        // Within the skew window (3ms later) it is NOT reuse — a freshly
        // spawned claude whose creation rounds just past the reference.
        let fresh = snap_with(&[], &[(400, 1_000)], &[(400, "claude.exe")]);
        assert!(claude_present_in_inclusive_subtree(400, &fresh, 1_000_003));
        // Unresolved creation time (0) must never count as reuse.
        let unknown = snap_with(&[], &[(400, 0)], &[(400, "claude.exe")]);
        assert!(claude_present_in_inclusive_subtree(
            400, &unknown, 2_000_000
        ));
    }

    /// Regression test for the live 2026-07-03 false-poll-dead-close incident
    /// (found during Phase 1 verification of PR #630). A terminal spawned at
    /// boot (PID created at 1_000s == 1_000_000ms) later gets a SECOND session
    /// record opened against it — e.g. a reconnect into the same pane — whose
    /// own `opened_at` is much later (2_000_000ms). Passing that later
    /// record's `opened_at` as the reference (the old, buggy call site) would
    /// wrongly conclude the PID was recycled and return `false`, eventually
    /// flipping a genuinely live, idle session to `poll-dead`. Passing the
    /// primary's own boot time instead (1_000_000ms, at/before the PID's
    /// creation) is the fix: the same live PID is correctly still trusted.
    #[test]
    fn claude_present_true_when_terminal_reused_by_later_record() {
        let snap = snap_with(&[], &[(500, 1_000)], &[(500, "claude.exe")]);
        // The bug: a per-record `opened_at` well after this PID's creation
        // falsely reads as PID reuse.
        assert!(!claude_present_in_inclusive_subtree(500, &snap, 2_000_000));
        // The fix: the primary's own boot time (at/before the PID's creation)
        // correctly reads the same live PID as present, regardless of how
        // much later a reused terminal's newest session record was opened.
        assert!(claude_present_in_inclusive_subtree(
            500,
            &snap,
            REFERENCE_FRESH
        ));
    }

    #[test]
    fn claude_anchor_picks_earliest_known_creation() {
        // Two claude images under a shell: the OLDER (earlier creation) is the
        // anchor. Shell (200) is not claude and must be ignored.
        let snap = snap_with(
            &[(200, &[201, 202])],
            &[(200, 900), (201, 1_100), (202, 1_050)],
            &[(200, "pwsh.exe"), (201, "claude.exe"), (202, "claude")],
        );
        assert_eq!(claude_anchor_in_subtree(200, &snap), Some((202, 1_050)));

        // Root itself is claude with a known creation → anchor is the root.
        let root = snap_with(&[], &[(300, 1_000)], &[(300, "claude")]);
        assert_eq!(claude_anchor_in_subtree(300, &root), Some((300, 1_000)));

        // Unknown (0) creation sorts LAST: the one with a known time wins.
        let mixed = snap_with(
            &[(400, &[401, 402])],
            &[(400, 900), (401, 0), (402, 2_000)],
            &[(400, "sh"), (401, "claude"), (402, "claude")],
        );
        assert_eq!(claude_anchor_in_subtree(400, &mixed), Some((402, 2_000)));

        // No claude image anywhere → None.
        let none = snap_with(&[(500, &[501])], &[], &[(500, "sh"), (501, "node")]);
        assert_eq!(claude_anchor_in_subtree(500, &none), None);
    }

    #[test]
    fn parse_session_id_handles_space_equals_and_quoted_forms() {
        let id = "1a2b3c4d-5e6f-7a8b-9c0d-1e2f3a4b5c6d";
        // Space-separated.
        assert_eq!(
            parse_session_id_from_cmdline(&format!("claude --session-id {id} --resume")),
            Some(id.to_string())
        );
        // Equals.
        assert_eq!(
            parse_session_id_from_cmdline(&format!("claude --session-id={id}")),
            Some(id.to_string())
        );
        // Quoted.
        assert_eq!(
            parse_session_id_from_cmdline(&format!("claude --session-id \"{id}\" -p")),
            Some(id.to_string())
        );
        // Reject: value present but not UUID-shaped.
        assert_eq!(
            parse_session_id_from_cmdline("claude --session-id not-a-uuid"),
            None
        );
        // Reject: flag absent.
        assert_eq!(parse_session_id_from_cmdline("claude --resume abc"), None);
    }

    #[cfg(windows)]
    #[test]
    fn parse_command_lines_json_maps_pid_to_cmdline() {
        let json = r#"[
            {"ProcessId":10,"CommandLine":"C:/bin/claude.exe --session-id abc"},
            {"ProcessId":11,"CommandLine":null},
            {"ProcessId":12,"CommandLine":"powershell.exe"}
        ]"#;
        let map = parse_command_lines_json(json);
        assert_eq!(
            map.get(&10).map(|s| s.as_str()),
            Some("C:/bin/claude.exe --session-id abc")
        );
        assert!(!map.contains_key(&11), "null command line is absent");
        assert_eq!(map.get(&12).map(|s| s.as_str()), Some("powershell.exe"));
        // Single-row (array-forced) shape still parses.
        let single = r#"[{"ProcessId":20,"CommandLine":"claude"}]"#;
        assert_eq!(
            parse_command_lines_json(single)
                .get(&20)
                .map(|s| s.as_str()),
            Some("claude")
        );
        // Empty stdout → empty map.
        assert!(parse_command_lines_json("").is_empty());
    }

    #[test]
    fn identify_service_pid_port_owner_wins() {
        // descendants = {2, 3, 4}; port_owner = 3 → returns Some(3).
        let mut parent_map: HashMap<u32, Vec<u32>> = HashMap::new();
        parent_map.insert(1, vec![2]);
        parent_map.insert(2, vec![3]);
        parent_map.insert(3, vec![4]);
        let descendants = pids(&[(2, 0), (3, 0), (4, 0)]);
        let got =
            identify_service_pid_with_lookup(&descendants, &parent_map, Some(8000), 1, |_port| {
                Some(3)
            });
        assert_eq!(got, Some(3));
    }

    #[test]
    fn identify_service_pid_port_owner_outside_tree_falls_through_to_leaf() {
        // port_owner returns a PID not in our tracked descendants → don't
        // claim it; fall through to leaf rule.
        let mut parent_map: HashMap<u32, Vec<u32>> = HashMap::new();
        parent_map.insert(1, vec![2]);
        parent_map.insert(2, vec![3]);
        let descendants = pids(&[(2, 0), (3, 0)]);
        let got = identify_service_pid_with_lookup(
            &descendants,
            &parent_map,
            Some(8000),
            1,
            |_port| Some(9999), // unrelated PID
        );
        // Leaf is 3 (no tracked children).
        assert_eq!(got, Some(3));
    }

    #[test]
    fn identify_service_pid_leaf_wins() {
        // No health_port; 4-deep chain 1 → 2 → 3 → 4. Leaf = 4.
        let mut parent_map: HashMap<u32, Vec<u32>> = HashMap::new();
        parent_map.insert(1, vec![2]);
        parent_map.insert(2, vec![3]);
        parent_map.insert(3, vec![4]);
        let descendants = pids(&[(2, 0), (3, 0), (4, 0)]);
        let got = identify_service_pid_with_lookup(&descendants, &parent_map, None, 1, |_| None);
        assert_eq!(got, Some(4));
    }

    #[test]
    fn identify_service_pid_fallback_when_no_descendants() {
        let parent_map: HashMap<u32, Vec<u32>> = HashMap::new();
        let descendants: Vec<PidWithSpawnedAt> = Vec::new();
        let got =
            identify_service_pid_with_lookup(&descendants, &parent_map, Some(8000), 42, |_| None);
        assert_eq!(got, Some(42));
    }

    #[cfg(windows)]
    #[test]
    fn parse_wmi_creation_date_dotnet_style() {
        let v = serde_json::Value::String("/Date(1715911300000)/".to_string());
        assert_eq!(parse_wmi_creation_date(&v), 1715911300);
    }

    #[cfg(windows)]
    #[test]
    fn parse_wmi_creation_date_iso_style() {
        let v = serde_json::Value::String("2026-05-16T18:50:06.000Z".to_string());
        // Just assert non-zero — exact value depends on UTC epoch, but
        // this verifies the ISO branch wasn't dropped.
        assert!(parse_wmi_creation_date(&v) > 1_700_000_000);
    }

    #[cfg(windows)]
    #[test]
    fn parse_powershell_snapshot_builds_parent_map() {
        let json = r#"[
            {"ProcessId":1,"ParentProcessId":0,"CreationDate":"/Date(1715911000000)/"},
            {"ProcessId":2,"ParentProcessId":1,"CreationDate":"/Date(1715911100000)/"},
            {"ProcessId":3,"ParentProcessId":2,"CreationDate":"/Date(1715911200000)/"}
        ]"#;
        let snap = parse_powershell_snapshot(json);
        assert_eq!(
            snap.parent_map.get(&1).map(|v| v.as_slice()),
            Some(&[2u32][..])
        );
        assert_eq!(
            snap.parent_map.get(&2).map(|v| v.as_slice()),
            Some(&[3u32][..])
        );
        assert_eq!(snap.creation_times.get(&2).copied(), Some(1715911100));
    }

    #[cfg(windows)]
    #[test]
    fn parse_powershell_snapshot_captures_name() {
        let json = r#"[
            {"ProcessId":10,"ParentProcessId":0,"CreationDate":"/Date(1715911000000)/","Name":"powershell.exe"},
            {"ProcessId":11,"ParentProcessId":10,"CreationDate":"/Date(1715911000000)/","Name":"claude.exe"}
        ]"#;
        let snap = parse_powershell_snapshot(json);
        assert_eq!(
            snap.names.get(&10).map(|s| s.as_str()),
            Some("powershell.exe")
        );
        assert_eq!(snap.names.get(&11).map(|s| s.as_str()), Some("claude.exe"));
        // End-to-end: the inclusive-subtree helper sees claude under the shell.
        // reference == the shell's creation in millis so the reuse guard is a
        // no-op (creation does not predate the reference).
        assert!(claude_present_in_inclusive_subtree(
            10,
            &snap,
            1_715_911_000_000
        ));
    }
}
