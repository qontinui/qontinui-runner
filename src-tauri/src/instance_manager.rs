//! Instance Manager for spawning and tracking secondary runner processes.
//!
//! Each instance runs on its own port (via `QONTINUI_PORT` env var) and gets
//! its own Tauri window. It is launched as a SUBJECT under its own instance
//! root (`QONTINUI_INSTANCE_ROOT`, see [`subject_instance_root`]): its settings,
//! pairing, `~/.qontinui`, logs, WebView data and embedded PostgreSQL cluster
//! all live under that root, never in this runner's stores. This runner's own
//! database still records which instances it launched.
//!
//! Active instance IDs are persisted to `active_instances.json` so that
//! instances can be restored after a rebuild / restart.  The file is written
//! on every launch/stop and **deleted** on intentional close so that a normal
//! shutdown does not trigger restoration on the next start.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::database::pg::PgDb;
use crate::settings::{RunnerInstanceConfig, SpawnPlacement};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

/// Status of a runner instance.
#[derive(Debug, Clone, Serialize)]
pub struct InstanceStatus {
    pub id: String,
    pub name: String,
    pub port: u16,
    pub running: bool,
    pub pid: Option<u32>,
    pub api_ready: bool,
    /// `"configured"` for entries in `settings.json`, `"discovered"` for
    /// entries that exist only in the DB registry (e.g. supervisor-spawned
    /// runners that registered themselves but were never saved as a slot).
    pub source: &'static str,
    /// Per-instance spawn-window placement, if configured. Only present
    /// for `"configured"` entries — `"discovered"` rows from the DB
    /// registry don't carry placement info.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spawn_placement: Option<SpawnPlacement>,
}

/// Handle for a spawned runner instance.
struct InstanceHandle {
    config: RunnerInstanceConfig,
    child: std::process::Child,
}

/// What `deregister_instance` cleaned up. The two flags can disagree —
/// in-memory has the entry only if THIS runner accepted the original
/// `register_instance` call, while the DB row may persist independently
/// from a previous session that crashed before deregistering. Callers
/// returning HTTP responses use `.any()` to decide between 200 and 404.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct DeregisterResult {
    pub removed_in_memory: bool,
    pub removed_db: bool,
}

impl DeregisterResult {
    pub fn any(self) -> bool {
        self.removed_in_memory || self.removed_db
    }
}

// =============================================================================
// Subject instance root (plan
// `2026-10-04-a-subject-runner-must-be-fully-isolated-from-the-harness-runner`,
// Phase 4)
// =============================================================================

/// The instance root a runner-launched secondary is given:
/// `<this runner's config dir>/instances/runner-<sanitized id>`, made absolute
/// and lexically normalized.
///
/// For a primary that is under `<config_dir>/com.qontinui.runner/instances/` —
/// the supervisor's layout. The `runner-` prefix keeps it out of the
/// supervisor's namespace (its ids are `test-*` / `named-*`, and its reaper
/// removes `instances/<id>` by id). For a launcher that is itself a subject,
/// its config dir lies inside its own root, so its children's roots nest inside
/// it too. On Windows the platform config dir is the ROAMING profile
/// (`%APPDATA%`), so a subject's cluster and WebView data live there.
///
/// The id comes from the UI, so it is sanitized to `[A-Za-z0-9_-]`, capped at 48
/// characters, and — whenever sanitizing changed it — suffixed with a hash of
/// the raw id, so two ids that sanitize alike (`a.b`, `a b`) never share a root.
/// An id with nothing usable left is refused rather than collapsing onto the
/// shared `instances/` parent.
pub(crate) fn subject_instance_root(
    launcher_config_dir: &std::path::Path,
    instance_id: &str,
) -> Result<std::path::PathBuf, String> {
    let mut safe: String = instance_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect();
    if safe.trim_matches('_').is_empty() {
        return Err(format!(
            "instance id {instance_id:?} yields no usable instance-root directory name"
        ));
    }
    if safe != instance_id {
        // FNV-1a 64: stable across builds and platforms (unlike `DefaultHasher`).
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in instance_id.as_bytes() {
            h ^= u64::from(*byte);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        safe = format!("{safe}-{h:016x}");
    }
    let joined = launcher_config_dir
        .join("instances")
        .join(format!("runner-{safe}"));
    let absolute = std::path::absolute(&joined)
        .map_err(|e| format!("cannot make the instance root {joined:?} absolute: {e}"))?;
    Ok(qontinui_runner_lib::instance_env::lexically_normalized(
        &absolute,
    ))
}

/// Check `root` and `port` against the child's own startup contract
/// (`instance_env::validate_instance_root`) BEFORE spawning, so a launch that
/// the child would refuse with exit 78 fails here, with its reason, instead of
/// reporting a PID that dies a moment later.
pub(crate) fn check_subject_root(root: &std::path::Path, port: u16) -> Result<(), String> {
    use qontinui_runner_lib::instance_env::{
        machine_global_dirs, validate_instance_root, PortCheck, PLATFORM_PATH_CASE,
    };
    let port = port.to_string();
    validate_instance_root(
        root,
        PortCheck::Required(Some(&port)),
        &[],
        &machine_global_dirs(),
        PLATFORM_PATH_CASE,
    )
}

/// A secondary may not bind `0`, the primary's port, or this launcher's own:
/// under an instance root the child refuses the first two at startup (exit
/// 78), and the third would collide with the launcher.
pub(crate) fn check_subject_port(port: u16, launcher_port: u16) -> Result<(), String> {
    let primary = qontinui_runner_lib::runner_breadcrumb::PRIMARY_PORT;
    if port == 0 || port == primary || port == launcher_port {
        return Err(format!(
            "port {port} is not available to a secondary runner instance (0, the primary's \
             {primary} and this runner's own {launcher_port} are refused)"
        ));
    }
    Ok(())
}

/// The pairing files a launcher copies from its own secure-storage dir into its
/// subject's `<root>/secure`, and the box files it copies from its own
/// `~/.qontinui` into `<root>/home` — explicit provisioning INTO the root,
/// which D2 of the plan allows (an implicit read outside it is what it forbids).
/// Every copy is skip-if-present, so a persistent instance keeps its own state.
///
/// Mirrors the supervisor's spawn-test default (the primary's pairing snapshot
/// plus `machine.json` / `profiles.json`): without them a launched instance
/// would come up unpaired and with no device identity. Copying `auth_tokens.enc`
/// DUPLICATES the launcher's token store — its per-tenant device-JWT slots and
/// its OAuth refresh token (the AES key derives from host and user, so the
/// child can decrypt it). That is accepted, as it is for the supervisor's
/// spawn-test: if the identity provider rotates refresh tokens, a refresh by
/// either side can invalidate the other's copy, and the loser re-signs in.
/// Nothing ELSE the child could mint for itself (the `coord-device-jwt` file,
/// loopback keys) and no ledger is copied, and `profiles.json` is PROJECTED
/// ([`project_profiles_json`]) rather than copied.
const SUBJECT_SECURE_FILES: [&str; 2] = ["paired_user.json", "auth_tokens.enc"];
const SUBJECT_HOME_FILES: [&str; 2] = ["machine.json", "profiles.json"];

/// The only keys of a profile a subject is given.
const PROJECTED_PROFILE_KEYS: [&str; 2] = ["coord_url", "api_url"];

/// Project `profiles.json` down to `active` plus each profile's `coord_url` /
/// `api_url`. Everything else is dropped on purpose: a profile can carry secrets
/// (`blob.secret_key`, `auth.token`, a DSN or proxy URL with a password), and
/// its `database_url` / `redis_url` / `blob` name THIS runner's stores, which a
/// subject must not use — with no `database_url` it boots its own private
/// embedded cluster under its root. `Err` when the bytes are not a JSON object;
/// provisioning then skips the file (with a WARN) rather than copy it raw.
pub(crate) fn project_profiles_json(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let doc: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| format!("profiles.json is not JSON: {e}"))?;
    let obj = doc
        .as_object()
        .ok_or_else(|| "profiles.json is not a JSON object".to_string())?;
    let mut out = serde_json::Map::new();
    if let Some(active) = obj.get("active") {
        out.insert("active".to_string(), active.clone());
    }
    let mut profiles = serde_json::Map::new();
    if let Some(src) = obj.get("profiles").and_then(|p| p.as_object()) {
        for (name, profile) in src {
            let mut kept = serde_json::Map::new();
            if let Some(profile) = profile.as_object() {
                for key in PROJECTED_PROFILE_KEYS {
                    if let Some(v) = profile.get(key) {
                        kept.insert(key.to_string(), v.clone());
                    }
                }
            }
            profiles.insert(name.clone(), serde_json::Value::Object(kept));
        }
    }
    out.insert("profiles".to_string(), serde_json::Value::Object(profiles));
    serde_json::to_vec_pretty(&serde_json::Value::Object(out))
        .map_err(|e| format!("could not serialize the projected profiles.json: {e}"))
}

fn copy_absent_files(
    source_dir: Option<&std::path::Path>,
    dest_dir: &std::path::Path,
    files: &[&str],
) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(dest_dir)
        .map_err(|e| format!("could not create {}: {e}", dest_dir.display()))?;
    let Some(source_dir) = source_dir else {
        return Ok(Vec::new());
    };
    let mut copied = Vec::new();
    for file in files {
        let src = source_dir.join(file);
        let dst = dest_dir.join(file);
        if !src.is_file() || dst.exists() {
            continue;
        }
        let bytes =
            std::fs::read(&src).map_err(|e| format!("read {} failed: {e}", src.display()))?;
        let bytes = if *file == "profiles.json" {
            match project_profiles_json(&bytes) {
                Ok(projected) => projected,
                Err(e) => {
                    // Skipped, not fatal: the child then runs with its tier's
                    // default coordinator, exactly as a fresh box would.
                    warn!("not provisioning {}: {e}", src.display());
                    continue;
                }
            }
        } else {
            bytes
        };
        // Temp file + rename, so an interrupted copy never leaves a truncated
        // file that skip-if-present would then keep forever.
        let tmp = dest_dir.join(format!(".{file}.provisioning"));
        std::fs::write(&tmp, bytes)
            .and_then(|()| std::fs::rename(&tmp, &dst))
            .map_err(|e| format!("write {} failed: {e}", dst.display()))?;
        copied.push((*file).to_string());
    }
    Ok(copied)
}

/// Create `root` and provision it from the launcher's own stores. Returns the
/// basenames copied, as `secure/<f>` / `home/<f>`.
pub(crate) fn provision_subject_root(
    root: &std::path::Path,
    launcher_secure_dir: Option<&std::path::Path>,
    launcher_home: Option<&std::path::Path>,
) -> Result<Vec<String>, String> {
    use qontinui_runner_lib::instance_env::{
        INSTANCE_ROOT_HOME_SUBDIR, INSTANCE_ROOT_SECURE_SUBDIR,
    };
    let mut copied = Vec::new();
    for (sub, source, files) in [
        (
            INSTANCE_ROOT_SECURE_SUBDIR,
            launcher_secure_dir,
            &SUBJECT_SECURE_FILES,
        ),
        (
            INSTANCE_ROOT_HOME_SUBDIR,
            launcher_home,
            &SUBJECT_HOME_FILES,
        ),
    ] {
        for f in copy_absent_files(source, &root.join(sub), files)? {
            copied.push(format!("{sub}/{f}"));
        }
    }
    Ok(copied)
}

/// Point a launch command at its subject root: export `QONTINUI_INSTANCE_ROOT`
/// and REMOVE every root-confined variable the child would otherwise inherit
/// from this process. A launcher that is itself a subject carries its own
/// `QONTINUI_CONFIG_DIR`, `QONTINUI_HOME`, … — all outside the child's root, so
/// the child would refuse to start; with them removed it derives each one under
/// its own root.
pub(crate) fn apply_subject_root_env(cmd: &mut std::process::Command, root: &std::path::Path) {
    use qontinui_runner_lib::instance_env::{INSTANCE_ROOT_ENV, ROOT_CONFINED_ENV_KEYS};
    for key in ROOT_CONFINED_ENV_KEYS {
        cmd.env_remove(key);
    }
    for key in HARNESS_STORE_ENV_KEYS {
        cmd.env_remove(key);
    }
    cmd.env(INSTANCE_ROOT_ENV, root);
}

/// Inherited variables that would hand a subject the LAUNCHER's stores or
/// credentials, so the launch command removes them: `profiles::load` falls back
/// to `RUNNER_DATABASE_URL` / `REDIS_URL` when a profile names no database
/// (which a projected `profiles.json` never does), a device JWT is read from
/// `COORD_DEVICE_JWT`, and `QONTINUI_DISABLE_EMBEDDED_PG` /
/// `QONTINUI_ALLOW_NO_DB` would leave the child with no database at all instead
/// of its private cluster; `COORD_ADMIN_SECRET` is the operator's coord admin
/// credential. GitHub tokens are deliberately kept: they are the user's own
/// credentials for the repositories a subject's sessions work on. Mirrors the
/// supervisor's `process::instance_root::HARNESS_STORE_ENV_KEYS` (added by the
/// same plan phase).
pub(crate) const HARNESS_STORE_ENV_KEYS: [&str; 8] = [
    "COORD_ADMIN_SECRET",
    "RUNNER_DATABASE_URL",
    "DATABASE_URL",
    "REDIS_URL",
    "COORD_DEVICE_JWT",
    "QONTINUI_RUNNER_TOKEN",
    "QONTINUI_DISABLE_EMBEDDED_PG",
    "QONTINUI_ALLOW_NO_DB",
];

#[cfg(test)]
mod subject_root_tests {
    use super::*;
    use qontinui_runner_lib::instance_env::{INSTANCE_ROOT_ENV, ROOT_CONFINED_ENV_KEYS};
    use std::path::Path;

    #[test]
    fn root_is_namespaced_under_the_launchers_config_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("com.qontinui.runner");
        assert_eq!(
            subject_instance_root(&cfg, "inst-1").unwrap(),
            cfg.join("instances").join("runner-inst-1")
        );
        // Separators and `..` cannot escape `instances/`, and ids that sanitize
        // alike still get distinct roots.
        let escaped = subject_instance_root(&cfg, "../a/b").unwrap();
        assert_eq!(escaped.parent(), Some(cfg.join("instances").as_path()));
        assert_ne!(
            subject_instance_root(&cfg, "a.b").unwrap(),
            subject_instance_root(&cfg, "a b").unwrap()
        );
        for bad in ["", "..", "/", "  "] {
            assert!(subject_instance_root(&cfg, bad).is_err(), "{bad:?}");
        }
        // A relative or `..`-carrying config dir still yields an absolute,
        // normalized root.
        let rel = subject_instance_root(Path::new("rel/cfg/../cfg"), "inst-1").unwrap();
        assert!(rel.is_absolute(), "{rel:?}");
        assert!(!rel
            .components()
            .any(|c| c == std::path::Component::ParentDir));
    }

    /// The root the launcher computes for a primary passes the child's own
    /// startup contract — the property the launcher exists to provide.
    #[test]
    fn the_produced_root_passes_the_childs_startup_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let root =
            subject_instance_root(&tmp.path().join("com.qontinui.runner"), "inst-1").unwrap();
        check_subject_root(&root, 9877).expect("a fresh root on a free port must validate");
        assert!(check_subject_root(&root, 9876).is_err());
        assert!(check_subject_root(Path::new("relative/root"), 9877).is_err());
    }

    #[test]
    fn profiles_projection_keeps_only_urls_and_active() {
        let raw = br#"{"active":"dev","profiles":{
            "dev":{"coord_url":"ws://c","api_url":"http://a","redis_url":"redis://h",
                   "auth":{"kind":"static","token":"T"},"blob":{"secret_key":"S"}},
            "odd":"not-an-object"}}"#;
        let out: serde_json::Value =
            serde_json::from_slice(&project_profiles_json(raw).unwrap()).unwrap();
        assert_eq!(
            out,
            serde_json::json!({
                "active": "dev",
                "profiles": {"dev": {"coord_url": "ws://c", "api_url": "http://a"}, "odd": {}}
            })
        );
        assert!(project_profiles_json(b"[1]").is_err());
    }

    #[test]
    fn refused_ports() {
        assert!(check_subject_port(9877, 9876).is_ok());
        for (port, own) in [(0u16, 9876u16), (9876, 9876), (9876, 9890), (9890, 9890)] {
            assert!(check_subject_port(port, own).is_err(), "{port}/{own}");
        }
    }

    #[test]
    fn provisioning_copies_only_the_listed_files_and_keeps_existing_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let secure = tmp.path().join("parent-secure");
        let home = tmp.path().join("parent-home");
        std::fs::create_dir_all(&secure).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(secure.join("paired_user.json"), "{}").unwrap();
        std::fs::write(home.join("machine.json"), r#"{"device_id":"d"}"#).unwrap();
        std::fs::write(home.join("coord-device-jwt"), "secret").unwrap();
        std::fs::write(
            home.join("profiles.json"),
            r#"{"active":"c","profiles":{"c":{"coord_url":"wss://coord","database_url":"postgres://u:p@h/d"}}}"#,
        )
        .unwrap();
        let root = tmp.path().join("root");

        let copied = provision_subject_root(&root, Some(&secure), Some(&home)).unwrap();
        assert_eq!(
            copied,
            vec![
                "secure/paired_user.json",
                "home/machine.json",
                "home/profiles.json"
            ]
        );
        assert!(!root.join("home").join("coord-device-jwt").exists());
        let profiles = std::fs::read_to_string(root.join("home").join("profiles.json")).unwrap();
        assert!(profiles.contains("wss://coord"), "{profiles}");
        assert!(
            !profiles.contains("database_url"),
            "the launcher's own database must never reach a subject: {profiles}"
        );

        std::fs::write(root.join("secure").join("paired_user.json"), "mine").unwrap();
        assert!(provision_subject_root(&root, Some(&secure), Some(&home))
            .unwrap()
            .is_empty());
        assert_eq!(
            std::fs::read_to_string(root.join("secure").join("paired_user.json")).unwrap(),
            "mine"
        );
    }

    /// The launch command carries the root and NO inherited confined key, even
    /// when the launcher itself has them set — asserted on a real `Command`.
    #[test]
    fn launch_command_carries_the_root_and_removes_inherited_confined_keys() {
        let mut cmd = std::process::Command::new("true");
        for key in ROOT_CONFINED_ENV_KEYS
            .iter()
            .chain(HARNESS_STORE_ENV_KEYS.iter())
        {
            cmd.env(key, "/launcher/elsewhere");
        }
        let root = Path::new("/c/com.qontinui.runner/instances/runner-inst-1");
        apply_subject_root_env(&mut cmd, root);
        let envs: std::collections::HashMap<String, Option<String>> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert_eq!(
            envs.get(INSTANCE_ROOT_ENV).cloned().flatten().as_deref(),
            Some(root.to_str().unwrap())
        );
        for key in ROOT_CONFINED_ENV_KEYS
            .iter()
            .chain(HARNESS_STORE_ENV_KEYS.iter())
        {
            assert_eq!(
                envs.get(*key),
                Some(&None),
                "{key} must be REMOVED, not inherited from the launcher"
            );
        }
    }
}

/// An externally-registered runner instance (not a child process of this runner).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredInstance {
    pub id: String,
    pub name: String,
    pub port: u16,
    pub pid: Option<u32>,
    pub registered_at: chrono::DateTime<chrono::Utc>,
    pub last_heartbeat: chrono::DateTime<chrono::Utc>,
    pub running_tasks: u32,
}

/// Manages spawned runner instance processes and externally-registered instances.
pub struct InstanceManager {
    /// Child processes spawned by this runner.
    instances: Mutex<HashMap<String, InstanceHandle>>,
    /// Externally-registered instances (not child processes).
    registered: Mutex<HashMap<String, RegisteredInstance>>,
    /// PostgreSQL database for persistent instance registry.
    pg_db: Arc<PgDb>,
}

impl InstanceManager {
    pub fn new(pg_db: Arc<PgDb>) -> Self {
        Self {
            instances: Mutex::new(HashMap::new()),
            registered: Mutex::new(HashMap::new()),
            pg_db,
        }
    }

    /// Register the primary runner in the DB on startup.
    pub async fn register_self_as_primary(&self) {
        let port = crate::mcp::types::get_mcp_api_port();
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "localhost".to_string());
        let pid = std::process::id();
        let id = format!("primary-{}", port);

        eprintln!(
            "[InstanceManager] register_self_as_primary: id={} port={}",
            id, port
        );
        match self
            .pg_db
            .upsert_runner_instance(&id, "primary", port, &hostname, true, Some(pid), "healthy")
            .await
        {
            Ok(()) => {
                eprintln!("[InstanceManager] register_self_as_primary: OK");
                info!(
                    "Registered primary runner in DB (port={}, pid={})",
                    port, pid
                );
            }
            Err(e) => {
                eprintln!("[InstanceManager] register_self_as_primary: ERR: {}", e);
                warn!("Failed to register primary in DB: {}", e);
            }
        }
    }

    /// Register an externally-started runner instance.
    /// Returns the assigned or existing instance ID.
    pub async fn register_instance(&self, name: String, port: u16, pid: Option<u32>) -> String {
        let mut registered = self.registered.lock().await;

        // Check if already registered by port
        if let Some(existing) = registered.values_mut().find(|r| r.port == port) {
            existing.name = name;
            existing.pid = pid;
            existing.last_heartbeat = chrono::Utc::now();
            info!(
                "Updated registration for instance '{}' on port {}",
                existing.name, existing.port
            );
            return existing.id.clone();
        }

        let id = format!(
            "ext-{}-{}",
            port,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                % 100000
        );
        let now = chrono::Utc::now();
        let inst = RegisteredInstance {
            id: id.clone(),
            name: name.clone(),
            port,
            pid,
            registered_at: now,
            last_heartbeat: now,
            running_tasks: 0,
        };
        info!(
            "Registered external instance '{}' (id={}, port={})",
            name, id, port
        );
        registered.insert(id.clone(), inst);
        drop(registered); // release lock before async DB call

        // Write-through to PostgreSQL
        let hostname = "localhost".to_string();
        match self
            .pg_db
            .upsert_runner_instance(&id, &name, port, &hostname, false, pid, "healthy")
            .await
        {
            Ok(()) => info!(
                "DB write-through: registered instance {} on port {}",
                id, port
            ),
            Err(e) => warn!("Failed to persist registered instance to DB: {}", e),
        }

        id
    }

    /// Update heartbeat for a registered instance.
    pub async fn update_heartbeat(&self, id: &str, running_tasks: Option<u32>) -> bool {
        let mut registered = self.registered.lock().await;
        if let Some(inst) = registered.get_mut(id) {
            inst.last_heartbeat = chrono::Utc::now();
            if let Some(count) = running_tasks {
                inst.running_tasks = count;
            }
            drop(registered);

            // Write-through to DB
            let _ = self
                .pg_db
                .update_runner_instance_heartbeat(id, running_tasks, "healthy")
                .await;
            return true;
        }
        drop(registered);
        false
    }

    /// Deregister an externally-registered instance from both the in-memory
    /// `registered` map and the persistent DB registry. Either side may be
    /// empty for a given id — the in-memory map is per-process so a row
    /// from a previous session lives only in the DB; conversely a freshly
    /// registered instance whose DB write-through failed lives only in
    /// memory. The returned `DeregisterResult` tells callers which side(s)
    /// were actually cleaned up.
    pub async fn deregister_instance(&self, id: &str) -> DeregisterResult {
        let mut registered = self.registered.lock().await;
        let removed_in_memory = registered.remove(id).is_some();
        drop(registered);

        let removed_db = self.pg_db.remove_runner_instance(id).await.unwrap_or(false);

        DeregisterResult {
            removed_in_memory,
            removed_db,
        }
    }

    /// Get all registered (external) instances.
    pub async fn get_registered_instances(&self) -> Vec<RegisteredInstance> {
        let registered = self.registered.lock().await;
        registered.values().cloned().collect()
    }

    /// Remove registered instances whose ports are not reachable.
    /// Returns the number of instances removed.
    pub async fn purge_unreachable_registered(&self) -> u32 {
        let registered = self.registered.lock().await;
        let to_check: Vec<(String, u16)> = registered
            .values()
            .map(|r| (r.id.clone(), r.port))
            .collect();
        drop(registered);

        let mut removed = 0u32;
        for (id, port) in &to_check {
            if !crate::process_capture::health::is_port_in_use(*port) {
                self.deregister_instance(id).await;
                info!("Purged unreachable registered instance (port {})", port);
                removed += 1;
            }
        }
        removed
    }

    /// Allocate the next free port in the 9877-9899 range.
    ///
    /// Three-way check: in-memory child processes + DB registry + TCP port probe.
    pub async fn allocate_port(&self) -> Result<u16, String> {
        let instances = self.instances.lock().await;
        let child_ports: std::collections::HashSet<u16> =
            instances.values().map(|h| h.config.port).collect();
        drop(instances);

        let registered = self.registered.lock().await;
        let reg_ports: std::collections::HashSet<u16> =
            registered.values().map(|r| r.port).collect();
        drop(registered);

        let db_ports: std::collections::HashSet<u16> = self
            .pg_db
            .get_all_runner_instances()
            .await
            .unwrap_or_default()
            .iter()
            .map(|r| r.port as u16)
            .collect();

        let own_port = crate::mcp::types::get_mcp_api_port();

        (9877..=9899)
            .find(|p| {
                *p != own_port
                    && !child_ports.contains(p)
                    && !reg_ports.contains(p)
                    && !db_ports.contains(p)
                    && !crate::process_capture::health::is_port_in_use(*p)
            })
            .ok_or_else(|| {
                "No available ports in range 9877-9899. Stop some instances first.".to_string()
            })
    }

    /// Get the IDs of all currently running instances.
    pub async fn get_running_ids(&self) -> Vec<String> {
        let mut instances = self.instances.lock().await;
        let mut running = Vec::new();
        let mut dead = Vec::new();
        for (id, handle) in instances.iter_mut() {
            if is_process_alive(&mut handle.child) {
                running.push(id.clone());
            } else {
                dead.push(id.clone());
            }
        }
        // Clean up dead entries
        for id in dead {
            instances.remove(&id);
        }
        running
    }

    /// Launch a new runner instance with the given configuration.
    ///
    /// `resource_override` is forwarded to the spawn-time resource gate — see
    /// [`Self::launch_instance_with_app`].
    pub async fn launch_instance(
        &self,
        config: &RunnerInstanceConfig,
        resource_override: bool,
    ) -> Result<u32, String> {
        self.launch_instance_with_app(config, None, resource_override)
            .await
    }

    /// Launch a new runner instance, optionally resolving spawn
    /// placement against the supplied AppHandle. Callers from a Tauri
    /// context (commands, HTTP handlers) should pass `Some(app)` so
    /// `config.spawn_placement` can be honored. Callers without a
    /// Tauri handle (e.g. session-restore on startup before the
    /// AppHandle is shared with us) can pass `None` and the OS picks
    /// the position.
    ///
    /// `resource_override` carries an explicit "start it anyway" past the
    /// spawn-time resource gate (plan
    /// `2026-08-07-runner-resource-guard-and-session-protection` §Part D). A
    /// secondary runner is a whole second copy of this application — its own
    /// webview, its own embedded services, its own PTYs — so it is exactly the
    /// kind of new process the incident showed at risk, and it gets the same
    /// gate the PTY seam does. See [`crate::resource_guard`].
    pub async fn launch_instance_with_app(
        &self,
        config: &RunnerInstanceConfig,
        app: Option<&tauri::AppHandle>,
        resource_override: bool,
    ) -> Result<u32, String> {
        let mut instances = self.instances.lock().await;

        // Check if already running
        if let Some(handle) = instances.get_mut(&config.id) {
            if is_process_alive(&mut handle.child) {
                return Err(format!("Instance '{}' is already running", config.name));
            }
            // Dead process — remove stale handle
            instances.remove(&config.id);
        }

        // Spawn-time resource gate (§Part D). Placed AFTER the already-running
        // check so a redundant launch of a live instance cannot produce a
        // spurious low-memory notice, and BEFORE anything is created so a
        // refusal leaves no half-launched process and touches nothing that is
        // already running. Off the async worker: the probe refreshes sysinfo and
        // enumerates volumes, which is milliseconds but is still blocking work.
        {
            let app_for_gate = app.cloned();
            spawn_blocking_tracked(move || {
                crate::resource_guard::admit_spawn(
                    "runner instance",
                    resource_override,
                    app_for_gate.as_ref(),
                )
            })
            .await
            .map_err(|e| format!("resource guard task failed: {e}"))??;
        }

        let exe_path = std::env::current_exe()
            .map_err(|e| format!("Failed to get current executable path: {}", e))?;

        info!(
            "Launching runner instance '{}' on port {} (exe: {:?})",
            config.name, config.port, exe_path
        );

        let own_port = crate::mcp::types::get_mcp_api_port();
        check_subject_port(config.port, own_port)
            .map_err(|e| format!("Cannot launch instance '{}': {e}", config.name))?;

        // The instance root (plan
        // `2026-10-04-a-subject-runner-must-be-fully-isolated-from-the-harness-runner`,
        // Phase 4): everything the child owns — settings, pairing, `~/.qontinui`,
        // logs, embedded PG, WebView data — resolves under it, so it can never
        // read, absorb or overwrite this runner's stores. Validated against the
        // child's own startup contract, then provisioned from this runner's own
        // pairing and box files, before the child starts. Off the async worker:
        // it is filesystem work, done while the instances lock is held.
        let (launcher_config_dir, _) = crate::settings::resolve_config_dir()
            .map_err(|e| format!("Cannot launch instance '{}': {e}", config.name))?;
        let root = subject_instance_root(&launcher_config_dir, &config.id)
            .and_then(|root| check_subject_root(&root, config.port).map(|()| root))
            .map_err(|e| format!("Cannot launch instance '{}': {e}", config.name))?;
        let launcher_secure_dir = qontinui_runner_lib::pair::binding_store_candidate_paths()
            .into_iter()
            .next()
            .and_then(|p| p.parent().map(std::path::Path::to_path_buf));
        let launcher_home = qontinui_runner_lib::ambient::qontinui_dir();
        let provisioned = {
            let root = root.clone();
            spawn_blocking_tracked(move || {
                provision_subject_root(
                    &root,
                    launcher_secure_dir.as_deref(),
                    launcher_home.as_deref(),
                )
            })
            .await
            .map_err(|e| format!("instance-root provisioning task failed: {e}"))?
        }
        .map_err(|e| {
            format!(
                "Cannot launch instance '{}': provisioning its instance root failed: {e}",
                config.name
            )
        })?;
        info!(
            "Instance '{}' instance root {:?} (provisioned: {:?})",
            config.name, root, provisioned
        );

        let mut cmd = crate::process_helpers::no_window(&exe_path);

        // Set environment variables
        apply_subject_root_env(&mut cmd, &root);
        cmd.env("QONTINUI_PORT", config.port.to_string());
        cmd.env("QONTINUI_INSTANCE_NAME", &config.name);
        // Propagate the primary runner's port so secondaries can proxy
        // process capture requests back and send heartbeats.
        cmd.env("QONTINUI_PRIMARY_PORT", own_port.to_string());

        // Apply spawn-window placement if configured. This is best-effort:
        // a resolution failure (e.g. monitor disconnected since config) is
        // logged at WARN and the spawn proceeds without the env vars.
        if let Some(placement) = &config.spawn_placement {
            match app {
                Some(app) => {
                    match crate::spawn_placement::resolve_to_global_physical(app, placement) {
                        Ok(resolved) => {
                            info!(
                                "Applying spawn placement to instance '{}': global=({}, {}) size=({}x{}) [{}]",
                                config.name,
                                resolved.global_x,
                                resolved.global_y,
                                resolved.width,
                                resolved.height,
                                resolved.monitor_label
                            );
                            cmd.env("QONTINUI_WINDOW_X", resolved.global_x.to_string());
                            cmd.env("QONTINUI_WINDOW_Y", resolved.global_y.to_string());
                            cmd.env("QONTINUI_WINDOW_WIDTH", resolved.width.to_string());
                            cmd.env("QONTINUI_WINDOW_HEIGHT", resolved.height.to_string());
                            // Decorations is plumbed independently of the
                            // resolve step — it doesn't depend on monitor
                            // geometry and falls back to true (Tauri default)
                            // when the field is None.
                            if let Some(deco) = placement.decorations {
                                cmd.env(
                                    "QONTINUI_WINDOW_DECORATIONS",
                                    if deco { "1" } else { "0" },
                                );
                            }
                        }
                        Err(e) => {
                            warn!(
                                "Spawn placement for '{}' could not be resolved ({}); launching without explicit position",
                                config.name, e
                            );
                        }
                    }
                }
                None => {
                    warn!(
                        "Instance '{}' has spawn_placement configured but launch was invoked without an AppHandle; skipping placement",
                        config.name
                    );
                }
            }
        }

        // Critical: remove CLAUDECODE env var so Claude CLI can start inside the instance
        cmd.env_remove("CLAUDECODE");
        // Same rule, sibling marker — see `session::transport::claude_cli` docs.
        cmd.env_remove(qontinui_runner_lib::claude_env::CLAUDE_CHILD_SESSION_ENV);

        // Create the process in a new process group (Windows)
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            // Combine with existing creation flags (no_window sets CREATE_NO_WINDOW)
            cmd.creation_flags(0x0800_0000 | CREATE_NEW_PROCESS_GROUP);
        }

        let child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn instance '{}': {}", config.name, e))?;

        let pid = child.id();
        info!(
            "Instance '{}' launched with PID {} on port {}",
            config.name, pid, config.port
        );

        // Do NOT assign instances to the Job Object.  The supervisor
        // explicitly stops unprotected secondaries before killing the primary,
        // and protected instances must survive primary restarts/rebuilds.

        instances.insert(
            config.id.clone(),
            InstanceHandle {
                config: config.clone(),
                child,
            },
        );

        // Persist the running set so a rebuild can restore it
        let running: Vec<String> = instances.keys().cloned().collect();
        drop(instances); // release lock before file I/O
        save_active_instances(&running);

        // Write-through to DB
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "localhost".to_string());
        if let Err(e) = self
            .pg_db
            .upsert_runner_instance(
                &config.id,
                &config.name,
                config.port,
                &hostname,
                false,
                Some(pid),
                "starting",
            )
            .await
        {
            warn!("Failed to persist launched instance to DB: {}", e);
        }

        Ok(pid)
    }

    /// Stop a running instance by ID.
    pub async fn stop_instance(&self, id: &str) -> Result<(), String> {
        let mut instances = self.instances.lock().await;
        if let Some(mut handle) = instances.remove(id) {
            info!(
                "Stopping instance '{}' (PID: {})",
                handle.config.name,
                handle.child.id()
            );
            handle
                .child
                .kill()
                .map_err(|e| format!("Failed to kill instance '{}': {}", handle.config.name, e))?;
            let _ = handle.child.wait(); // Reap the process
            info!("Instance '{}' stopped", handle.config.name);

            // Update the persisted running set
            let running: Vec<String> = instances.keys().cloned().collect();
            drop(instances);
            save_active_instances(&running);

            // Remove from DB
            let _ = self.pg_db.remove_runner_instance(id).await;

            Ok(())
        } else {
            Err(format!("Instance '{}' is not running", id))
        }
    }

    /// Get status of a specific instance.
    ///
    /// `running` is true if EITHER (a) we own a live child process for this
    /// id OR (b) the port responds to `/status`. The drop-down consumers
    /// (Orchestration Loop target picker, Settings → Runner Instances) need
    /// to see externally-spawned runners — supervisor children, manually
    /// launched test runners — as live, even though no child handle is held
    /// here. `api_ready` is the probe result on its own, which lets callers
    /// distinguish "process alive but API still warming up" from "API
    /// answering."
    pub async fn get_instance_status(&self, config: &RunnerInstanceConfig) -> InstanceStatus {
        let mut instances = self.instances.lock().await;
        let (child_alive, pid) = if let Some(handle) = instances.get_mut(&config.id) {
            if is_process_alive(&mut handle.child) {
                (true, Some(handle.child.id()))
            } else {
                // Dead — clean up
                instances.remove(&config.id);
                (false, None)
            }
        } else {
            (false, None)
        };
        drop(instances);

        let api_ready = probe_instance_api(config.port).await;

        InstanceStatus {
            id: config.id.clone(),
            name: config.name.clone(),
            port: config.port,
            running: child_alive || api_ready,
            pid,
            api_ready,
            source: "configured",
            spawn_placement: config.spawn_placement.clone(),
        }
    }

    /// Get statuses of all configured instances.
    pub async fn get_all_statuses(&self, configs: &[RunnerInstanceConfig]) -> Vec<InstanceStatus> {
        let mut result = Vec::with_capacity(configs.len());
        for config in configs {
            result.push(self.get_instance_status(config).await);
        }
        result
    }

    /// Get the unified runner instance list: every configured slot from
    /// `settings.json`, merged with every non-primary entry in the DB
    /// `runner_instances` registry that isn't already represented (deduped
    /// by port). Each entry has its `running`/`api_ready` resolved from a
    /// fresh `/status` probe so the drop-down reflects what's actually
    /// alive — not just what's saved as a slot.
    ///
    /// This is the source the Orchestration Loop target drop-down should
    /// consume. The previous `get_all_statuses` path saw only configured
    /// slots, so a supervisor-spawned runner that registered itself in the
    /// DB would never appear in the picker even though it was answering on
    /// its port.
    pub async fn get_unified_instances(
        &self,
        configs: &[RunnerInstanceConfig],
    ) -> Vec<InstanceStatus> {
        let mut by_port: HashMap<u16, InstanceStatus> = HashMap::new();
        for cfg in configs {
            let status = self.get_instance_status(cfg).await;
            by_port.insert(cfg.port, status);
        }

        let own_port = crate::mcp::types::get_mcp_api_port();
        let db_rows = self
            .pg_db
            .get_all_runner_instances()
            .await
            .unwrap_or_default();

        // Probe each not-yet-seen DB row's port in parallel — sequential
        // probes block the whole drop-down load on the slowest endpoint
        // (1s timeout each), and there can easily be 5+ stale rows.
        let to_probe: Vec<_> = db_rows
            .into_iter()
            .filter(|row| {
                let port = row.port as u16;
                !row.is_primary && port != own_port && !by_port.contains_key(&port)
            })
            .collect();

        let probes = to_probe
            .iter()
            .map(|row| probe_instance_api(row.port as u16));
        let results = futures::future::join_all(probes).await;

        for (row, api_ready) in to_probe.into_iter().zip(results) {
            let port = row.port as u16;
            by_port.insert(
                port,
                InstanceStatus {
                    id: row.id,
                    name: row.name,
                    port,
                    running: api_ready,
                    pid: row.pid.map(|p| p as u32),
                    api_ready,
                    source: "discovered",
                    spawn_placement: None,
                },
            );
        }

        let mut result: Vec<_> = by_port.into_values().collect();
        result.sort_by_key(|s| s.port);
        result
    }
}

// ============================================================================
// Active-instance session persistence
// ============================================================================

/// Path to the session file that tracks which instances were running.
fn session_file_path() -> Option<std::path::PathBuf> {
    qontinui_runner_lib::instance_env::runner_config_base().map(|d| d.join("active_instances.json"))
}

/// Persist the set of running instance IDs.
/// Called automatically by `launch_instance` / `stop_instance`.
fn save_active_instances(ids: &[String]) {
    let Some(path) = session_file_path() else {
        return;
    };
    if ids.is_empty() {
        // No instances running — remove the file so a clean start doesn't restore anything
        let _ = std::fs::remove_file(&path);
        return;
    }
    match serde_json::to_string(ids) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                tracing::warn!("Failed to save active instances: {}", e);
            }
        }
        Err(e) => tracing::warn!("Failed to serialize active instances: {}", e),
    }
}

/// Delete the session file.  Called on intentional (user-initiated) close so
/// that the next normal startup does **not** restore instances.
pub fn clear_active_instances() {
    if let Some(path) = session_file_path() {
        let _ = std::fs::remove_file(&path);
    }
}

/// Load the list of instance IDs that were active before the last shutdown.
/// Clears the file after reading so instances aren't re-launched on every restart.
pub fn load_and_clear_active_instances() -> Vec<String> {
    let Some(path) = session_file_path() else {
        return Vec::new();
    };
    if !path.exists() {
        return Vec::new();
    }

    let ids: Vec<String> = match std::fs::read_to_string(&path) {
        Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
        Err(_) => return Vec::new(),
    };

    // Clear the file so we don't re-launch on every start
    let _ = std::fs::remove_file(&path);

    ids
}

/// Wait (synchronously) for a port to become free, with a timeout.
/// Returns `true` if the port is free, `false` if still occupied after timeout.
pub fn wait_for_port_free(port: u16, timeout: std::time::Duration) -> bool {
    let start = std::time::Instant::now();
    let check_interval = std::time::Duration::from_millis(250);
    loop {
        if !crate::process_capture::health::is_port_in_use(port) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(check_interval);
    }
}

/// Check if a child process is still alive (non-blocking).
fn is_process_alive(child: &mut std::process::Child) -> bool {
    match child.try_wait() {
        Ok(Some(_)) => false, // Exited
        Ok(None) => true,     // Still running
        Err(_) => false,      // Error checking — assume dead
    }
}

/// Probe an instance's HTTP API to check if it's ready to accept requests.
/// Returns true if the `/status` endpoint responds within 1 second.
async fn probe_instance_api(port: u16) -> bool {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1))
        .build();

    let client = match client {
        Ok(c) => c,
        Err(_) => return false,
    };

    let url = format!("{}/status", crate::mcp::types::runner_loopback_base(port));
    client.get(&url).send().await.is_ok()
}
