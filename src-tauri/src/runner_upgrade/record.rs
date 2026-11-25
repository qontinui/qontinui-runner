//! The upgrade record and the booting marker.
//!
//! - **The record** `${QONTINUI_HOME:-~/.qontinui}/runner/upgrades/<ts>.json`
//!   is the audit of one upgrade and the hand-off between its three actors:
//!   the old runner writes it (from/to sha and dir, the holders, the
//!   `DIES-WITH-RUNNER` set with resume specs, the embedded-PG schema), the
//!   successor writes its probation into it, and the rollback guard reads its
//!   state when it fires and writes the rollback.
//! - **The marker** `…/upgrades/in-flight.json` is the booting signal Phase 4's
//!   clients read to decide whether a refused connect means "coming back" (retry)
//!   or "down" (fail fast). Its shape is fixed:
//!   `{"state":"in_flight","port":…,"started_at":…,"deadline":…,"from_sha":…,"to_sha":…}`.
//!   It exists from just before the exit until the successor passes probation
//!   or the guard has acted.
//!
//! Every write is atomic (temp file + rename): the guard and the successor
//! read these files while the other may be writing them.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The record format version.
pub const RECORD_SCHEMA: u32 = 1;

/// The booting marker's file name.
pub const IN_FLIGHT_FILE: &str = "in-flight.json";

/// Where one upgrade stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpgradeState {
    /// The old runner exited (or is exiting) into the successor; the guard is
    /// armed; probation has not concluded.
    InFlight,
    /// The successor proved itself; the guard was cancelled.
    ProbationPassed,
    /// The successor checked itself and found something missing. The guard
    /// still fires and rolls back.
    ProbationFailed,
    /// The guard re-pointed `current` at the old build and restarted it.
    RolledBack,
    /// The guard did NOT roll back: the successor changed the embedded-PG
    /// schema, which the old build may not read. Escalated, never silent.
    RollbackRefused,
    /// The guard tried to roll back and a step failed. Escalated.
    RollbackFailed,
    /// The executor stopped before the exit (a step failed); nothing changed
    /// hands.
    Aborted,
}

impl UpgradeState {
    /// The guard acts only on these: the successor has not proved itself.
    pub fn guard_should_act(self) -> bool {
        matches!(self, Self::InFlight | Self::ProbationFailed)
    }

    /// A state that ends the upgrade and must reach the operator.
    pub fn needs_escalation(self) -> bool {
        matches!(
            self,
            Self::ProbationFailed | Self::RolledBack | Self::RollbackRefused | Self::RollbackFailed
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::InFlight => "in_flight",
            Self::ProbationPassed => "probation_passed",
            Self::ProbationFailed => "probation_failed",
            Self::RolledBack => "rolled_back",
            Self::RollbackRefused => "rollback_refused",
            Self::RollbackFailed => "rollback_failed",
            Self::Aborted => "aborted",
        }
    }
}

/// How the old build comes back if the guard fires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RestartTarget {
    /// The runner is a systemd user unit; the guard restarts it after
    /// re-pointing `current` (the unit's `ExecStart` goes through the link).
    SystemdUnit { unit: String },
    /// No service manager: the guard ends the successor (if it still runs)
    /// and starts `from_exe` itself.
    SelfManaged {
        from_exe: PathBuf,
        successor_pid: Option<u32>,
    },
}

/// A holder pane that was live when the old runner exited — the successor
/// must adopt it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolderSnapshot {
    pub pane_id: String,
    pub holder_pid: Option<u32>,
    pub child_pid: Option<u32>,
    pub negotiated_version: Option<u32>,
    /// The sessions running in it, by `claude_session_id` — probation joins on
    /// these, never on a pid (an account migration may move a session).
    pub session_ids: Vec<String>,
}

/// A session NOT in a holder: it dies with the runner, and the successor
/// resumes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeSpec {
    /// `in_process_pane` | `ai_session` | `headless` | `unclassified` |
    /// `undecided`.
    pub kind: String,
    pub session_id: Option<String>,
    pub pid: Option<u32>,
    pub cwd: Option<String>,
    /// The account (`CLAUDE_CONFIG_DIR`) it runs under, when known.
    pub config_dir: Option<String>,
    pub terminal_id: Option<String>,
}

/// One `DIES-WITH-RUNNER` session's resume, as the successor verified it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeOutcome {
    pub session_id: Option<String>,
    pub kind: String,
    /// The resume door's verdict, or why none was attempted.
    pub verdict: String,
    /// A process read showed it running again.
    pub verified: bool,
}

/// The successor's probation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProbationReport {
    pub passed: bool,
    /// `/health` `gitSha` as the successor served it.
    pub health_sha: Option<String>,
    /// Holder panes not adopted (by pane id, with no session found live
    /// elsewhere either).
    pub holders_missing: Vec<String>,
    pub resumes: Vec<ResumeOutcome>,
    /// Every reason probation failed.
    pub failures: Vec<String>,
    pub finished_at: Option<String>,
}

/// The upgrade record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpgradeRecord {
    pub schema: u32,
    pub id: String,
    pub state: UpgradeState,
    pub created_at: String,
    pub from_sha: String,
    pub to_sha: String,
    pub from_dir: PathBuf,
    pub to_dir: PathBuf,
    /// The `target/primary/current` link the swap and the rollback move.
    pub current_link: PathBuf,
    /// An explicit downgrade was asked for.
    pub rollback: bool,
    pub api_port: u16,
    /// The old runner's pid (the successor of a self-managed handoff waits
    /// for it to exit).
    pub from_pid: u32,
    pub restart: RestartTarget,
    /// The transient systemd unit running the guard (`<unit>.timer` +
    /// `<unit>.service`), or `None` for a detached guard process.
    pub guard_unit: Option<String>,
    pub guard_pid: Option<u32>,
    pub guard_delay_secs: u64,
    /// RFC 3339: when the guard fires.
    pub deadline: String,
    pub holders: Vec<HolderSnapshot>,
    pub dies_with_runner: Vec<ResumeSpec>,
    /// The embedded-PG schema fingerprint before the exit
    /// (`super::pg_schema`).
    pub from_pg_schema: Option<String>,
    /// The fingerprint the successor saw after it booted; `None` until it
    /// records one.
    pub to_pg_schema: Option<String>,
    pub probation: Option<ProbationReport>,
    /// Why the record is in its current terminal state.
    pub outcome_reason: Option<String>,
    /// The guard rolled back without a successor schema to compare against.
    #[serde(default)]
    pub pg_schema_unverified: bool,
    pub concluded_at: Option<String>,
    /// The state whose outcome was last sent to coord (finding + operator
    /// notification), and when. A later escalating state is reported again.
    pub reported_state: Option<UpgradeState>,
    pub reported_at: Option<String>,
    /// The preflight verdict the upgrade ran under.
    pub preflight: serde_json::Value,
}

impl UpgradeRecord {
    /// The record ends in a state the operator must hear about and has not
    /// heard about yet.
    pub fn report_due(&self) -> bool {
        self.state.needs_escalation() && self.reported_state != Some(self.state)
    }

    /// Conclude in `state` with `reason`.
    pub fn conclude(&mut self, state: UpgradeState, reason: impl Into<String>, now: &str) {
        self.state = state;
        self.outcome_reason = Some(reason.into());
        self.concluded_at = Some(now.to_string());
    }
}

/// The booting marker. Phase 4 clients read exactly these fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InFlightMarker {
    /// Always `"in_flight"`.
    pub state: String,
    pub port: u16,
    pub started_at: String,
    pub deadline: String,
    pub from_sha: String,
    pub to_sha: String,
}

impl InFlightMarker {
    pub fn for_record(r: &UpgradeRecord) -> Self {
        Self {
            state: "in_flight".to_string(),
            port: r.api_port,
            started_at: r.created_at.clone(),
            deadline: r.deadline.clone(),
            from_sha: r.from_sha.clone(),
            to_sha: r.to_sha.clone(),
        }
    }
}

/// Write `bytes` to `path` atomically: a sibling temp file, fsynced, renamed
/// over `path`.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// The record file for an upgrade id.
pub fn record_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

/// The marker file.
pub fn marker_path(dir: &Path) -> PathBuf {
    dir.join(IN_FLIGHT_FILE)
}

pub fn write_record(path: &Path, r: &UpgradeRecord) -> io::Result<()> {
    let text = serde_json::to_vec_pretty(r).map_err(io::Error::other)?;
    write_atomic(path, &text)
}

pub fn read_record(path: &Path) -> Result<UpgradeRecord, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read upgrade record {}: {e}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|e| format!("upgrade record {} is malformed: {e}", path.display()))
}

pub fn write_marker(dir: &Path, m: &InFlightMarker) -> io::Result<()> {
    let text = serde_json::to_vec_pretty(m).map_err(io::Error::other)?;
    write_atomic(&marker_path(dir), &text)
}

pub fn read_marker(dir: &Path) -> Option<InFlightMarker> {
    let text = std::fs::read_to_string(marker_path(dir)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Remove the marker; absent is fine.
pub fn remove_marker(dir: &Path) -> io::Result<()> {
    match std::fs::remove_file(marker_path(dir)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Every record in `dir`, oldest first (ids sort by time). Unreadable files
/// are returned as errors beside the good ones, never dropped silently.
pub fn all_records(dir: &Path) -> (Vec<(PathBuf, UpgradeRecord)>, Vec<String>) {
    let mut good = Vec::new();
    let mut bad = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (good, bad);
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().is_some_and(|x| x == "json")
                && p.file_name().is_some_and(|n| n != IN_FLIGHT_FILE)
        })
        .collect();
    paths.sort();
    for p in paths {
        match read_record(&p) {
            Ok(r) => good.push((p, r)),
            Err(e) => bad.push(e),
        }
    }
    (good, bad)
}

/// The newest record still `in_flight` or `probation_failed` — the one a
/// booting successor or a firing guard is about.
pub fn latest_open_record(dir: &Path) -> Option<(PathBuf, UpgradeRecord)> {
    all_records(dir)
        .0
        .into_iter()
        .rev()
        .find(|(_, r)| r.state.guard_should_act())
}

/// A sortable, file-name-safe upgrade id: `20261124T221500.123Z`.
pub fn new_id(now: chrono::DateTime<chrono::Utc>) -> String {
    now.format("%Y%m%dT%H%M%S%.3fZ").to_string()
}

pub fn rfc3339(now: chrono::DateTime<chrono::Utc>) -> String {
    now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
pub(crate) fn sample_record(dir: &Path) -> UpgradeRecord {
    UpgradeRecord {
        schema: RECORD_SCHEMA,
        id: "20261124T220000.000Z".into(),
        state: UpgradeState::InFlight,
        created_at: "2026-11-24T22:00:00Z".into(),
        from_sha: "4d524e71bcc3".into(),
        to_sha: "9f00aa11bb22".into(),
        from_dir: dir.join("4d524e71b"),
        to_dir: dir.join("9f00aa11b"),
        current_link: dir.join("current"),
        rollback: false,
        api_port: 9911,
        from_pid: 4242,
        restart: RestartTarget::SystemdUnit {
            unit: "qr-scratch-9911.service".into(),
        },
        guard_unit: Some("qontinui-runner-upgrade-guard-test".into()),
        guard_pid: None,
        guard_delay_secs: 180,
        deadline: "2026-11-24T22:03:00Z".into(),
        holders: vec![HolderSnapshot {
            pane_id: "pane-1".into(),
            holder_pid: Some(100),
            child_pid: Some(101),
            negotiated_version: Some(3),
            session_ids: vec!["11111111-1111-1111-1111-111111111111".into()],
        }],
        dies_with_runner: vec![],
        from_pg_schema: Some("md5:aa/10".into()),
        to_pg_schema: None,
        probation: None,
        outcome_reason: None,
        pg_schema_unverified: false,
        concluded_at: None,
        reported_state: None,
        reported_at: None,
        preflight: serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_has_exactly_the_phase4_fields() {
        let d = tempfile::tempdir().unwrap();
        let r = sample_record(d.path());
        write_marker(d.path(), &InFlightMarker::for_record(&r)).unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(marker_path(d.path())).unwrap()).unwrap();
        let mut keys: Vec<&str> = raw
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "deadline",
                "from_sha",
                "port",
                "started_at",
                "state",
                "to_sha"
            ]
        );
        assert_eq!(raw["state"], "in_flight");
        assert_eq!(raw["port"], 9911);
        remove_marker(d.path()).unwrap();
        remove_marker(d.path()).unwrap();
        assert!(read_marker(d.path()).is_none());
    }

    #[test]
    fn records_round_trip_and_the_latest_open_one_is_found() {
        let d = tempfile::tempdir().unwrap();
        let mut old = sample_record(d.path());
        old.id = "20261123T000000.000Z".into();
        old.state = UpgradeState::ProbationPassed;
        write_record(&record_path(d.path(), &old.id), &old).unwrap();
        let open = sample_record(d.path());
        write_record(&record_path(d.path(), &open.id), &open).unwrap();
        // The marker is not a record.
        write_marker(d.path(), &InFlightMarker::for_record(&open)).unwrap();
        let (all, bad) = all_records(d.path());
        assert_eq!(all.len(), 2);
        assert!(bad.is_empty());
        let (p, r) = latest_open_record(d.path()).unwrap();
        assert_eq!(r, open);
        assert_eq!(p, record_path(d.path(), &open.id));
    }

    #[test]
    fn ids_sort_by_time() {
        let a = new_id(
            chrono::DateTime::parse_from_rfc3339("2026-11-24T09:00:00Z")
                .unwrap()
                .into(),
        );
        let b = new_id(
            chrono::DateTime::parse_from_rfc3339("2026-11-24T10:00:00Z")
                .unwrap()
                .into(),
        );
        assert!(a < b, "{a} {b}");
    }
}
