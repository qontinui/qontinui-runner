//! Durable runner-local registry of looping agents.
//!
//! ## Design choice: JSON store, not a DB table
//!
//! Matches the house pattern for runner-local durable session/lifecycle
//! registries: `session_lifecycle_store.rs` (`~/.qontinui/runner/
//! terminal-sessions.json`) and `pane_store.rs` both use a single JSON file
//! rewritten atomically via temp-file + rename, with a tolerant per-record
//! load so one malformed row never wipes the store. The runner's embedded DB
//! is used for high-volume operational data, not for small operator-facing
//! config registries — and Phase 2 moves authoring to qontinui-web anyway, at
//! which point this file becomes the offline-fallback projection, which a
//! JSON file models better than a schema-migrated table.
//!
//! ## Record shape
//!
//! `id -> { def, runtime }` where [`LoopingAgentDef`] is the (Phase-2
//! web-authored) definition and [`LoopingAgentRuntime`] is the runner's own
//! durable spawn bookkeeping (current terminal / claude-session ids, cycle
//! counters) that lets a runner restart re-attach to a still-live tab instead
//! of spawning a duplicate.
//!
//! ## No-surprise default
//!
//! The built-in Merge Shepherd is seeded `enabled=false` /
//! `desired_state=stopped`: a runner upgrade must NEVER start spawning
//! autonomous terminals by itself — the operator flips it on explicitly via
//! the `looping_agent_set_enabled` command.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tracing::warn;

/// Registry id of the built-in Merge Shepherd (Phase 1's single agent).
pub const MERGE_SHEPHERD_ID: &str = "merge-shepherd";

/// Whether the supervisor should be keeping this agent's tab alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesiredState {
    /// Keep a live tab running the playbook loop (spawn / nudge / relaunch /
    /// respawn as needed).
    Running,
    /// Do not supervise: no nudges, no relaunches, no respawns. An existing
    /// tab is left alone for the operator to inspect/close.
    Stopped,
}

/// Relaunch/self-heal knobs for one looping agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LifecyclePolicy {
    /// Proactively relaunch a FRESH claude (new `--session-id`, re-read
    /// journal) after this many cycles even if no context-low marker was
    /// seen — the backstop that keeps a long-lived loop from ever nearing
    /// the context limit. Minimum 1 (a 0 is treated as 1 by the policy core).
    pub relaunch_every_cycles: u32,
    /// After a (re)spawn, leave the fresh session alone for this long before
    /// evaluating idle/nudge/relaunch against it (lets the CLI boot and start
    /// its first cycle without a premature nudge racing the startup paint).
    pub spawn_grace_secs: u64,
    /// Context-low relaunch threshold: when the on-screen
    /// "Context left until auto-compact: N%" marker parses to a percentage at
    /// or below this, the supervisor relaunches fresh at the next idle tick.
    pub context_low_threshold_pct: u32,
}

impl Default for LifecyclePolicy {
    fn default() -> Self {
        Self {
            relaunch_every_cycles: 8,
            spawn_grace_secs: 90,
            context_low_threshold_pct: 15,
        }
    }
}

/// One looping-agent definition — the (future) web-authored record. Phase 1
/// hard-codes a single built-in instance (the Merge Shepherd) seeded from
/// [`builtin_merge_shepherd`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopingAgentDef {
    /// Stable registry id (the map key).
    pub id: String,
    /// Human-readable name — used as the visible tab title.
    pub name: String,
    /// Which bundled playbook drives this agent (resolved via
    /// [`crate::looping_agent::playbook::playbook_for`]). Phase 2 lets a
    /// web-served record override the bundled content.
    pub playbook_ref: String,
    /// Minimum spacing between cycle STARTS. The nudge fires only when the
    /// tab is idle AND this much time has passed since the last cycle began,
    /// so an over-long cycle naturally stretches the interval.
    pub cadence_secs: u64,
    /// Repos this agent shepherds. Empty = "all repos this tenant owns"
    /// (the playbook's own default scope).
    #[serde(default)]
    pub repos: Vec<String>,
    /// Absolute path of the agent's durable journal — its real memory.
    pub journal_path: String,
    /// Relaunch/self-heal knobs.
    #[serde(default)]
    pub lifecycle_policy: LifecyclePolicy,
    /// Master switch. `false` ⇒ the supervisor ignores this agent entirely.
    pub enabled: bool,
    /// Operational target state (see [`DesiredState`]).
    pub desired_state: DesiredState,
}

/// The runner's durable spawn bookkeeping for one agent. Persisted so a
/// runner restart can RE-ATTACH to a still-live (boot-restored) tab instead
/// of spawning a duplicate shepherd, and so cycle counters survive restarts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LoopingAgentRuntime {
    /// Terminal id of the tab currently (or last) hosting this agent.
    pub terminal_id: Option<String>,
    /// The pinned `--session-id` of the current (or last) spawn — the key
    /// into the session lifecycle store for restart re-attach.
    pub claude_session_id: Option<String>,
    /// Cycles run since the last FRESH spawn (spawn itself counts as cycle 1;
    /// each nudge increments). Compared against
    /// [`LifecyclePolicy::relaunch_every_cycles`].
    pub cycles_since_relaunch: u32,
    /// Unix millis of the last fresh spawn/relaunch.
    pub last_spawn_at_ms: Option<i64>,
    /// Unix millis when the most recent cycle was started (spawn or nudge).
    pub last_cycle_started_at_ms: Option<i64>,
    /// Total fresh relaunches over this agent's lifetime (observability).
    pub relaunch_count: u32,
}

/// One persisted registry row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopingAgentRecord {
    pub def: LoopingAgentDef,
    #[serde(default)]
    pub runtime: LoopingAgentRuntime,
}

/// The built-in Merge Shepherd definition (Tier-1 playbook bundled in the
/// runner — see `playbook.rs`). `journal_path` is caller-supplied because the
/// pure lib can't resolve the home dir consistently with the bin's
/// port-namespacing rules. Seeded DISABLED (see module doc).
pub fn builtin_merge_shepherd(journal_path: String) -> LoopingAgentDef {
    LoopingAgentDef {
        id: MERGE_SHEPHERD_ID.to_string(),
        name: "Merge Shepherd".to_string(),
        playbook_ref: MERGE_SHEPHERD_ID.to_string(),
        // Playbook default cycle interval: 5 minutes.
        cadence_secs: 300,
        // Empty = all repos this tenant owns (the playbook's default scope).
        repos: Vec::new(),
        journal_path,
        lifecycle_policy: LifecyclePolicy::default(),
        enabled: false,
        desired_state: DesiredState::Stopped,
    }
}

/// Durable map of `agent_id -> LoopingAgentRecord`. Every mutation takes the
/// lock and rewrites the backing file atomically (temp + rename). Mirrors
/// `SessionLifecycleStore`'s posture: a write failure is logged, never
/// propagated (the in-memory map still reflects the mutation).
#[derive(Debug)]
pub struct LoopingAgentRegistry {
    path: PathBuf,
    map: Mutex<HashMap<String, LoopingAgentRecord>>,
}

impl LoopingAgentRegistry {
    /// Open (or initialize) the registry at `path`, loading any existing map
    /// with per-record tolerance (one malformed row is warn-dropped, the rest
    /// are kept).
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let map = load_map(&path);
        Ok(Self {
            path,
            map: Mutex::new(map),
        })
    }

    /// Seed the built-in Merge Shepherd iff absent. Returns `true` when the
    /// row was inserted. NEVER overwrites an existing row (operator edits —
    /// enabled state, cadence — survive upgrades).
    pub fn seed_merge_shepherd(&self, journal_path: String) -> bool {
        let snapshot = {
            let mut m = self.lock();
            if m.contains_key(MERGE_SHEPHERD_ID) {
                return false;
            }
            m.insert(
                MERGE_SHEPHERD_ID.to_string(),
                LoopingAgentRecord {
                    def: builtin_merge_shepherd(journal_path),
                    runtime: LoopingAgentRuntime::default(),
                },
            );
            m.clone()
        };
        self.persist(&snapshot);
        true
    }

    /// Clone of every record, ordered by id for stable presentation.
    pub fn list(&self) -> Vec<LoopingAgentRecord> {
        let m = self.lock();
        let mut v: Vec<LoopingAgentRecord> = m.values().cloned().collect();
        v.sort_by(|a, b| a.def.id.cmp(&b.def.id));
        v
    }

    /// Clone of one record by id.
    pub fn get(&self, id: &str) -> Option<LoopingAgentRecord> {
        self.lock().get(id).cloned()
    }

    /// Flip an agent's master switch. `enabled=true` also sets
    /// `desired_state=Running`; `enabled=false` sets `Stopped` (the two are
    /// deliberately coupled in Phase 1 — one operator-facing switch).
    /// Returns the updated record, or `None` for an unknown id.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Option<LoopingAgentRecord> {
        let (snapshot, updated) = {
            let mut m = self.lock();
            let rec = m.get_mut(id)?;
            rec.def.enabled = enabled;
            rec.def.desired_state = if enabled {
                DesiredState::Running
            } else {
                DesiredState::Stopped
            };
            let updated = rec.clone();
            (m.clone(), updated)
        };
        self.persist(&snapshot);
        Some(updated)
    }

    /// Mutate an agent's runtime bookkeeping in place (spawn/nudge/relaunch
    /// bookkeeping). No-op for an unknown id.
    pub fn update_runtime(&self, id: &str, f: impl FnOnce(&mut LoopingAgentRuntime)) {
        let snapshot = {
            let mut m = self.lock();
            let Some(rec) = m.get_mut(id) else {
                return;
            };
            f(&mut rec.runtime);
            m.clone()
        };
        self.persist(&snapshot);
    }

    /// Poison-tolerant lock (house style — a panicked writer must not wedge
    /// every later reader; the map is always internally consistent because
    /// mutations are single-call).
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, LoopingAgentRecord>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Best-effort atomic flush. A write failure is logged, not propagated.
    fn persist(&self, snapshot: &HashMap<String, LoopingAgentRecord>) {
        if let Err(e) = write_map(&self.path, snapshot) {
            warn!(
                error = %e,
                path = %self.path.display(),
                "looping_agent registry: persist failed — kept in memory only"
            );
        }
    }
}

/// Tolerant two-stage load: parse the top-level object, then each record
/// individually — one malformed row is warn-dropped, the rest kept (the
/// `session_lifecycle_store::load_map` posture, minus the `.corrupt` sidecar:
/// this registry is tiny and seedable, so a dropped row is re-seedable).
fn load_map(path: &Path) -> HashMap<String, LoopingAgentRecord> {
    if !path.exists() {
        return HashMap::new();
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "looping_agent registry: read failed — starting empty");
            return HashMap::new();
        }
    };
    let raw: HashMap<String, serde_json::Value> = match serde_json::from_slice(&bytes) {
        Ok(raw) => raw,
        Err(e) => {
            warn!(
                error = %e,
                path = %path.display(),
                "looping_agent registry: corrupt file (not a JSON object) — starting empty"
            );
            return HashMap::new();
        }
    };
    let mut map = HashMap::with_capacity(raw.len());
    for (key, value) in raw {
        match serde_json::from_value::<LoopingAgentRecord>(value) {
            Ok(rec) => {
                map.insert(key, rec);
            }
            Err(e) => warn!(
                error = %e,
                key = %key,
                "looping_agent registry: malformed record — dropped, keeping the rest"
            ),
        }
    }
    map
}

fn write_map(path: &Path, map: &HashMap<String, LoopingAgentRecord>) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(map).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn seed_is_disabled_and_stopped_by_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("looping-agents.json");
        let reg = LoopingAgentRegistry::open(&path).unwrap();

        assert!(reg.seed_merge_shepherd("C:/j/journal.jsonl".to_string()));
        let rec = reg.get(MERGE_SHEPHERD_ID).expect("seeded");
        // UX no-surprise rule: a new autonomous agent must NOT start spawning
        // terminals on upgrade — it ships off.
        assert!(!rec.def.enabled, "built-in must seed disabled");
        assert_eq!(rec.def.desired_state, DesiredState::Stopped);
        assert_eq!(rec.def.cadence_secs, 300);
        assert_eq!(rec.def.lifecycle_policy.relaunch_every_cycles, 8);
        assert_eq!(rec.def.journal_path, "C:/j/journal.jsonl");
        assert!(rec.def.repos.is_empty(), "empty repos = all tenant repos");
        assert_eq!(rec.runtime, LoopingAgentRuntime::default());

        // Second seed is a no-op (idempotent across boots).
        assert!(!reg.seed_merge_shepherd("C:/other.jsonl".to_string()));
        assert_eq!(
            reg.get(MERGE_SHEPHERD_ID).unwrap().def.journal_path,
            "C:/j/journal.jsonl",
            "reseed must not clobber the existing row"
        );
    }

    #[test]
    fn seed_never_clobbers_operator_edits_across_boots() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("looping-agents.json");
        {
            let reg = LoopingAgentRegistry::open(&path).unwrap();
            reg.seed_merge_shepherd("C:/j.jsonl".to_string());
            reg.set_enabled(MERGE_SHEPHERD_ID, true).unwrap();
        }
        // "Upgrade + reboot": reopen and reseed.
        let reg = LoopingAgentRegistry::open(&path).unwrap();
        assert!(!reg.seed_merge_shepherd("C:/j.jsonl".to_string()));
        let rec = reg.get(MERGE_SHEPHERD_ID).unwrap();
        assert!(rec.def.enabled, "operator enable survives reseed");
        assert_eq!(rec.def.desired_state, DesiredState::Running);
    }

    #[test]
    fn set_enabled_couples_desired_state() {
        let dir = tempdir().unwrap();
        let reg = LoopingAgentRegistry::open(dir.path().join("r.json")).unwrap();
        reg.seed_merge_shepherd("C:/j.jsonl".to_string());

        let on = reg.set_enabled(MERGE_SHEPHERD_ID, true).unwrap();
        assert!(on.def.enabled);
        assert_eq!(on.def.desired_state, DesiredState::Running);

        let off = reg.set_enabled(MERGE_SHEPHERD_ID, false).unwrap();
        assert!(!off.def.enabled);
        assert_eq!(off.def.desired_state, DesiredState::Stopped);

        assert!(reg.set_enabled("no-such-agent", true).is_none());
    }

    #[test]
    fn runtime_updates_persist_across_reload() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("r.json");
        {
            let reg = LoopingAgentRegistry::open(&path).unwrap();
            reg.seed_merge_shepherd("C:/j.jsonl".to_string());
            reg.update_runtime(MERGE_SHEPHERD_ID, |rt| {
                rt.terminal_id = Some("term-1".to_string());
                rt.claude_session_id = Some("sess-1".to_string());
                rt.cycles_since_relaunch = 3;
                rt.last_spawn_at_ms = Some(1000);
                rt.last_cycle_started_at_ms = Some(2000);
                rt.relaunch_count = 2;
            });
            // Unknown id: no-op, no panic.
            reg.update_runtime("ghost", |rt| rt.cycles_since_relaunch = 99);
        }
        let reg = LoopingAgentRegistry::open(&path).unwrap();
        let rt = reg.get(MERGE_SHEPHERD_ID).unwrap().runtime;
        assert_eq!(rt.terminal_id.as_deref(), Some("term-1"));
        assert_eq!(rt.claude_session_id.as_deref(), Some("sess-1"));
        assert_eq!(rt.cycles_since_relaunch, 3);
        assert_eq!(rt.last_spawn_at_ms, Some(1000));
        assert_eq!(rt.last_cycle_started_at_ms, Some(2000));
        assert_eq!(rt.relaunch_count, 2);
    }

    #[test]
    fn tolerant_load_drops_bad_record_keeps_good() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("r.json");
        let file = r#"{
            "good": {
                "def": {
                    "id":"good","name":"Good","playbookRef":"merge-shepherd",
                    "cadenceSecs":300,"journalPath":"C:/j.jsonl",
                    "enabled":false,"desiredState":"stopped"
                }
            },
            "bad": { "def": { "id": 42 } }
        }"#;
        std::fs::write(&path, file).unwrap();
        let reg = LoopingAgentRegistry::open(&path).unwrap();
        assert!(reg.get("good").is_some(), "valid row kept");
        assert!(reg.get("bad").is_none(), "malformed row dropped");
        // Defaults filled in for omitted optional fields.
        let good = reg.get("good").unwrap();
        assert_eq!(good.def.lifecycle_policy, LifecyclePolicy::default());
        assert!(good.def.repos.is_empty());
        assert_eq!(good.runtime, LoopingAgentRuntime::default());
    }

    #[test]
    fn corrupt_file_starts_empty_and_is_reseedable() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("r.json");
        std::fs::write(&path, b"not json at all").unwrap();
        let reg = LoopingAgentRegistry::open(&path).unwrap();
        assert!(reg.list().is_empty());
        assert!(reg.seed_merge_shepherd("C:/j.jsonl".to_string()));
        assert_eq!(reg.list().len(), 1);
    }

    #[test]
    fn record_round_trips_through_serde() {
        let mut def = builtin_merge_shepherd("C:/j.jsonl".to_string());
        def.repos = vec!["qontinui/qontinui-runner".to_string()];
        def.enabled = true;
        def.desired_state = DesiredState::Running;
        let rec = LoopingAgentRecord {
            def,
            runtime: LoopingAgentRuntime {
                terminal_id: Some("t".to_string()),
                claude_session_id: Some("s".to_string()),
                cycles_since_relaunch: 5,
                last_spawn_at_ms: Some(1),
                last_cycle_started_at_ms: Some(2),
                relaunch_count: 1,
            },
        };
        let json = serde_json::to_string(&rec).unwrap();
        // Wire shape is camelCase + snake_case desired_state values.
        assert!(json.contains("\"desiredState\":\"running\""), "{json}");
        assert!(json.contains("\"relaunchEveryCycles\":8"), "{json}");
        let back: LoopingAgentRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rec);
    }
}
