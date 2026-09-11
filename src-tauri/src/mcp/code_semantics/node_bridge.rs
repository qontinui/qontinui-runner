//! Node TypeScript Language-Service helper supervisor.
//!
//! Spawns and supervises the long-lived `ts-language-service.mjs` child
//! (CONTRACT.md §A), speaks the newline-delimited JSON stdio protocol with
//! request-id correlation, and restarts the child on crash with bounded
//! backoff. Modeled after the child-process pattern in
//! `src-tauri/src/agent_runtime.rs` (spawn + pump stdout BufReader loop).
//!
//! One bridge instance per (repo, language) scope; v1 default scope is the
//! runner's own TS frontend project. The bridge owns the child; the scope
//! registry (`mod.rs`) maps a scope key to a bridge.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};
use tracing::{debug, error, info, warn};

/// Outcome of attempting to find a usable `node` + helper script.
#[derive(Debug, Clone)]
pub enum HelperAvailability {
    /// `node` and the helper script are both present.
    Available { node: String, script: PathBuf },
    /// Permanently unavailable (no node, or no helper script) — the sidecar
    /// degrades to the code_graph fallback / `engine_unavailable`.
    Unavailable(String),
}

/// The helper script's location relative to the crate root — the same relative
/// path in the shipped bundle (`bundle.resources` preserves it) and in a
/// checkout, so one constant serves both rungs.
///
/// `pub(crate)` because [`crate::bundled_resources::runtime_assets`] probes it
/// for the capability manifest's `bundled_resources` row. The manifest asks the
/// OWNER for the relative path rather than re-spelling the literal, so a rename
/// here cannot leave the manifest reporting a rung for a file nothing reads.
pub(crate) fn helper_script_relpath() -> PathBuf {
    Path::new("resources")
        .join("code-semantics")
        .join("ts-language-service.mjs")
}

/// Resolve the helper script path.
///
/// Three rungs, each existence-checked, first hit wins:
///
/// 1. `$QONTINUI_CODE_SEM_HELPER` — the operator's explicit override;
/// 2. the **bundled** copy, resolved through Tauri's `BaseDirectory::Resource`;
/// 3. the dev-checkout copy under the resolved workspace root
///    (`<root>/qontinui-runner/src-tauri/resources/...`), which is what keeps a
///    `cargo run` / `cargo test` session working with no bundle present.
///
/// A total miss returns `None` and the sidecar degrades to the code_graph
/// fallback / `engine_unavailable` — unchanged. Fail-soft is deliberate: the
/// Node language service is an accelerator, not a correctness dependency.
///
/// Rungs 2 and 3 replace a single `env!("CARGO_MANIFEST_DIR")/resources/...`
/// join (plan `2026-08-04-remove-hardcoded-machine-paths-from-product-code`,
/// slice 5 Phase 6). `CARGO_MANIFEST_DIR` is a **compile-time** constant: it
/// named the source tree the binary was BUILT from, so it baked the build
/// host's absolute layout into a shipped open-source binary and pointed at a
/// directory that exists on no other machine. Note the swap only works because
/// the same phase added `resources/code-semantics/**/*` to `bundle.resources` in
/// `tauri.conf.json` — before that the bundle shipped no `resources/` at all,
/// so rung 2 would have named a path present on **no** host.
pub fn resolve_helper_script() -> Option<PathBuf> {
    let relative = helper_script_relpath();
    let env_override = std::env::var("QONTINUI_CODE_SEM_HELPER")
        .ok()
        .filter(|p| !p.trim().is_empty())
        .map(PathBuf::from);
    let bundled = crate::bundled_resources::resolve(&relative);
    let dev = crate::bundled_resources::dev_checkout(&relative);
    crate::bundled_resources::first_existing([
        env_override.as_deref(),
        bundled.as_deref(),
        dev.as_deref(),
    ])
}

/// Locate a `node` binary. Honors `QONTINUI_NODE_PATH`, else `node` on PATH.
fn resolve_node() -> Option<String> {
    if let Ok(p) = std::env::var("QONTINUI_NODE_PATH") {
        if !p.is_empty() {
            return Some(p);
        }
    }
    Some("node".to_string())
}

/// Determine whether the Node engine can be used at all (permanent degrade if
/// not). This is a cheap check; an actual spawn failure is also treated as
/// unavailable at request time.
pub fn check_availability() -> HelperAvailability {
    let script = match resolve_helper_script() {
        Some(s) => s,
        None => {
            return HelperAvailability::Unavailable(
                "code-semantics helper script not found".to_string(),
            )
        }
    };
    match resolve_node() {
        Some(node) => HelperAvailability::Available { node, script },
        None => HelperAvailability::Unavailable("node binary not found".to_string()),
    }
}

/// A supervised helper child for one scope.
pub struct NodeBridge {
    node: String,
    script: PathBuf,
    project: String,
    next_id: AtomicU64,
    inner: Arc<Mutex<BridgeInner>>,
}

struct BridgeInner {
    stdin: Option<ChildStdin>,
    child: Option<Child>,
    /// id -> responder
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    /// Restart bookkeeping.
    restart_count: u32,
    /// True once `init` has returned successfully (scope warm).
    initialized: bool,
    /// Cached file count from the last successful init.
    file_count: u64,
}

#[derive(Debug)]
pub enum BridgeError {
    /// The helper process is not reachable (spawn failed / crashed).
    Unavailable(String),
    /// The helper returned `{ok:false,error:...}`.
    Helper(String),
    /// Protocol / serialization error.
    Protocol(String),
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BridgeError::Unavailable(m) => write!(f, "helper unavailable: {m}"),
            BridgeError::Helper(m) => write!(f, "helper error: {m}"),
            BridgeError::Protocol(m) => write!(f, "protocol error: {m}"),
        }
    }
}

impl NodeBridge {
    /// Create (but do not yet spawn) a bridge for a scope rooted at `project`
    /// (an abs dir or a tsconfig.json path).
    pub fn new(node: String, script: PathBuf, project: String) -> Self {
        NodeBridge {
            node,
            script,
            project,
            next_id: AtomicU64::new(1),
            inner: Arc::new(Mutex::new(BridgeInner {
                stdin: None,
                child: None,
                pending: Arc::new(Mutex::new(HashMap::new())),
                restart_count: 0,
                initialized: false,
                file_count: 0,
            })),
        }
    }

    pub fn project(&self) -> &str {
        &self.project
    }

    /// True once the scope has completed `init` (warm).
    pub async fn is_initialized(&self) -> bool {
        self.inner.lock().await.initialized
    }

    pub async fn file_count(&self) -> u64 {
        self.inner.lock().await.file_count
    }

    /// Spawn the child if not already running. Wires the stdout pump that
    /// resolves pending request ids. Does NOT send `init`.
    async fn ensure_spawned(&self) -> Result<(), BridgeError> {
        let mut inner = self.inner.lock().await;
        if inner.stdin.is_some() && inner.child.is_some() {
            // Cheap liveness check.
            if let Some(child) = inner.child.as_mut() {
                if let Ok(Some(_status)) = child.try_wait() {
                    // Child exited; fall through to respawn.
                    inner.stdin = None;
                    inner.child = None;
                    inner.initialized = false;
                } else {
                    return Ok(());
                }
            }
        }

        debug!(
            "code_semantics: spawning helper node={} script={}",
            self.node,
            self.script.display()
        );
        let mut cmd = crate::process_helpers::tokio_no_window(&self.node);
        cmd.arg(&self.script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Run with cwd at the script's THIRD ancestor. From
        // `<X>/resources/code-semantics/ts-language-service.mjs` that is `<X>`,
        // which in a dev checkout is **`src-tauri`** — not the repo root. The
        // index is deliberate, not off by one: Node resolves `node_modules` by
        // walking UP from the cwd, so starting at `src-tauri` finds the repo
        // root's `node_modules/typescript` on the next step. Do not "fix" this
        // to `nth(4)`; that would change which tree the helper resolves from.
        //
        // **Existence-checked** (slice 5 Phase 6). Until this phase the script
        // only ever resolved to the dev-checkout path, where that ancestor
        // always existed. Now it can also resolve into Tauri's unpacked resource
        // directory, whose third ancestor is not guaranteed to be a directory —
        // and `current_dir` on a non-existent path makes the SPAWN fail rather
        // than degrade. Skipping the `current_dir` instead lets the helper start
        // and fall through its own documented resolution order
        // (`$QONTINUI_TS_PATH`, then the global require), which ends in an
        // actionable message rather than "spawn failed".
        if let Some(src_tauri_dir) = self.script.ancestors().nth(3).filter(|p| p.is_dir()) {
            cmd.current_dir(src_tauri_dir);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| BridgeError::Unavailable(format!("spawn failed: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| BridgeError::Unavailable("no stdin handle".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| BridgeError::Unavailable("no stdout handle".to_string()))?;
        let stderr = child.stderr.take();

        let pending = inner.pending.clone();

        // Pump stdout: parse each line as a response, resolve the pending id.
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(trimmed) {
                    Ok(val) => {
                        let id = val.get("id").and_then(|v| v.as_u64());
                        if let Some(id) = id {
                            let tx = { pending.lock().await.remove(&id) };
                            if let Some(tx) = tx {
                                let _ = tx.send(val);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("code_semantics: bad helper line: {e}: {trimmed}");
                    }
                }
            }
            debug!("code_semantics: helper stdout pump ended");
        });

        // Drain stderr to logs.
        if let Some(stderr) = stderr {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    info!("code_semantics[helper]: {line}");
                }
            });
        }

        inner.stdin = Some(stdin);
        inner.child = Some(child);
        Ok(())
    }

    /// Send a request and await the correlated response (bounded timeout).
    async fn request(&self, mut body: Value) -> Result<Value, BridgeError> {
        self.ensure_spawned().await?;
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        if let Value::Object(ref mut map) = body {
            map.insert("id".to_string(), json!(id));
        }

        let (tx, rx) = oneshot::channel();
        let (pending, line) = {
            let inner = self.inner.lock().await;
            let pending = inner.pending.clone();
            (pending, serde_json::to_string(&body).unwrap() + "\n")
        };
        pending.lock().await.insert(id, tx);

        {
            let mut inner = self.inner.lock().await;
            let stdin = inner
                .stdin
                .as_mut()
                .ok_or_else(|| BridgeError::Unavailable("stdin gone".to_string()))?;
            if let Err(e) = stdin.write_all(line.as_bytes()).await {
                pending.lock().await.remove(&id);
                inner.stdin = None;
                inner.child = None;
                inner.initialized = false;
                return Err(BridgeError::Unavailable(format!("write failed: {e}")));
            }
            let _ = stdin.flush().await;
        }

        let resp = match tokio::time::timeout(Duration::from_secs(60), rx).await {
            Ok(Ok(v)) => v,
            Ok(Err(_)) => {
                pending.lock().await.remove(&id);
                return Err(BridgeError::Protocol("responder dropped".to_string()));
            }
            Err(_) => {
                pending.lock().await.remove(&id);
                return Err(BridgeError::Protocol("request timed out".to_string()));
            }
        };

        if resp.get("ok").and_then(|v| v.as_bool()) == Some(true) {
            Ok(resp)
        } else {
            let msg = resp
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown helper error")
                .to_string();
            Err(BridgeError::Helper(msg))
        }
    }

    /// Ensure the scope is initialized (idempotent). Returns Ok once warm.
    /// Restart bookkeeping with bounded backoff applies on spawn failure.
    pub async fn ensure_init(&self) -> Result<(), BridgeError> {
        if self.is_initialized().await {
            return Ok(());
        }
        // Bounded restart backoff guard.
        {
            let inner = self.inner.lock().await;
            if inner.restart_count > 5 {
                return Err(BridgeError::Unavailable(
                    "helper exceeded restart budget".to_string(),
                ));
            }
        }
        let resp = self
            .request(json!({"cmd":"init","project": self.project}))
            .await
            .inspect_err(|_| {
                // Count the failed attempt.
                if let Ok(mut inner) = self.inner.try_lock() {
                    inner.restart_count += 1;
                }
            })?;
        let file_count = resp.get("file_count").and_then(|v| v.as_u64()).unwrap_or(0);
        let mut inner = self.inner.lock().await;
        inner.initialized = true;
        inner.file_count = file_count;
        inner.restart_count = 0;
        info!(
            "code_semantics: scope initialized project={} files={file_count}",
            self.project
        );
        Ok(())
    }

    // ---- Query passthroughs (each returns the §A result body Value) ----

    pub async fn symbol_lookup(
        &self,
        name: &str,
        kind: Option<&str>,
        file: Option<&str>,
    ) -> Result<Value, BridgeError> {
        self.ensure_init().await?;
        let mut body = json!({"cmd":"symbol_lookup","name": name});
        if let Some(k) = kind {
            body["kind"] = json!(k);
        }
        if let Some(f) = file {
            body["file"] = json!(f);
        }
        self.request(body).await
    }

    pub async fn signature(
        &self,
        file: &str,
        name: Option<&str>,
        kind: Option<&str>,
        line: Option<u32>,
        col: Option<u32>,
    ) -> Result<Value, BridgeError> {
        self.ensure_init().await?;
        let mut body = json!({"cmd":"signature","file": file});
        if let Some(n) = name {
            body["name"] = json!(n);
        }
        if let Some(k) = kind {
            body["kind"] = json!(k);
        }
        if let Some(l) = line {
            body["line"] = json!(l);
        }
        if let Some(c) = col {
            body["col"] = json!(c);
        }
        self.request(body).await
    }

    pub async fn find_references(
        &self,
        file: &str,
        name: Option<&str>,
        line: Option<u32>,
        col: Option<u32>,
    ) -> Result<Value, BridgeError> {
        self.ensure_init().await?;
        let mut body = json!({"cmd":"find_references","file": file});
        if let Some(n) = name {
            body["name"] = json!(n);
        }
        if let Some(l) = line {
            body["line"] = json!(l);
        }
        if let Some(c) = col {
            body["col"] = json!(c);
        }
        self.request(body).await
    }

    pub async fn typecheck(
        &self,
        file: &str,
        overlay_patch: Option<Value>,
    ) -> Result<Value, BridgeError> {
        self.ensure_init().await?;
        let mut body = json!({"cmd":"typecheck","file": file});
        if let Some(p) = overlay_patch {
            body["overlay_patch"] = p;
        }
        self.request(body).await
    }

    /// Kill the child on shutdown.
    pub async fn shutdown(&self) {
        let mut inner = self.inner.lock().await;
        inner.stdin = None;
        if let Some(mut child) = inner.child.take() {
            let _ = child.start_kill();
        }
        inner.initialized = false;
    }
}

#[cfg(test)]
mod helper_script_tests {
    use super::*;
    // The existence rule itself is owned and tested by `crate::bundled_resources`;
    // what these tests pin is THIS site's rung ORDER (override → bundle → dev).
    use crate::bundled_resources::first_existing;
    use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

    /// A throwaway tree to hang candidate paths off. pid/counter-scoped because
    /// several worktrees on this box run `cargo test` at once, and torn down by
    /// a `Drop` guard so a failing assertion does not leak it. Mirrors the
    /// `Fixture` in `crate::workspace_paths`.
    struct Fixture {
        root: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    impl Fixture {
        /// Create `<root>/<name>` as a file and return its path.
        fn file(&self, name: &str) -> PathBuf {
            let p = self.root.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"// fixture").unwrap();
            p
        }

        /// A path under the fixture that is deliberately never created.
        fn absent(&self, name: &str) -> PathBuf {
            self.root.join(name)
        }
    }

    fn fixture() -> Fixture {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "qontinui_code_sem_helper_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, AtomicOrdering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    /// The operator's `$QONTINUI_CODE_SEM_HELPER` override outranks both the
    /// bundled resource and the dev checkout. Unchanged from before Phase 6 —
    /// asserted so the rung order cannot silently drift.
    #[test]
    fn the_env_override_wins_over_the_bundle_and_the_dev_checkout() {
        let f = fixture();
        let over = f.file("override.mjs");
        let bundled = f.file("bundled.mjs");
        let dev = f.file("dev.mjs");

        let got = first_existing([Some(&over), Some(&bundled), Some(&dev)]);

        assert_eq!(got.as_deref(), Some(over.as_path()));
    }

    /// An override pointing at a path that does not exist is skipped rather
    /// than short-circuiting to `None` — a stale env var must not disable a
    /// perfectly good bundled helper.
    #[test]
    fn a_nonexistent_override_falls_through_to_the_bundle() {
        let f = fixture();
        let over = f.absent("no-such-override.mjs");
        let bundled = f.file("bundled.mjs");

        let got = first_existing([Some(&over), Some(&bundled), None]);

        assert_eq!(got.as_deref(), Some(bundled.as_path()));
    }

    /// The dev rung is what a `cargo run` / `cargo test` session hits: no
    /// `AppHandle` is set, so the bundled candidate is `None`, and resolution
    /// must continue past it instead of stopping.
    #[test]
    fn an_absent_app_handle_falls_through_to_the_dev_checkout() {
        let f = fixture();
        let dev = f.file("src-tauri/resources/code-semantics/ts-language-service.mjs");

        let got = first_existing([None, None, Some(&dev)]);

        assert_eq!(got.as_deref(), Some(dev.as_path()));
    }

    /// Nothing resolving is `None`, never a fabricated path — the sidecar then
    /// degrades to the code_graph fallback / `engine_unavailable`.
    #[test]
    fn nothing_resolving_yields_none_rather_than_a_fabricated_path() {
        let f = fixture();
        let missing = f.absent("gone.mjs");

        assert_eq!(first_existing([Some(&missing), None, None]), None);

        let all_absent: [Option<&Path>; 3] = [None, None, None];
        assert_eq!(first_existing(all_absent), None);
    }

    /// The bundle rung and the dev rung must agree on the relative path, or one
    /// of them silently resolves to nothing. `bundle.resources` preserves a
    /// resource's path relative to the crate root, so this literal is also what
    /// `resources/code-semantics/**/*` produces inside the installer.
    #[test]
    fn the_relative_path_matches_the_bundled_layout() {
        assert_eq!(
            helper_script_relpath(),
            Path::new("resources")
                .join("code-semantics")
                .join("ts-language-service.mjs")
        );
    }
}
