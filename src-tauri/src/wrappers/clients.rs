//! AI client config detection and connect/disconnect helpers.
//!
//! For each supported AI client (claude CLI, Claude Desktop, Cursor,
//! Continue, Cline) this module:
//!
//! 1. Locates the client's MCP server config file.
//! 2. Detects whether the client is installed (config exists OR launcher on PATH).
//! 3. Reads / mutates / atomically writes the JSON, adding or removing the
//!    `mcpServers["qontinui-wrappers"]` entry that points at our stable
//!    launcher path.
//!
//! Stateless — every call re-reads the relevant config files. The only
//! per-process state is the once-per-session backup tracker so two
//! connect calls in a row don't churn out N backups for the same file.
//!
//! See `PLAN-mcp-client-connect-ux.md` D4 (read-modify-write) and D5
//! (detection rules).

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::fs_atomic;
use crate::wrappers::launcher;

const MCP_KEY: &str = "qontinui-wrappers";
const ENV_PORT_KEY: &str = "QONTINUI_PRIMARY_PORT";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum AiClientId {
    ClaudeCli,
    ClaudeDesktop,
    Cursor,
    Continue,
    Cline,
}

impl AiClientId {
    pub const ALL: [AiClientId; 5] = [
        AiClientId::ClaudeCli,
        AiClientId::ClaudeDesktop,
        AiClientId::Cursor,
        AiClientId::Continue,
        AiClientId::Cline,
    ];

    pub fn display_name(&self) -> &'static str {
        match self {
            AiClientId::ClaudeCli => "Claude CLI",
            AiClientId::ClaudeDesktop => "Claude Desktop",
            AiClientId::Cursor => "Cursor",
            AiClientId::Continue => "Continue",
            AiClientId::Cline => "Cline",
        }
    }

    /// Launcher executable name to search for on PATH for "installed" detection.
    /// `None` means we only detect via config file existence.
    fn launcher_name(&self) -> Option<&'static str> {
        match self {
            AiClientId::ClaudeCli => Some("claude"),
            AiClientId::Cursor => Some("cursor"),
            AiClientId::Continue => Some("continue"),
            AiClientId::ClaudeDesktop | AiClientId::Cline => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub id: AiClientId,
    pub display_name: String,
    pub installed: bool,
    pub config_path: Option<PathBuf>,
    pub connection: ConnectionState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "PascalCase")]
pub enum ConnectionState {
    Connected,
    Drifted {
        current_command: PathBuf,
        current_port: Option<u16>,
    },
    NotConnected,
    // Surfaces malformed JSON at LIST time so the UI can disable Connect
    // and point at the offending file before the user clicks anything.
    ConfigParseError {
        path: PathBuf,
    },
}

#[derive(Debug)]
pub enum ClientError {
    ConfigParseFailed { path: PathBuf },
    LauncherMissing,
    IoError { path: PathBuf, msg: String },
    NotInstalled,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::ConfigParseFailed { path } => write!(
                f,
                "Your config at {} has invalid JSON. Open it and fix it, or delete it to start fresh.",
                path.display()
            ),
            ClientError::LauncherMissing => write!(
                f,
                "Launcher not installed. The runner failed to drop wrappers_mcp at the stable path."
            ),
            ClientError::IoError { path, msg } => {
                write!(f, "I/O error for {}: {}", path.display(), msg)
            }
            ClientError::NotInstalled => write!(f, "Client is not installed on this machine."),
        }
    }
}

impl std::error::Error for ClientError {}

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

pub fn detect_all(primary_port: u16) -> Vec<ClientInfo> {
    AiClientId::ALL
        .iter()
        .map(|id| detect_one(*id, primary_port))
        .collect()
}

pub fn connect(id: AiClientId, primary_port: u16) -> Result<(), ClientError> {
    let path = config_path_for(id).ok_or(ClientError::NotInstalled)?;
    let launcher_path = launcher::launcher_path();
    if !launcher_path.is_file() {
        return Err(ClientError::LauncherMissing);
    }
    connect_at_path(&path, &launcher_path, primary_port)
}

pub fn disconnect(id: AiClientId) -> Result<(), ClientError> {
    let path = config_path_for(id).ok_or(ClientError::NotInstalled)?;
    disconnect_at_path(&path)
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

fn detect_one(id: AiClientId, primary_port: u16) -> ClientInfo {
    let config_path = config_path_for(id);
    let launcher_path = launcher::launcher_path();

    let config_exists = config_path.as_ref().map(|p| p.is_file()).unwrap_or(false);
    let on_path = id
        .launcher_name()
        .map(|name| launcher_on_path(name))
        .unwrap_or(false);
    let installed = config_exists || on_path;

    let connection = if let Some(path) = &config_path {
        connection_state_for(path, &launcher_path, primary_port)
    } else {
        ConnectionState::NotConnected
    };

    ClientInfo {
        id,
        display_name: id.display_name().to_string(),
        installed,
        config_path,
        connection,
    }
}

fn connection_state_for(
    config_path: &Path,
    launcher_path: &Path,
    primary_port: u16,
) -> ConnectionState {
    let raw = match fs::read_to_string(config_path) {
        Ok(r) => r,
        Err(_) => return ConnectionState::NotConnected,
    };
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        // Surface parse failure to the UI so Connect can be disabled and
        // the bad path shown without waiting for an action-time 422.
        Err(_) => {
            return ConnectionState::ConfigParseError {
                path: config_path.to_path_buf(),
            }
        }
    };
    // A non-object root (e.g. a bare array) also can't host mcpServers —
    // mirror connect_at_path's stricter check so the UI flags it the same way.
    if !parsed.is_object() {
        return ConnectionState::ConfigParseError {
            path: config_path.to_path_buf(),
        };
    }
    let Some(entry) = parsed.get("mcpServers").and_then(|v| v.get(MCP_KEY)) else {
        return ConnectionState::NotConnected;
    };

    let current_command = entry
        .get("command")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_default();
    let current_port = entry
        .get("env")
        .and_then(|v| v.get(ENV_PORT_KEY))
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u16>().ok());

    let command_matches = paths_equal(&current_command, launcher_path);
    let port_matches = current_port == Some(primary_port);

    if command_matches && port_matches {
        ConnectionState::Connected
    } else {
        ConnectionState::Drifted {
            current_command,
            current_port,
        }
    }
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    let canon_a = fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let canon_b = fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    canon_a == canon_b
}

fn launcher_on_path(name: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    let exe_suffixes: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat", ""]
    } else {
        &[""]
    };
    for dir in std::env::split_paths(&path_var) {
        for suffix in exe_suffixes {
            let candidate = dir.join(format!("{}{}", name, suffix));
            if candidate.is_file() {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Config path resolution
// ---------------------------------------------------------------------------

fn config_path_for(id: AiClientId) -> Option<PathBuf> {
    match id {
        AiClientId::ClaudeCli => dirs::home_dir().map(|h| h.join(".claude.json")),
        AiClientId::ClaudeDesktop => claude_desktop_path(),
        AiClientId::Cursor => dirs::home_dir().map(|h| h.join(".cursor").join("mcp.json")),
        AiClientId::Continue => dirs::home_dir().map(|h| h.join(".continue").join("config.json")),
        AiClientId::Cline => cline_path(),
    }
}

fn claude_desktop_path() -> Option<PathBuf> {
    if cfg!(windows) {
        dirs::config_dir().map(|d| d.join("Claude").join("claude_desktop_config.json"))
    } else if cfg!(target_os = "macos") {
        dirs::home_dir().map(|h| {
            h.join("Library")
                .join("Application Support")
                .join("Claude")
                .join("claude_desktop_config.json")
        })
    } else {
        dirs::config_dir().map(|d| d.join("Claude").join("claude_desktop_config.json"))
    }
}

fn cline_path() -> Option<PathBuf> {
    let cline_dir = ["saoudrizwan.claude-dev", "settings"];
    if cfg!(windows) {
        let base = dirs::config_dir()?;
        Some(
            base.join("Code")
                .join("User")
                .join("globalStorage")
                .join(cline_dir[0])
                .join(cline_dir[1])
                .join("cline_mcp_settings.json"),
        )
    } else if cfg!(target_os = "macos") {
        let home = dirs::home_dir()?;
        Some(
            home.join("Library")
                .join("Application Support")
                .join("Code")
                .join("User")
                .join("globalStorage")
                .join(cline_dir[0])
                .join(cline_dir[1])
                .join("cline_mcp_settings.json"),
        )
    } else {
        // Linux skipped per Open Q5.
        None
    }
}

// ---------------------------------------------------------------------------
// Connect / disconnect (path-injectable for tests)
// ---------------------------------------------------------------------------

fn connect_at_path(
    config_path: &Path,
    launcher_path: &Path,
    primary_port: u16,
) -> Result<(), ClientError> {
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).map_err(|e| ClientError::IoError {
            path: parent.to_path_buf(),
            msg: e.to_string(),
        })?;
    }

    let mut root = read_root(config_path)?;
    let original_bytes = serialize(&root);

    let mcp_servers = ensure_object(&mut root, "mcpServers");
    let entry = build_entry(launcher_path, primary_port);
    mcp_servers.insert(MCP_KEY.to_string(), entry);

    let new_bytes = serialize(&root);
    if new_bytes == original_bytes {
        return Ok(());
    }

    backup_once(config_path)?;
    fs_atomic::atomic_write(config_path, &new_bytes).map_err(|e| ClientError::IoError {
        path: config_path.to_path_buf(),
        msg: e.to_string(),
    })
}

fn disconnect_at_path(config_path: &Path) -> Result<(), ClientError> {
    if !config_path.exists() {
        return Ok(());
    }
    let mut root = read_root(config_path)?;
    let original_bytes = serialize(&root);

    let removed = root
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
        .map(|m| m.remove(MCP_KEY).is_some())
        .unwrap_or(false);
    if !removed {
        return Ok(());
    }

    let new_bytes = serialize(&root);
    if new_bytes == original_bytes {
        return Ok(());
    }

    backup_once(config_path)?;
    fs_atomic::atomic_write(config_path, &new_bytes).map_err(|e| ClientError::IoError {
        path: config_path.to_path_buf(),
        msg: e.to_string(),
    })
}

fn read_root(config_path: &Path) -> Result<Value, ClientError> {
    let raw = match fs::read_to_string(config_path) {
        Ok(r) => r,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Value::Object(Map::new())),
        Err(e) => {
            return Err(ClientError::IoError {
                path: config_path.to_path_buf(),
                msg: e.to_string(),
            })
        }
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    let parsed: Value =
        serde_json::from_str(trimmed).map_err(|_| ClientError::ConfigParseFailed {
            path: config_path.to_path_buf(),
        })?;
    if !parsed.is_object() {
        return Err(ClientError::ConfigParseFailed {
            path: config_path.to_path_buf(),
        });
    }
    Ok(parsed)
}

fn ensure_object<'a>(root: &'a mut Value, key: &str) -> &'a mut Map<String, Value> {
    let map = root.as_object_mut().expect("root must be object");
    if !map.get(key).map(Value::is_object).unwrap_or(false) {
        map.insert(key.to_string(), Value::Object(Map::new()));
    }
    map.get_mut(key).unwrap().as_object_mut().unwrap()
}

fn build_entry(launcher_path: &Path, primary_port: u16) -> Value {
    let mut env = Map::new();
    env.insert(
        ENV_PORT_KEY.to_string(),
        Value::String(primary_port.to_string()),
    );
    let mut entry = Map::new();
    entry.insert(
        "command".to_string(),
        Value::String(launcher_path.to_string_lossy().into_owned()),
    );
    entry.insert("args".to_string(), Value::Array(vec![]));
    entry.insert("env".to_string(), Value::Object(env));
    Value::Object(entry)
}

fn serialize(root: &Value) -> Vec<u8> {
    serde_json::to_vec_pretty(root).expect("serializing JSON Value to Vec<u8> never fails")
}

// ---------------------------------------------------------------------------
// Backup management
// ---------------------------------------------------------------------------

static BACKED_UP: Lazy<Mutex<HashSet<PathBuf>>> = Lazy::new(|| Mutex::new(HashSet::new()));

fn backup_once(config_path: &Path) -> Result<(), ClientError> {
    if !config_path.exists() {
        return Ok(());
    }
    let canonical = fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());
    {
        let mut guard = BACKED_UP.lock().unwrap();
        if guard.contains(&canonical) {
            return Ok(());
        }
        guard.insert(canonical);
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let backup_name = format!(
        "{}.qontinui-backup.{}",
        config_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
        ts
    );
    let backup_path = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(backup_name);
    fs::copy(config_path, &backup_path).map_err(|e| ClientError::IoError {
        path: backup_path.clone(),
        msg: e.to_string(),
    })?;
    prune_old_backups(config_path);
    Ok(())
}

fn prune_old_backups(config_path: &Path) {
    let Some(parent) = config_path.parent() else {
        return;
    };
    let Some(stem) = config_path.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let prefix = format!("{}.qontinui-backup.", stem);
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    let mut backups: Vec<(u64, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let ts_str = name.strip_prefix(&prefix)?;
            let ts: u64 = ts_str.parse().ok()?;
            Some((ts, e.path()))
        })
        .collect();
    if backups.len() <= 5 {
        return;
    }
    backups.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in backups.into_iter().skip(5) {
        let _ = fs::remove_file(path);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex as StdMutex;

    // The BACKED_UP set is process-wide; serialize tests that touch it so
    // tracking from one doesn't suppress backups in another.
    static SERIAL: Lazy<StdMutex<()>> = Lazy::new(|| StdMutex::new(()));

    fn reset_backup_tracker() {
        BACKED_UP.lock().unwrap().clear();
    }

    fn launcher_for_tests(dir: &Path) -> PathBuf {
        let p = dir.join("launcher.exe");
        fs::write(&p, b"#!fake-launcher").unwrap();
        p
    }

    #[test]
    fn merge_preserves_unrelated_keys() {
        let _g = SERIAL.lock().unwrap();
        reset_backup_tracker();
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("cfg.json");
        let initial = json!({
            "mcpServers": {
                "other-server": { "command": "/usr/bin/other", "args": [] }
            },
            "unrelatedTopLevel": 42
        });
        fs::write(&cfg, serde_json::to_string_pretty(&initial).unwrap()).unwrap();

        let launcher = launcher_for_tests(dir.path());
        connect_at_path(&cfg, &launcher, 9876).unwrap();

        let after: Value = serde_json::from_str(&fs::read_to_string(&cfg).unwrap()).unwrap();
        assert_eq!(after.get("unrelatedTopLevel").unwrap(), &json!(42));
        let servers = after.get("mcpServers").unwrap().as_object().unwrap();
        assert!(servers.contains_key("other-server"));
        assert!(servers.contains_key(MCP_KEY));
    }

    #[test]
    fn merge_into_empty_object() {
        let _g = SERIAL.lock().unwrap();
        reset_backup_tracker();
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("cfg.json");
        fs::write(&cfg, "{}").unwrap();

        let launcher = launcher_for_tests(dir.path());
        connect_at_path(&cfg, &launcher, 12345).unwrap();

        let after: Value = serde_json::from_str(&fs::read_to_string(&cfg).unwrap()).unwrap();
        let entry = after
            .get("mcpServers")
            .and_then(|v| v.get(MCP_KEY))
            .unwrap();
        assert_eq!(
            entry.get("command").and_then(|v| v.as_str()),
            Some(launcher.to_string_lossy().as_ref())
        );
        assert_eq!(
            entry.get("env").and_then(|e| e.get(ENV_PORT_KEY)),
            Some(&json!("12345"))
        );
    }

    #[test]
    fn disconnect_keeps_mcp_servers_when_only_key() {
        let _g = SERIAL.lock().unwrap();
        reset_backup_tracker();
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("cfg.json");
        fs::write(&cfg, "{}").unwrap();
        let launcher = launcher_for_tests(dir.path());

        connect_at_path(&cfg, &launcher, 9876).unwrap();
        reset_backup_tracker();
        disconnect_at_path(&cfg).unwrap();

        let after: Value = serde_json::from_str(&fs::read_to_string(&cfg).unwrap()).unwrap();
        let servers = after.get("mcpServers").unwrap().as_object().unwrap();
        assert!(!servers.contains_key(MCP_KEY));
        assert!(after.get("mcpServers").unwrap().is_object());
    }

    #[test]
    fn connect_is_idempotent_no_extra_backup() {
        let _g = SERIAL.lock().unwrap();
        reset_backup_tracker();
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("cfg.json");
        fs::write(&cfg, "{}").unwrap();
        let launcher = launcher_for_tests(dir.path());

        connect_at_path(&cfg, &launcher, 9876).unwrap();
        let backups_after_first = count_backups(dir.path(), "cfg.json");
        let mtime_after_first = fs::metadata(&cfg).unwrap().modified().unwrap();

        // Second call: same args. No file change, no new backup.
        std::thread::sleep(std::time::Duration::from_millis(20));
        connect_at_path(&cfg, &launcher, 9876).unwrap();
        let backups_after_second = count_backups(dir.path(), "cfg.json");
        let mtime_after_second = fs::metadata(&cfg).unwrap().modified().unwrap();

        assert_eq!(backups_after_first, backups_after_second);
        assert_eq!(mtime_after_first, mtime_after_second);
    }

    fn count_backups(dir: &Path, stem: &str) -> usize {
        let prefix = format!("{}.qontinui-backup.", stem);
        fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
            .count()
    }

    #[test]
    fn drift_detection_command_path() {
        let _g = SERIAL.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("cfg.json");
        let stale = dir.path().join("stale-launcher.exe");
        fs::write(&stale, b"#!stale").unwrap();
        let body = json!({
            "mcpServers": {
                MCP_KEY: {
                    "command": stale.to_string_lossy(),
                    "args": [],
                    "env": { ENV_PORT_KEY: "9876" }
                }
            }
        });
        fs::write(&cfg, serde_json::to_string_pretty(&body).unwrap()).unwrap();
        let real_launcher = launcher_for_tests(dir.path());

        let state = connection_state_for(&cfg, &real_launcher, 9876);
        match state {
            ConnectionState::Drifted {
                current_command,
                current_port,
            } => {
                assert_eq!(current_port, Some(9876));
                assert!(current_command.to_string_lossy().contains("stale-launcher"));
            }
            other => panic!("expected Drifted, got {:?}", other),
        }
    }

    #[test]
    fn drift_detection_port_only() {
        let _g = SERIAL.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("cfg.json");
        let launcher = launcher_for_tests(dir.path());
        let body = json!({
            "mcpServers": {
                MCP_KEY: {
                    "command": launcher.to_string_lossy(),
                    "args": [],
                    "env": { ENV_PORT_KEY: "11111" }
                }
            }
        });
        fs::write(&cfg, serde_json::to_string_pretty(&body).unwrap()).unwrap();

        let state = connection_state_for(&cfg, &launcher, 22222);
        match state {
            ConnectionState::Drifted {
                current_command,
                current_port,
            } => {
                assert_eq!(current_port, Some(11111));
                assert!(paths_equal(&current_command, &launcher));
            }
            other => panic!("expected Drifted, got {:?}", other),
        }
    }

    #[test]
    fn detect_returns_config_parse_error_on_malformed_json() {
        let _g = SERIAL.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("cfg.json");
        // Truncated mid-object — same shape as the AC #6 manual test.
        fs::write(&cfg, r#"{"mcpServers": {"foo": "#).unwrap();
        let launcher = launcher_for_tests(dir.path());

        let state = connection_state_for(&cfg, &launcher, 9876);
        match state {
            ConnectionState::ConfigParseError { path } => assert_eq!(path, cfg),
            other => panic!("expected ConfigParseError, got {:?}", other),
        }
    }

    #[test]
    fn malformed_json_returns_parse_failed() {
        let _g = SERIAL.lock().unwrap();
        reset_backup_tracker();
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("cfg.json");
        fs::write(&cfg, "{ this is not json").unwrap();
        let launcher = launcher_for_tests(dir.path());

        let err = connect_at_path(&cfg, &launcher, 9876).unwrap_err();
        match err {
            ClientError::ConfigParseFailed { path } => assert_eq!(path, cfg),
            other => panic!("expected ConfigParseFailed, got {:?}", other),
        }
    }
}
