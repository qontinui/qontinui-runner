//! Wrapper registry: filesystem scan + JSON-file cache + change watcher.
//!
//! On startup, scans `<APP_DATA>/qontinui-runner/wrappers/<id>/` for
//! installed wrappers. For each subdirectory it runs
//! `node dist/index-node.js --manifest-only`, parses the resulting
//! `{ manifestVersion, manifest, actions }` payload, and stores it in an
//! in-memory cache backed by `wrappers-index.json` next to the install
//! root. A `notify` watcher keeps the cache in sync with on-disk changes
//! (install / update / uninstall mutate the directory).
//!
//! ## Storage choice (deviation from plan)
//!
//! The plan calls for a SQLite table. The runner has no SQLite
//! infrastructure — Postgres is the operational store, and adding rusqlite
//! purely for wrapper bookkeeping introduces a new dependency, build flag
//! and migration surface for ~100 rows of metadata. We instead persist the
//! cache as a JSON index file (`wrappers-index.json`) co-located with the
//! install root, mirroring how `config_storage.rs` already persists the
//! config metadata index. Reads are served from memory; writes flush the
//! whole index atomically. Migrating to a real DB later requires only
//! swapping the `Persistence` impl.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

use super::manifest::{
    validate_manifest, ActionDescriptor, ManifestOnlyOutput, WrapperManifest,
    SUPPORTED_MANIFEST_VERSION,
};

/// Filename of the persistence index inside the wrappers root.
const INDEX_FILE: &str = "wrappers-index.json";

/// Max stdout bytes accepted from `--manifest-only`. Defensive bound to
/// avoid an OOM if a wrapper's stdout goes feral.
const MAX_MANIFEST_STDOUT_BYTES: usize = 256 * 1024;

/// Timeout applied to `node dist/index-node.js --manifest-only`. The plan
/// calls for sub-500ms; we allow 10s for cold-start variance.
const MANIFEST_ONLY_TIMEOUT: Duration = Duration::from_secs(10);

/// Debounce window for filesystem events before triggering a rescan. The
/// `notify` crate fires multiple events per logical install (mkdir,
/// node_modules churn, ...); 500ms is enough to coalesce a `pnpm install`
/// burst without making the UI feel stale.
pub const RESCAN_DEBOUNCE: Duration = Duration::from_millis(500);

/// Cached wrapper entry. Mirrors the plan's `wrappers` table 1:1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wrapper {
    pub id: String,
    pub package_name: String,
    pub version: String,
    pub manifest: WrapperManifest,
    pub actions: Vec<ActionDescriptor>,
    pub install_path: String,
    /// Unix epoch seconds.
    pub installed_at: i64,
    /// Unix epoch seconds.
    pub updated_at: i64,
}

/// On-disk representation of the registry index. Kept in a single file so
/// updates are atomic via `write` + rename.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedIndex {
    #[serde(default)]
    wrappers: HashMap<String, Wrapper>,
}

/// Errors raised by the registry.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("wrapper '{0}' not found")]
    NotFound(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("manifest-only invocation failed: {0}")]
    Spawn(String),
    #[error("manifest-only output is not valid JSON: {0}")]
    BadJson(String),
    #[error("manifest validation failed: {0}")]
    Validation(String),
    #[error("manifest version {found} is newer than supported {supported}")]
    UnsupportedVersion { found: u32, supported: u32 },
}

/// The wrapper registry. Cheaply clonable via `Arc`.
pub struct WrapperRegistry {
    root: PathBuf,
    inner: RwLock<PersistedIndex>,
    /// File watcher kept alive for the lifetime of the registry; dropping
    /// it stops the underlying inotify/ReadDirectoryChanges watch.
    _watcher: std::sync::Mutex<Option<RecommendedWatcher>>,
}

impl WrapperRegistry {
    /// Build a registry rooted at `root` and trigger an initial rescan.
    /// The directory is created if it doesn't exist.
    pub async fn new(root: PathBuf) -> Arc<Self> {
        if let Err(e) = std::fs::create_dir_all(&root) {
            warn!(
                "wrappers: failed to create root '{}': {} — registry will be empty",
                root.display(),
                e
            );
        }
        let registry = Arc::new(Self {
            root: root.clone(),
            inner: RwLock::new(PersistedIndex::default()),
            _watcher: std::sync::Mutex::new(None),
        });
        // Prime from disk if an index already exists.
        if let Err(e) = registry.load_index().await {
            warn!("wrappers: failed to load index: {}", e);
        }
        // Initial fs rescan to reconcile the cache with reality.
        if let Err(e) = registry.rescan_all().await {
            warn!("wrappers: initial rescan_all failed: {}", e);
        }
        registry
    }

    /// Default app-data root: `<data_dir>/qontinui-runner/wrappers/`.
    /// Falls back to `./qontinui-runner/wrappers` if `dirs::data_dir` is
    /// unavailable (highly unusual).
    pub fn default_root() -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("qontinui-runner")
            .join("wrappers")
    }

    /// Returns the wrappers root directory. Useful for installer / uninstall
    /// code that needs to compute install paths.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns all currently-cached wrappers.
    pub async fn get_all(&self) -> Vec<Wrapper> {
        self.inner
            .read()
            .await
            .wrappers
            .values()
            .cloned()
            .collect()
    }

    /// Look up a wrapper by id.
    pub async fn get(&self, id: &str) -> Option<Wrapper> {
        self.inner.read().await.wrappers.get(id).cloned()
    }

    /// Re-scan a single wrapper directory and update the cache.
    pub async fn rescan(&self, id: &str) -> Result<(), RegistryError> {
        let wrapper_dir = self.root.join(id);
        if !wrapper_dir.is_dir() {
            // Directory gone — treat as uninstall.
            self.remove(id).await;
            return Ok(());
        }
        match scan_one(&wrapper_dir).await {
            Ok(wrapper) => {
                let mut idx = self.inner.write().await;
                let installed_at = idx
                    .wrappers
                    .get(id)
                    .map(|w| w.installed_at)
                    .unwrap_or_else(|| Utc::now().timestamp());
                let mut wrapper = wrapper;
                wrapper.installed_at = installed_at;
                wrapper.updated_at = Utc::now().timestamp();
                idx.wrappers.insert(wrapper.id.clone(), wrapper);
                drop(idx);
                self.save_index().await?;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Re-scan every subdirectory of the wrappers root.
    pub async fn rescan_all(&self) -> Result<(), RegistryError> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };

        let mut seen: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let dir_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            // Skip dotfiles and the index file itself.
            if dir_name.starts_with('.') {
                continue;
            }
            match scan_one(&path).await {
                Ok(wrapper) => {
                    let id = wrapper.id.clone();
                    let mut idx = self.inner.write().await;
                    let installed_at = idx
                        .wrappers
                        .get(&id)
                        .map(|w| w.installed_at)
                        .unwrap_or_else(|| Utc::now().timestamp());
                    let mut wrapper = wrapper;
                    wrapper.installed_at = installed_at;
                    wrapper.updated_at = Utc::now().timestamp();
                    idx.wrappers.insert(id.clone(), wrapper);
                    seen.push(id);
                }
                Err(e) => warn!(
                    "wrappers: scan of '{}' failed, skipping: {}",
                    path.display(),
                    e
                ),
            }
        }

        // Drop entries whose directories disappeared.
        let mut idx = self.inner.write().await;
        idx.wrappers.retain(|id, _| seen.iter().any(|s| s == id));
        drop(idx);
        self.save_index().await?;
        Ok(())
    }

    /// Drop a wrapper from the cache (used by uninstall).
    pub async fn remove(&self, id: &str) {
        let mut idx = self.inner.write().await;
        idx.wrappers.remove(id);
        drop(idx);
        if let Err(e) = self.save_index().await {
            warn!("wrappers: save_index after remove('{}') failed: {}", id, e);
        }
    }

    /// Spawn the filesystem watcher. Idempotent — calling this twice will
    /// replace the previous watcher (the prior one is dropped, ending its
    /// task). Caller must keep the returned `Arc<Self>` alive.
    pub fn start_watcher(self: &Arc<Self>) {
        let registry = Arc::clone(self);
        let root = self.root.clone();
        let (tx, mut rx) = mpsc::channel::<PathBuf>(32);

        let watcher = match notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                match event.kind {
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
                        for path in event.paths {
                            let _ = tx.try_send(path);
                        }
                    }
                    _ => {}
                }
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                warn!(
                    "wrappers: failed to create watcher for '{}': {}",
                    root.display(),
                    e
                );
                return;
            }
        };
        let mut watcher = watcher;
        if let Err(e) = watcher.watch(&root, RecursiveMode::NonRecursive) {
            warn!(
                "wrappers: failed to watch '{}': {} — registry will not auto-update",
                root.display(),
                e
            );
            return;
        }

        // Stash the watcher so it stays alive.
        if let Ok(mut guard) = self._watcher.lock() {
            *guard = Some(watcher);
        }

        // Debounce + rescan task.
        tokio::spawn(async move {
            loop {
                let path = match rx.recv().await {
                    Some(p) => p,
                    None => return,
                };
                // Drain pending events within the debounce window.
                tokio::time::sleep(RESCAN_DEBOUNCE).await;
                while rx.try_recv().is_ok() {}

                let id = path
                    .strip_prefix(&registry.root)
                    .ok()
                    .and_then(|p| p.components().next())
                    .and_then(|c| c.as_os_str().to_str())
                    .map(|s| s.to_string());

                match id {
                    Some(id) if !id.starts_with('.') && id != INDEX_FILE => {
                        debug!("wrappers: filesystem change in '{}', rescanning", id);
                        if let Err(e) = registry.rescan(&id).await {
                            warn!("wrappers: rescan('{}') failed: {}", id, e);
                        }
                    }
                    _ => {
                        // Top-level mutation we can't attribute — full rescan.
                        if let Err(e) = registry.rescan_all().await {
                            warn!("wrappers: rescan_all failed: {}", e);
                        }
                    }
                }
            }
        });
    }

    async fn load_index(&self) -> Result<(), RegistryError> {
        let path = self.root.join(INDEX_FILE);
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<PersistedIndex>(&bytes) {
                Ok(idx) => {
                    *self.inner.write().await = idx;
                    Ok(())
                }
                Err(e) => {
                    warn!(
                        "wrappers: index '{}' is corrupt ({}), starting empty",
                        path.display(),
                        e
                    );
                    Ok(())
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    async fn save_index(&self) -> Result<(), RegistryError> {
        let path = self.root.join(INDEX_FILE);
        let tmp = self.root.join(format!("{}.tmp", INDEX_FILE));
        let snapshot = self.inner.read().await;
        let bytes = serde_json::to_vec_pretty(&*snapshot).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
        })?;
        std::fs::write(&tmp, &bytes)?;
        // Best-effort atomic replace.
        if let Err(e) = std::fs::rename(&tmp, &path) {
            // On Windows rename fails if the target exists in some FS configs;
            // fall back to a non-atomic write.
            warn!(
                "wrappers: atomic rename of index failed ({}), writing directly",
                e
            );
            std::fs::write(&path, &bytes)?;
            let _ = std::fs::remove_file(&tmp);
        }
        Ok(())
    }
}

/// Resolve a wrapper directory's `package.json` and run `--manifest-only`,
/// returning a populated `Wrapper`.
///
/// Public so the install pipeline can fetch a manifest immediately after
/// a `pnpm install` finishes without going through the cache.
pub async fn scan_one(wrapper_dir: &Path) -> Result<Wrapper, RegistryError> {
    let entry_rel = Path::new("dist").join("index-node.js");
    let entry = wrapper_dir.join(&entry_rel);
    if !entry.exists() {
        return Err(RegistryError::Spawn(format!(
            "wrapper directory '{}' has no dist/index-node.js — is it built?",
            wrapper_dir.display()
        )));
    }

    let pkg_path = wrapper_dir.join("package.json");
    let (pkg_name, pkg_version) = read_package_json(&pkg_path).unwrap_or_else(|e| {
        warn!(
            "wrappers: package.json read failed for '{}': {}",
            wrapper_dir.display(),
            e
        );
        (String::new(), String::new())
    });

    let stdout = run_manifest_only(wrapper_dir, &entry).await?;
    let parsed: ManifestOnlyOutput = serde_json::from_str(&stdout)
        .map_err(|e| RegistryError::BadJson(format!("{}: {}", e, truncate(&stdout, 200))))?;

    if parsed.manifest_version > SUPPORTED_MANIFEST_VERSION {
        return Err(RegistryError::UnsupportedVersion {
            found: parsed.manifest_version,
            supported: SUPPORTED_MANIFEST_VERSION,
        });
    }

    validate_manifest(&parsed.manifest).map_err(RegistryError::Validation)?;

    let now = Utc::now().timestamp();
    Ok(Wrapper {
        id: parsed.manifest.id.clone(),
        package_name: pkg_name,
        version: pkg_version,
        manifest: parsed.manifest,
        actions: parsed.actions,
        install_path: wrapper_dir.to_string_lossy().to_string(),
        installed_at: now,
        updated_at: now,
    })
}

/// Read `name` and `version` from a `package.json`.
fn read_package_json(path: &Path) -> Result<(String, String), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let name = v
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string();
    let version = v
        .get("version")
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string();
    Ok((name, version))
}

/// Run `node <entry> --manifest-only` and capture stdout.
async fn run_manifest_only(cwd: &Path, entry: &Path) -> Result<String, RegistryError> {
    let cwd = cwd.to_path_buf();
    let entry = entry.to_path_buf();
    let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let mut cmd = Command::new("node");
        cmd.current_dir(&cwd)
            .arg(&entry)
            .arg("--manifest-only")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let output = cmd.output().map_err(|e| format!("spawn node: {}", e))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "node {:?} --manifest-only exited with {}: {}",
                entry,
                output.status,
                truncate(stderr.as_ref(), 400)
            ));
        }
        if output.stdout.len() > MAX_MANIFEST_STDOUT_BYTES {
            return Err(format!(
                "manifest-only stdout too large ({} bytes, max {})",
                output.stdout.len(),
                MAX_MANIFEST_STDOUT_BYTES
            ));
        }
        let s = String::from_utf8_lossy(&output.stdout).to_string();
        // The contract is one line of JSON; trim leading/trailing whitespace
        // so a stray newline before flush doesn't break parsing.
        Ok(s.trim().to_string())
    });

    let joined =
        tokio::time::timeout(MANIFEST_ONLY_TIMEOUT, result)
            .await
            .map_err(|_| {
                RegistryError::Spawn(format!(
                    "manifest-only timed out after {:?}",
                    MANIFEST_ONLY_TIMEOUT
                ))
            })?;

    match joined {
        Ok(Ok(stdout)) => Ok(stdout),
        Ok(Err(e)) => Err(RegistryError::Spawn(e)),
        Err(e) => Err(RegistryError::Spawn(format!("join error: {}", e))),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fresh_registry_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = WrapperRegistry::new(tmp.path().to_path_buf()).await;
        assert!(reg.get_all().await.is_empty());
    }

    #[tokio::test]
    async fn rescan_missing_dir_removes_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = WrapperRegistry::new(tmp.path().to_path_buf()).await;
        // Inject a synthetic entry, then rescan an id with no directory.
        {
            let mut idx = reg.inner.write().await;
            idx.wrappers.insert(
                "ghost".to_string(),
                Wrapper {
                    id: "ghost".to_string(),
                    package_name: "p".to_string(),
                    version: "0.0.0".to_string(),
                    manifest: WrapperManifest {
                        id: "ghost".to_string(),
                        display_name: "g".to_string(),
                        description: String::new(),
                        transport: "api".to_string(),
                        categories: vec![],
                        env_vars: vec![],
                    },
                    actions: vec![],
                    install_path: tmp.path().join("ghost").to_string_lossy().to_string(),
                    installed_at: 0,
                    updated_at: 0,
                },
            );
        }
        reg.rescan("ghost").await.unwrap();
        assert!(reg.get("ghost").await.is_none());
    }
}
