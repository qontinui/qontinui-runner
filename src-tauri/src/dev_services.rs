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

        services.push(ProcessConfig {
            id: dev_service_id(workspace, FRONTEND_SERVICE_ID_SUFFIX),
            name: "Frontend (Next.js)".to_string(),
            command: "npm".to_string(),
            args: vec!["run".to_string(), "dev".to_string()],
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
            build_command: Some("npm".to_string()),
            build_args: vec!["run".to_string(), "build".to_string()],
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
            command: "python".to_string(),
            args: vec![embedding_script.to_string_lossy().to_string()],
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
            rebuild_enabled: false,
            build_command: None,
            build_args: vec![],
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

/// Upgrade legacy configs and remove duplicate configs on dev-service ports.
///
/// Two responsibilities, run in order:
///
/// 1. **Upgrade:** if a user-added config matches a dev-service port but has
///    `auto_start: false`, bump `auto_start`, `start_group`, `dev_only`, and
///    replace `command/args/cwd/env` with the dev-service defaults. Legacy
///    configs often carry wrong commands (e.g. bare `python` instead of
///    `poetry run python run.py`).
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
        if !config.auto_start && config.enabled {
            if let Some(port) = config.health_port {
                if let Some(dev_default) = defaults.iter().find(|d| d.health_port == Some(port)) {
                    info!(
                        "Dev-mode: upgrading legacy config '{}' (port {}) -> command='{}', start_group={}, dev_only=true",
                        config.name, port, dev_default.command, dev_default.start_group
                    );
                    config.auto_start = true;
                    config.start_group = dev_default.start_group;
                    config.dev_only = true;
                    config.command = dev_default.command.clone();
                    config.args = dev_default.args.clone();
                    config.cwd = dev_default.cwd.clone();
                    config.env = dev_default.env.clone();
                    changed = true;
                }
            }
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
