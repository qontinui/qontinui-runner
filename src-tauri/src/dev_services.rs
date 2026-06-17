//! Dev-mode service defaults.
//!
//! When the runner starts in development mode, this module detects the
//! qontinui workspace layout and provides default ProcessConfig entries for
//! services that the runner should manage (Docker, Backend, Frontend).
//!
//! These defaults are only injected when:
//! - The runner is in dev-mode (debug build or QONTINUI_ENV=development)
//! - The user has not already configured managed processes for these ports
//! - The workspace is a qontinui development workspace

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tracing::info;

use crate::error_monitor::types::ParserType;
use crate::process_capture::ProcessConfig;

/// Well-known service identifiers for dev services.
/// Using deterministic IDs so we can detect and update them across restarts.
const DOCKER_SERVICE_ID_SUFFIX: &str = "dev-docker";
const BACKEND_SERVICE_ID_SUFFIX: &str = "dev-backend";
const FRONTEND_SERVICE_ID_SUFFIX: &str = "dev-frontend";
const EMBEDDING_SERVICE_ID_SUFFIX: &str = "dev-embedding";

/// Detect the qontinui workspace root by looking for expected directories.
///
/// Searches from the runner executable upward, then from CWD upward,
/// then checks QONTINUI_PARENT_DIR env var.
pub fn find_workspace_root() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    // Search from executable location upward
    if let Ok(exe_path) = std::env::current_exe() {
        let mut dir = exe_path.parent().map(|p| p.to_path_buf());
        while let Some(d) = dir {
            candidates.push(d.clone());
            dir = d.parent().map(|p| p.to_path_buf());
        }
    }

    // Search from CWD upward
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir = Some(cwd);
        while let Some(d) = dir {
            candidates.push(d.clone());
            dir = d.parent().map(|p| p.to_path_buf());
        }
    }

    // Check QONTINUI_PARENT_DIR env var
    if let Ok(parent_dir) = std::env::var("QONTINUI_PARENT_DIR") {
        candidates.push(PathBuf::from(parent_dir));
    }

    // A valid workspace has qontinui-web with backend and frontend
    for candidate in candidates {
        if is_qontinui_workspace(&candidate) {
            info!("Found qontinui workspace root: {}", candidate.display());
            return Some(candidate);
        }
    }

    None
}

/// Check if a directory is a qontinui workspace root.
fn is_qontinui_workspace(dir: &Path) -> bool {
    dir.join("qontinui-web").join("backend").exists()
        && dir.join("qontinui-web").join("frontend").exists()
        && dir.join("qontinui-runner").exists()
}

/// Generate a deterministic dev service ID based on workspace path and service name.
///
/// Uses UUID v5 (SHA-1 name-based) which is stable across Rust versions,
/// unlike `DefaultHasher` whose output can change between releases.
fn dev_service_id(workspace: &Path, suffix: &str) -> String {
    use uuid::Uuid;
    // Use the DNS namespace as a base; the actual uniqueness comes from the name string.
    let name = format!("{}\0{}", workspace.to_string_lossy(), suffix);
    Uuid::new_v5(&Uuid::NAMESPACE_DNS, name.as_bytes()).to_string()
}

/// Generate default dev-mode ProcessConfigs for the qontinui stack.
///
/// Returns configs for:
/// - Docker services (start_group 0, port 5432)
/// - Backend / FastAPI (start_group 1, port 8000)
/// - Frontend / Next.js (start_group 1, port 3001)
///
/// All configs have `auto_start: true` and `dev_only: true`.
pub fn get_default_dev_services(workspace: &Path) -> Vec<ProcessConfig> {
    let web_dir = workspace.join("qontinui-web");
    let backend_dir = web_dir.join("backend");
    let frontend_dir = web_dir.join("frontend");

    let mut services = Vec::new();

    // Docker services (PostgreSQL, Redis, MinIO)
    if web_dir.join("docker-compose.yml").exists() {
        services.push(ProcessConfig {
            id: dev_service_id(workspace, DOCKER_SERVICE_ID_SUFFIX),
            name: "Docker Services (PostgreSQL, Redis, MinIO)".to_string(),
            command: "docker".to_string(),
            args: vec!["compose".to_string(), "up".to_string(), "-d".to_string()],
            cwd: web_dir.to_string_lossy().to_string(),
            env: HashMap::new(),
            health_port: Some(5432),
            parser: ParserType::Generic,
            auto_start: true,
            category: "infrastructure".to_string(),
            buffer_size: 2000,
            enabled: true,
            ignore_patterns: vec![],
            start_group: 0,
            dev_only: true,
            rebuild_enabled: false,
            build_command: None,
            build_args: vec![],
        });
    }

    // Backend (FastAPI)
    if backend_dir.join("pyproject.toml").exists() {
        services.push(ProcessConfig {
            id: dev_service_id(workspace, BACKEND_SERVICE_ID_SUFFIX),
            name: "Backend (FastAPI)".to_string(),
            command: "poetry".to_string(),
            args: vec![
                "run".to_string(),
                "python".to_string(),
                "run.py".to_string(),
            ],
            cwd: backend_dir.to_string_lossy().to_string(),
            env: HashMap::new(),
            health_port: Some(8000),
            parser: ParserType::Python,
            auto_start: true,
            category: "backend".to_string(),
            buffer_size: 2000,
            enabled: true,
            ignore_patterns: vec![],
            start_group: 1,
            dev_only: true,
            rebuild_enabled: true,
            build_command: Some("poetry".to_string()),
            build_args: vec!["install".to_string()],
        });
    }

    // Frontend (Next.js)
    if frontend_dir.join("package.json").exists() {
        let mut env = HashMap::new();
        // Set TEMP/TMP to workspace .temp to avoid C drive issues on Windows
        let temp_dir = workspace.join(".temp");
        env.insert("TEMP".to_string(), temp_dir.to_string_lossy().to_string());
        env.insert("TMP".to_string(), temp_dir.to_string_lossy().to_string());

        // Use `npx next dev` rather than `npm run dev`: npx resolves the
        // project-local Next.js binary from `node_modules/.bin` directly,
        // independent of whether the spawning shell has populated PATH with
        // the project's `node_modules/.bin`. Tauri's process_capture spawn
        // path doesn't run the `npm run` PATH-injection lifecycle reliably
        // on all hosts, so `npm run dev` was failing with `next: ENOENT`
        // when the user POST'd `/processes/frontend/restart`.
        //
        // `--port 3001` pins the listener to the port the runner's UI Bridge
        // proxy + supervisor health watchers expect (see `health_port` below
        // and the hard-coded `localhost:3001` references in
        // `executor::url_lock` and `commands::web_integration`). Without it,
        // `next dev` defaults to 3000 and every health check + proxy hop
        // misses.
        //
        // `--hostname 0.0.0.0` lets the runner (and, in container/WSL setups,
        // other hosts on the bridge) reach the dev server; the Next.js
        // default of `localhost` rejects non-loopback peers.
        services.push(ProcessConfig {
            id: dev_service_id(workspace, FRONTEND_SERVICE_ID_SUFFIX),
            name: "Frontend (Next.js)".to_string(),
            command: "npx".to_string(),
            args: vec![
                "next".to_string(),
                "dev".to_string(),
                "--port".to_string(),
                "3001".to_string(),
                "--hostname".to_string(),
                "0.0.0.0".to_string(),
            ],
            cwd: frontend_dir.to_string_lossy().to_string(),
            env,
            health_port: Some(3001),
            parser: ParserType::JavaScript,
            auto_start: true,
            category: "frontend".to_string(),
            buffer_size: 2000,
            enabled: true,
            ignore_patterns: vec![],
            start_group: 1,
            dev_only: true,
            rebuild_enabled: true,
            build_command: Some("npx".to_string()),
            build_args: vec!["next".to_string(), "build".to_string()],
        });
    }

    // Embedding service (MiniLM-L6-v2 on port 8001)
    // Standalone FastAPI server used by the runner's EmbeddingClient for
    // hybrid search and knowledge graph embeddings.
    let runner_dir = workspace.join("qontinui-runner");
    let embedding_script = runner_dir.join("python-bridge").join("embedding_server.py");
    if embedding_script.exists() {
        services.push(ProcessConfig {
            id: dev_service_id(workspace, EMBEDDING_SERVICE_ID_SUFFIX),
            name: "Embedding Service (MiniLM-L6-v2)".to_string(),
            // Launch under Poetry (like the backend service above), NOT bare
            // `python`. uvicorn/fastapi/pydantic/sentence-transformers and the
            // qontinui package live in the python-bridge Poetry env, not in
            // the system interpreter that bare `python` resolves to — so
            // `python embedding_server.py` dies at import with
            // `ModuleNotFoundError: No module named 'uvicorn'`, never binds
            // 8001, and the runner reports derived_status=degraded. `poetry
            // run` (cwd = python-bridge, which owns the pyproject/poetry.lock)
            // resolves the right interpreter.
            command: "poetry".to_string(),
            args: vec![
                "run".to_string(),
                "python".to_string(),
                embedding_script.to_string_lossy().to_string(),
            ],
            cwd: runner_dir
                .join("python-bridge")
                .to_string_lossy()
                .to_string(),
            env: HashMap::new(),
            health_port: Some(8001),
            parser: ParserType::Python,
            auto_start: true,
            category: "infrastructure".to_string(),
            buffer_size: 2000,
            enabled: true,
            ignore_patterns: vec![],
            // start_group 2: after backend (group 1) so the qontinui package
            // and its sentence-transformers dependency are available.
            start_group: 2,
            dev_only: false, // embeddings are needed in production too
            // Provision the python-bridge Poetry env on rebuild, mirroring the
            // backend service's `poetry install`.
            rebuild_enabled: true,
            build_command: Some("poetry".to_string()),
            build_args: vec!["install".to_string()],
        });
    }

    services
}

/// Check if any existing managed processes already cover a given health port
/// with auto_start enabled (i.e., they will actually start).
fn port_has_auto_start(existing: &[ProcessConfig], port: u16) -> bool {
    existing
        .iter()
        .any(|p| p.health_port == Some(port) && p.enabled && p.auto_start)
}

/// Get dev services that are not already covered by user-configured processes.
///
/// This prevents duplicating services the user has already manually configured.
pub fn get_missing_dev_services(
    workspace: &Path,
    existing: &[ProcessConfig],
) -> Vec<ProcessConfig> {
    get_default_dev_services(workspace)
        .into_iter()
        .filter(|svc| {
            if let Some(port) = svc.health_port {
                // Only skip if an existing config for this port already has auto_start
                !port_has_auto_start(existing, port)
            } else {
                // No health port — check by dev service ID
                !existing.iter().any(|p| p.id == svc.id)
            }
        })
        .collect()
}

/// Sync user configs to dev-service defaults and dedupe duplicate configs on
/// dev-service ports.
///
/// Two responsibilities, run in order:
///
/// 1. **Sync:** for every enabled config whose `health_port` matches a
///    dev-service default, overwrite the launch-critical fields
///    (`command/args/cwd/env/start_group/dev_only/auto_start/build_command/
///    build_args`) with the current default. This keeps stale user entries —
///    old commands like `python -m uvicorn main:app` pointing at files that
///    no longer exist — aligned with whatever `get_default_dev_services` says
///    today. Per project stance (no external users yet, dev mode), silent
///    sync is preferred to leaving broken configs in place. User-only fields
///    (`id`, `name`, `buffer_size`, `ignore_patterns`, `rebuild_enabled`)
///    are preserved.
///
/// 2. **Dedupe:** for each dev-service port, collapse to a single config.
///    Prefer the canonical dev-service entry (deterministic UUID v5) when
///    present; otherwise keep the first match. All other enabled configs on
///    that port are dropped — two configs racing for the same port end up
///    with one failing `EADDRINUSE`, which is the exact symptom that made us
///    write this pass.
///
/// Returns `true` if anything was mutated so the caller can persist.
pub fn upgrade_and_dedupe_configs(configs: &mut Vec<ProcessConfig>, workspace: &Path) -> bool {
    let defaults = get_default_dev_services(workspace);
    let mut changed = false;

    for config in configs.iter_mut() {
        if !config.enabled {
            continue;
        }
        let Some(port) = config.health_port else {
            continue;
        };
        let Some(dev_default) = defaults.iter().find(|d| d.health_port == Some(port)) else {
            continue;
        };

        let differs = config.command != dev_default.command
            || config.args != dev_default.args
            || config.cwd != dev_default.cwd
            || config.env != dev_default.env
            || config.start_group != dev_default.start_group
            || config.dev_only != dev_default.dev_only
            || config.auto_start != dev_default.auto_start
            || config.build_command != dev_default.build_command
            || config.build_args != dev_default.build_args;

        if differs {
            info!(
                "Dev-mode: syncing config '{}' (port {}) to dev-service defaults -> command='{}', start_group={}, dev_only={}, auto_start={}",
                config.name,
                port,
                dev_default.command,
                dev_default.start_group,
                dev_default.dev_only,
                dev_default.auto_start
            );
            config.command = dev_default.command.clone();
            config.args = dev_default.args.clone();
            config.cwd = dev_default.cwd.clone();
            config.env = dev_default.env.clone();
            config.start_group = dev_default.start_group;
            config.dev_only = dev_default.dev_only;
            config.auto_start = dev_default.auto_start;
            config.build_command = dev_default.build_command.clone();
            config.build_args = dev_default.build_args.clone();
            changed = true;
        }
    }

    for default in &defaults {
        let Some(port) = default.health_port else {
            continue;
        };
        let matches: Vec<usize> = configs
            .iter()
            .enumerate()
            .filter(|(_, c)| c.health_port == Some(port) && c.enabled)
            .map(|(i, _)| i)
            .collect();
        if matches.len() <= 1 {
            continue;
        }
        let keep_idx = matches
            .iter()
            .copied()
            .find(|&i| configs[i].id == default.id)
            .unwrap_or(matches[0]);
        let removed_names: Vec<String> = matches
            .iter()
            .filter(|&&i| i != keep_idx)
            .map(|&i| configs[i].name.clone())
            .collect();
        info!(
            "Dev-mode: deduping port {} -> keeping '{}', removing {:?}",
            port, configs[keep_idx].name, removed_names
        );
        let mut to_remove: Vec<usize> = matches.into_iter().filter(|&i| i != keep_idx).collect();
        to_remove.sort_by(|a, b| b.cmp(a));
        for i in to_remove {
            configs.remove(i);
        }
        changed = true;
    }

    changed
}

/// Check if the current environment is development mode.
pub fn is_dev_mode() -> bool {
    crate::runtime_env::detect_environment() == "development"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Build a minimal qontinui workspace skeleton so the dev-service
    /// existence checks (`docker-compose.yml`, `pyproject.toml`,
    /// `frontend/package.json`) all match and the corresponding
    /// ProcessConfigs get emitted.
    fn make_workspace() -> TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let web = root.join("qontinui-web");
        let backend = web.join("backend");
        let frontend = web.join("frontend");
        fs::create_dir_all(&backend).unwrap();
        fs::create_dir_all(&frontend).unwrap();
        let python_bridge = root.join("qontinui-runner").join("python-bridge");
        fs::create_dir_all(&python_bridge).unwrap();
        fs::write(web.join("docker-compose.yml"), "services: {}\n").unwrap();
        fs::write(backend.join("pyproject.toml"), "[project]\n").unwrap();
        fs::write(frontend.join("package.json"), "{}\n").unwrap();
        // Embedding service is only emitted when this script exists.
        fs::write(python_bridge.join("embedding_server.py"), "# stub\n").unwrap();
        tmp
    }

    /// The embedding service MUST launch under Poetry, not bare `python`. Its
    /// deps (uvicorn/fastapi/sentence-transformers + the qontinui package) live
    /// in the python-bridge Poetry env; bare `python` resolves to the system
    /// interpreter, which lacks them — the service then dies at import and never
    /// binds 8001 (runner goes derived_status=degraded). Mirrors the backend.
    #[test]
    fn embedding_service_uses_poetry() {
        let tmp = make_workspace();
        let services = get_default_dev_services(tmp.path());

        let embedding = services
            .iter()
            .find(|s| s.health_port == Some(8001))
            .expect("embedding dev service should be emitted when embedding_server.py exists");

        assert_eq!(
            embedding.command, "poetry",
            "embedding service must launch via poetry, not bare python; got {:?}",
            embedding.command
        );
        // `poetry run python <script>` — run/python must precede the script path.
        assert_eq!(embedding.args.first().map(String::as_str), Some("run"));
        assert_eq!(embedding.args.get(1).map(String::as_str), Some("python"));
        assert!(
            embedding
                .args
                .last()
                .is_some_and(|a| a.ends_with("embedding_server.py")),
            "last arg must be the embedding_server.py path; got {:?}",
            embedding.args
        );
    }

    /// The frontend dev server MUST be told to listen on 3001 — Next's
    /// default is 3000, but the runner's UI Bridge proxy, supervisor
    /// watchers, and `health_port` on this very ProcessConfig all assume
    /// 3001. Bind hostname to 0.0.0.0 so the runner host (and any peer
    /// across a WSL/container bridge) can reach it.
    #[test]
    fn frontend_port_set() {
        let tmp = make_workspace();
        let services = get_default_dev_services(tmp.path());

        let frontend = services
            .iter()
            .find(|s| s.health_port == Some(3001))
            .expect(
                "frontend dev service should be emitted for a workspace with frontend/package.json",
            );

        assert_eq!(frontend.command, "npx");
        assert!(
            frontend.args.iter().any(|a| a == "--port")
                && frontend.args.iter().any(|a| a == "3001"),
            "frontend args must include `--port 3001`; got {:?}",
            frontend.args
        );
        assert!(
            frontend.args.iter().any(|a| a == "--hostname")
                && frontend.args.iter().any(|a| a == "0.0.0.0"),
            "frontend args must include `--hostname 0.0.0.0`; got {:?}",
            frontend.args
        );
        // Sanity: the port literal must immediately follow `--port` so Next
        // parses it as the flag's value, not as a positional directory.
        let port_idx = frontend
            .args
            .iter()
            .position(|a| a == "--port")
            .expect("--port present");
        assert_eq!(
            frontend.args.get(port_idx + 1).map(String::as_str),
            Some("3001")
        );
        let host_idx = frontend
            .args
            .iter()
            .position(|a| a == "--hostname")
            .expect("--hostname present");
        assert_eq!(
            frontend.args.get(host_idx + 1).map(String::as_str),
            Some("0.0.0.0")
        );
    }
}
