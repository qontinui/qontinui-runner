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

/// Resolve the helper script path: env override, then the dev path under
/// `CARGO_MANIFEST_DIR/resources/code-semantics/ts-language-service.mjs`.
pub fn resolve_helper_script() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("QONTINUI_CODE_SEM_HELPER") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    let dev = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("resources")
        .join("code-semantics")
        .join("ts-language-service.mjs");
    if dev.exists() {
        return Some(dev);
    }
    None
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
        // Run with cwd at the frontend root (two levels above src-tauri) so the
        // helper's frontend-node_modules resolution works in dev.
        if let Some(frontend_root) = self.script.ancestors().nth(3) {
            cmd.current_dir(frontend_root);
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
