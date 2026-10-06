//! The broker's files under `~/.qontinui/build-admission/` (plan D5, D8, D4):
//!
//! | File | Writer | Content |
//! |---|---|---|
//! | `state.json` | broker | the ledger: live and recent tickets (secret HASHES only) and measurement history; re-adopted on start |
//! | `estimates.json` | broker | per-key estimates and the non-build p95, for the wrappers' degraded arm (Phase 3) |
//! | `seeds.json` | operator / Phase 0 | labelled seeds `[{key, bytes}]` until 3 measurements exist |
//! | `overrides.toml` | operator / per-host rollout | level + parameters, highest file layer |
//! | `policy.json` | Phase 7 (coord's desired payload) | level + parameters, cached |
//!
//! Every write is an atomic owner-only rename, so a crash never leaves a torn
//! file and another user never reads one. An unparseable `state.json` is moved
//! aside (never silently overwritten) and the broker starts empty.

use std::path::{Path, PathBuf};

use qontinui_types::build_admission::{
    estimate::{estimate, Measurement, Seed},
    EstimateKey, EstimateSource, Fact, Policy,
};
use serde::{Deserialize, Serialize};

use super::broker::Broker;

pub fn dir() -> Option<PathBuf> {
    qontinui_runner_lib::ambient::qontinui_dir().map(|d| d.join("build-admission"))
}

fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    crate::fs_atomic::atomic_write_owner_only(path, bytes)
}

/// Persist the ledger.
pub fn save_state(dir: &Path, broker: &Broker) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(broker).map_err(std::io::Error::other)?;
    write(&dir.join("state.json"), &bytes)
}

/// Load the ledger. Returns the broker and a note when the file was unusable.
pub fn load_state(dir: &Path) -> (Broker, Option<String>) {
    let path = dir.join("state.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return (Broker::default(), None);
    };
    match serde_json::from_str::<Broker>(&text) {
        Ok(b) => (b, None),
        Err(e) => {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let aside = dir.join(format!("state.json.unreadable-{stamp}"));
            let _ = std::fs::rename(&path, &aside);
            (
                Broker::default(),
                Some(format!(
                    "state.json unreadable ({e}); moved to {}",
                    aside.display()
                )),
            )
        }
    }
}

#[derive(Debug, Deserialize)]
struct SeedRow {
    key: EstimateKey,
    bytes: u64,
}

/// Labelled seeds from `seeds.json`; absent or unreadable = none (estimates
/// then read `unknown`, never a made-up number).
pub fn load_seeds(dir: &Path) -> (Vec<Seed>, Option<String>) {
    let Ok(text) = std::fs::read_to_string(dir.join("seeds.json")) else {
        return (Vec::new(), None);
    };
    match serde_json::from_str::<Vec<SeedRow>>(&text) {
        Ok(rows) => (
            rows.into_iter()
                .map(|r| Seed {
                    key: r.key,
                    bytes: r.bytes,
                })
                .collect(),
            None,
        ),
        Err(e) => (
            Vec::new(),
            Some(format!("seeds.json unreadable, ignored: {e}")),
        ),
    }
}

/// One row of `estimates.json`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EstimateRow {
    pub key: EstimateKey,
    pub bytes: Option<u64>,
    pub source: EstimateSource,
}

#[derive(Debug, Serialize)]
pub struct EstimatesFile {
    pub written_at_s: u64,
    pub non_build_p95_bytes: Fact<u64>,
    pub estimates: Vec<EstimateRow>,
}

/// Every key the broker has a measurement or a seed for, estimated.
pub fn estimates(broker: &Broker, policy: &Policy) -> Vec<EstimateRow> {
    let history: Vec<Measurement> = broker.history.iter().map(Into::into).collect();
    let mut keys: Vec<EstimateKey> = history
        .iter()
        .map(|m| m.key.clone())
        .chain(broker.seeds.iter().map(|s| s.key.clone()))
        .collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .map(|k| {
            let e = estimate(&k, &history, &broker.seeds, None, policy);
            EstimateRow {
                key: k,
                bytes: e.bytes,
                source: e.source,
            }
        })
        .collect()
}

pub fn save_estimates(dir: &Path, file: &EstimatesFile) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(file).map_err(std::io::Error::other)?;
    write(&dir.join("estimates.json"), &bytes)
}

/// A config file read: absent is a fact (`Ok(None)`); any other failure is an
/// error the caller must not mistake for absence (D8: an unreadable layer
/// keeps its last-known value).
pub fn read_text(dir: &Path, name: &str) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(dir.join(name)) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::super::broker::{hash_secret, TicketRequest};
    use super::*;
    use qontinui_types::build_admission::{
        HostFacts, MeasureMethod, Subcommand, TargetDirKind, GIB,
    };

    fn facts() -> HostFacts {
        HostFacts {
            mem_total_bytes: 64 * GIB,
            cpus: 8,
            mem_available_bytes: Fact::Measured(32 * GIB),
            psi_mem_full_avg10: Fact::NotSupported,
            non_build_p95_bytes: Fact::LiveOnly(GIB),
            ci_reservation_bytes: Fact::Measured(0),
            unleased_build_rss_bytes: Fact::Measured(0),
            admissions_paused: false,
        }
    }

    #[test]
    fn state_round_trips_with_hashes_only_and_a_bad_file_is_moved_aside() {
        let d = tempfile::tempdir().unwrap();
        let mut b = Broker::default();
        let req = TicketRequest {
            repo: "r".into(),
            subcommand: Subcommand::Check,
            profile: "dev".into(),
            output_dir: "o".into(),
            target_dir_kind: TargetDirKind::PrivateCold,
            requested_jobs: None,
            class: None,
            session_id: None,
            worktree: None,
            pid: 7,
            pr: None,
        };
        b.open(
            "t1".into(),
            hash_secret("s3cret"),
            req,
            Some(9),
            &facts(),
            &Policy::default(),
            5,
        )
        .unwrap();
        save_state(d.path(), &b).unwrap();
        let text = std::fs::read_to_string(d.path().join("state.json")).unwrap();
        assert!(!text.contains("s3cret"));
        let (back, note) = load_state(d.path());
        assert!(note.is_none());
        assert!(back.authorize("t1", "s3cret").is_ok());

        std::fs::write(d.path().join("state.json"), "{not json").unwrap();
        let (empty, note) = load_state(d.path());
        assert!(empty.records.is_empty());
        assert!(note.unwrap().contains("unreadable"));
        assert!(std::fs::read_dir(d.path()).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("state.json.unreadable-")));
    }

    #[test]
    fn seeds_feed_estimates_with_their_label() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("seeds.json"),
            r#"[{"key":{"repo":"qontinui-coord","subcommand":"test","profile":"dev","target_dir_kind":"shared_warm","measure_method":"rss_sample"},"bytes":30000000000}]"#,
        )
        .unwrap();
        let (seeds, note) = load_seeds(d.path());
        assert!(note.is_none());
        let b = Broker {
            seeds,
            ..Default::default()
        };
        let rows = estimates(&b, &Policy::default());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key.measure_method, MeasureMethod::RssSample);
        assert_eq!(
            (rows[0].bytes, rows[0].source),
            (Some(30_000_000_000), EstimateSource::Seed)
        );
        std::fs::write(d.path().join("seeds.json"), "[{").unwrap();
        assert!(load_seeds(d.path()).1.is_some());
    }
}
