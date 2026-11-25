//! Durable store for the component baselines `matches_isolated_render` reads
//! (plan `2026-11-13-ui-bridge-matches-isolated-render-assertion`, Phase 3).
//!
//! A component baseline is a [`ComponentBaseline`]: the structural capture of
//! an isolated render (a Storybook story) plus its provenance. It is captured
//! on one page and asserted on another, so unlike the in-memory bbox
//! baselines in `ApiState::vision_baselines` (deliberately non-durable) it
//! must survive a runner restart.
//!
//! Layout: `<vision cache root>/component_baselines/<sha256(name)>.json`, one
//! file per baseline, the `name` kept INSIDE the file. The name is never
//! joined into a path: it conventionally contains `/`
//! (`storybook/DateRangePicker--default@<sha>`). The vision cache root is the
//! per-instance `runner_data_local_base()`-derived directory (served claim
//! `b8-runner-vision-cache-lives-in-the-instance-data-dir`), so a temp runner
//! never overwrites the primary's baselines.
//!
//! Writes are atomic: the JSON is written to a unique temp file in the same
//! directory and renamed over the target.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use qontinui_vision_core::ComponentBaseline;
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Directory under the vision cache root that holds the baselines.
pub const COMPONENT_BASELINES_DIR_NAME: &str = "component_baselines";

/// Longest baseline name accepted, in bytes.
pub const MAX_COMPONENT_BASELINE_NAME_BYTES: usize = 256;

/// Why a baseline name was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameRefusal {
    Empty,
    TooLong { bytes: usize },
    ControlCharacter,
}

impl std::fmt::Display for NameRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "component baseline name is empty"),
            Self::TooLong { bytes } => write!(
                f,
                "component baseline name is {bytes} bytes; the limit is \
                 {MAX_COMPONENT_BASELINE_NAME_BYTES}"
            ),
            Self::ControlCharacter => {
                write!(f, "component baseline name contains a control character")
            }
        }
    }
}

/// Refuse an empty name, one over [`MAX_COMPONENT_BASELINE_NAME_BYTES`], or
/// one containing a control character.
pub fn validate_name(name: &str) -> Result<(), NameRefusal> {
    if name.is_empty() {
        return Err(NameRefusal::Empty);
    }
    if name.len() > MAX_COMPONENT_BASELINE_NAME_BYTES {
        return Err(NameRefusal::TooLong { bytes: name.len() });
    }
    if name.chars().any(char::is_control) {
        return Err(NameRefusal::ControlCharacter);
    }
    Ok(())
}

/// The file stem a name is stored under: the lowercase hex sha256 of its
/// UTF-8 bytes.
pub fn file_stem_for(name: &str) -> String {
    hex::encode(Sha256::digest(name.as_bytes()))
}

/// A store failure.
#[derive(Debug)]
pub enum StoreError {
    /// The name was refused before touching the disk.
    InvalidName(NameRefusal),
    /// Disk I/O or (de)serialization failed.
    Io(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidName(r) => write!(f, "{r}"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

/// One listed baseline: provenance only, never the capture body.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ComponentBaselineSummary {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub story_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The captured component or element id, when the capture names one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    pub node_count: usize,
}

impl From<&ComponentBaseline> for ComponentBaselineSummary {
    fn from(b: &ComponentBaseline) -> Self {
        Self {
            name: b.name.clone(),
            story_id: b.story_id.clone(),
            source_sha: b.effective_source_sha().map(str::to_string),
            captured_at: b.captured_at,
            target_id: b.capture.target.as_ref().map(|t| t.id.clone()),
            node_count: b.capture.nodes.len(),
        }
    }
}

/// The result of listing the store: the readable baselines, plus the files
/// that could not be read (reported, never silently dropped).
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ComponentBaselineListing {
    pub baselines: Vec<ComponentBaselineSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unreadable: Vec<String>,
}

/// The on-disk store. Holds only its directory, so every read goes to disk
/// and a fresh instance sees exactly what an earlier one wrote.
#[derive(Debug, Clone)]
pub struct ComponentBaselineStore {
    dir: PathBuf,
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The store as the component-baseline routes' handler state: extracted from
/// the runner's `ApiState`, and mountable on its own in a test router.
#[derive(Debug, Clone)]
pub struct ComponentBaselines(pub std::sync::Arc<ComponentBaselineStore>);

impl axum::extract::FromRef<std::sync::Arc<crate::mcp::types::ApiState>> for ComponentBaselines {
    fn from_ref(state: &std::sync::Arc<crate::mcp::types::ApiState>) -> Self {
        Self(state.vision_component_baselines.clone())
    }
}

impl ComponentBaselineStore {
    /// A store rooted at `<vision_cache_root>/component_baselines`. Creates
    /// nothing until the first write.
    pub fn under_vision_cache_root(vision_cache_root: &Path) -> Self {
        Self {
            dir: vision_cache_root.join(COMPONENT_BASELINES_DIR_NAME),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_for(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.json", file_stem_for(name)))
    }

    /// Store `baseline` under its own `name`, replacing any earlier one with
    /// the same name. Atomic and durable: the temp file is written and synced
    /// before the rename, so a crash leaves either the old file or the new
    /// one whole.
    pub fn put(&self, baseline: &ComponentBaseline) -> Result<PathBuf, StoreError> {
        validate_name(&baseline.name).map_err(StoreError::InvalidName)?;
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| StoreError::Io(format!("create {}: {e}", self.dir.display())))?;
        let bytes = serde_json::to_vec_pretty(baseline)
            .map_err(|e| StoreError::Io(format!("serialize baseline: {e}")))?;
        let target = self.path_for(&baseline.name);
        let tmp = self.dir.join(format!(
            ".{}.{}.{}.tmp",
            file_stem_for(&baseline.name),
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let written = std::fs::File::create(&tmp).and_then(|mut f| {
            use std::io::Write;
            f.write_all(&bytes)?;
            f.sync_all()
        });
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(StoreError::Io(format!("write {}: {e}", tmp.display())));
        }
        if let Err(e) = std::fs::rename(&tmp, &target) {
            let _ = std::fs::remove_file(&tmp);
            return Err(StoreError::Io(format!(
                "rename {} -> {}: {e}",
                tmp.display(),
                target.display()
            )));
        }
        // Make the rename itself durable: sync the directory entry. Best
        // effort — the file is already whole on disk — and Unix only, where a
        // directory can be opened and synced.
        #[cfg(unix)]
        let _ = std::fs::File::open(&self.dir).and_then(|dir| dir.sync_all());
        Ok(target)
    }

    /// Read the baseline stored under `name`. `Ok(None)` when none is stored.
    /// A file whose inner `name` disagrees (a hash collision, or a hand-edited
    /// file) is an error, never served under the wrong name.
    pub fn get(&self, name: &str) -> Result<Option<ComponentBaseline>, StoreError> {
        validate_name(name).map_err(StoreError::InvalidName)?;
        let path = self.path_for(name);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StoreError::Io(format!("read {}: {e}", path.display()))),
        };
        let baseline: ComponentBaseline = serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Io(format!("parse {}: {e}", path.display())))?;
        if baseline.name != name {
            return Err(StoreError::Io(format!(
                "{} holds baseline '{}', not '{name}'",
                path.display(),
                baseline.name
            )));
        }
        Ok(Some(baseline))
    }

    /// Every stored baseline's summary, sorted by name. A missing directory
    /// is an empty store (nothing has been written yet).
    pub fn list(&self) -> Result<ComponentBaselineListing, StoreError> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ComponentBaselineListing::default())
            }
            Err(e) => return Err(StoreError::Io(format!("read {}: {e}", self.dir.display()))),
        };
        let mut listing = ComponentBaselineListing::default();
        for entry in entries.flatten() {
            let path = entry.path();
            let is_baseline = path.extension().and_then(|e| e.to_str()) == Some("json")
                && !path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('.'));
            if !is_baseline {
                continue;
            }
            match std::fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(|b| {
                    serde_json::from_slice::<ComponentBaseline>(&b).map_err(|e| e.to_string())
                }) {
                Ok(b) => listing.baselines.push(ComponentBaselineSummary::from(&b)),
                Err(e) => listing.unreadable.push(format!("{}: {e}", path.display())),
            }
        }
        listing.baselines.sort_by(|a, b| a.name.cmp(&b.name));
        listing.unreadable.sort();
        Ok(listing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_vision_core::ComponentCapture;

    fn baseline(name: &str) -> ComponentBaseline {
        ComponentBaseline {
            name: name.to_string(),
            story_id: Some("DateRangePicker--default".to_string()),
            source_sha: Some("abc123".to_string()),
            captured_at: None,
            capture: ComponentCapture::default(),
        }
    }

    #[test]
    fn put_then_get_round_trips_and_keeps_the_name_inside_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ComponentBaselineStore::under_vision_cache_root(tmp.path());
        let name = "storybook/DateRangePicker--default@abc123";
        let path = store.put(&baseline(name)).unwrap();
        // Stored under the hash, never the raw name.
        assert_eq!(
            path,
            tmp.path()
                .join("component_baselines")
                .join(format!("{}.json", file_stem_for(name)))
        );
        assert!(std::fs::read_to_string(&path).unwrap().contains(name));
        assert_eq!(store.get(name).unwrap(), Some(baseline(name)));
        assert_eq!(store.get("other").unwrap(), None);
    }

    #[test]
    fn a_fresh_store_instance_rereads_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let name = "storybook/Button--primary@deadbeef";
        ComponentBaselineStore::under_vision_cache_root(tmp.path())
            .put(&baseline(name))
            .unwrap();
        let fresh = ComponentBaselineStore::under_vision_cache_root(tmp.path());
        assert_eq!(fresh.get(name).unwrap(), Some(baseline(name)));
        let listing = fresh.list().unwrap();
        assert_eq!(listing.baselines.len(), 1);
        assert_eq!(listing.baselines[0].name, name);
        assert!(listing.unreadable.is_empty());
    }

    #[test]
    fn put_replaces_and_leaves_no_temp_files() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ComponentBaselineStore::under_vision_cache_root(tmp.path());
        let mut b = baseline("x");
        store.put(&b).unwrap();
        b.source_sha = Some("new".to_string());
        store.put(&b).unwrap();
        assert_eq!(
            store.get("x").unwrap().unwrap().source_sha.as_deref(),
            Some("new")
        );
        let files: Vec<_> = std::fs::read_dir(store.dir()).unwrap().flatten().collect();
        assert_eq!(files.len(), 1, "{files:?}");
    }

    #[test]
    fn unsafe_names_are_refused_before_touching_the_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ComponentBaselineStore::under_vision_cache_root(tmp.path());
        let long = "a".repeat(MAX_COMPONENT_BASELINE_NAME_BYTES + 1);
        for bad in ["", "line\nbreak", "nul\0byte", "tab\tname", long.as_str()] {
            assert!(
                matches!(store.put(&baseline(bad)), Err(StoreError::InvalidName(_))),
                "{bad:?} was accepted by put"
            );
            assert!(
                matches!(store.get(bad), Err(StoreError::InvalidName(_))),
                "{bad:?} was accepted by get"
            );
        }
        assert!(
            !store.dir().exists(),
            "a refused name created the store dir"
        );
        // The boundary itself, and path-like names, are fine: the name is
        // never a path.
        let max = "b".repeat(MAX_COMPONENT_BASELINE_NAME_BYTES);
        store.put(&baseline(&max)).unwrap();
        store.put(&baseline("../../etc/passwd")).unwrap();
        assert!(store.get("../../etc/passwd").unwrap().is_some());
        assert_eq!(std::fs::read_dir(store.dir()).unwrap().count(), 2);
    }

    #[test]
    fn a_file_whose_inner_name_disagrees_is_not_served() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ComponentBaselineStore::under_vision_cache_root(tmp.path());
        let path = store.put(&baseline("real")).unwrap();
        let forged = store.dir().join(format!("{}.json", file_stem_for("asked")));
        std::fs::rename(path, &forged).unwrap();
        assert!(matches!(store.get("asked"), Err(StoreError::Io(_))));
    }

    #[test]
    fn an_unreadable_file_is_listed_as_unreadable() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ComponentBaselineStore::under_vision_cache_root(tmp.path());
        store.put(&baseline("ok")).unwrap();
        std::fs::write(store.dir().join("garbage.json"), b"{not json").unwrap();
        let listing = store.list().unwrap();
        assert_eq!(listing.baselines.len(), 1);
        assert_eq!(listing.unreadable.len(), 1);
    }

    #[test]
    fn an_empty_store_lists_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let listing = ComponentBaselineStore::under_vision_cache_root(tmp.path())
            .list()
            .unwrap();
        assert!(listing.baselines.is_empty());
    }
}
