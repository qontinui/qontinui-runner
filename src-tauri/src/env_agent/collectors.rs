//! Best-effort, fail-open, SECRET-FREE collectors for the env-capture agent.
//!
//! Each collector returns a [`Section`] — a `serde_json::Map` whose values are
//! ALWAYS `Value::String`. The envelope contract (see `mod.rs`) requires every
//! section value to be a string.
//!
//! ## Secret-safety invariant (load-bearing)
//!
//! NONE of these collectors may emit a secret VALUE. We capture only NAMES and
//! TOPOLOGY:
//! - `services`: URLs are parsed with the `url` crate and userinfo is STRIPPED
//!   (`set_username("")` + `set_password(None)`) so a DSN like
//!   `postgres://u:pw@h:5432/db` never leaks `pw`. We emit only scheme/host/port.
//! - `db_schema`: schema/table NAMES + counts + version strings — no row data.
//! - `versions`: package/tool version strings parsed from manifests — no secrets.
//! - `env_contract`: env var NAMES only (allowlisted by prefix), value `"present"`.
//!   The VALUE is structurally dropped — we never read `std::env::var(name)`.
//! - `claude_accounts`: account NAMES + selection mode + credential/shortcut
//!   PRESENCE flags. The credential file (`.credentials.json`) is checked for
//!   EXISTENCE only — it is NEVER opened, so an OAuth token can't leak.
//!
//! The `secret_safety_*` unit tests below pin this invariant.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use serde_json::{Map, Value};

/// A captured section. Every value is a `Value::String` per the envelope
/// contract.
pub type Section = Map<String, Value>;

/// Helper: insert a string-valued key into a section.
fn put(section: &mut Section, key: &str, value: impl Into<String>) {
    section.insert(key.to_string(), Value::String(value.into()));
}

/// Strip userinfo (username + password) from a URL string and return a
/// secret-free `scheme://host[:port]` rendering. Returns `None` when the input
/// doesn't parse as a URL with a host. NEVER includes path/query/userinfo.
///
/// This is the single choke point that guarantees a DSN password can't leak
/// into the `services` section.
fn sanitize_url(raw: &str) -> Option<String> {
    let mut parsed = url::Url::parse(raw).ok()?;
    // Structurally remove credentials. `set_username`/`set_password` return
    // Err only for cannot-be-a-base URLs (which have no host anyway, filtered
    // below); ignore the result.
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    let host = parsed.host_str()?.to_string();
    let scheme = parsed.scheme().to_string();
    match parsed.port() {
        Some(port) => Some(format!("{scheme}://{host}:{port}")),
        None => Some(format!("{scheme}://{host}")),
    }
}

/// Probe whether `127.0.0.1:port` accepts a TCP connection within 200ms.
/// Returns `"listening"` or `"closed"`. Purely a topology signal — no data is
/// exchanged.
fn probe_local_port(port: u16) -> &'static str {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    match std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)) {
        Ok(_) => "listening",
        Err(_) => "closed",
    }
}

/// Known dev-topology service ports (per CLAUDE.md "Service Architecture &
/// Ports"). Each entry is `(logical_name, port)`. The collector probes each and
/// records `"<name>"` → `"listening"|"closed"`.
const KNOWN_DEV_PORTS: &[(&str, u16)] = &[
    ("backend", 8000),
    ("frontend", 3001),
    ("runner_vite", 1420),
    ("runner_mcp", 9876),
    ("supervisor", 9875),
    ("postgres", 5432),
    ("postgres_alt", 5433),
    ("redis", 6379),
    ("minio_api", 9000),
    ("minio_console", 9001),
];

/// Collect the `services` section: secret-free topology derived from the active
/// profile (`profiles::load`) plus liveness probes of the known dev ports.
///
/// Returns `None` only if NOTHING could be collected (never happens in practice
/// — the port probes always yield at least the known-port set), so the
/// isolation driver in `mod.rs` keeps the section.
pub async fn collect_services() -> Option<Section> {
    let mut section = Section::new();

    // Topology from the active profile — URLs sanitized (userinfo stripped).
    let profile = crate::profiles::load();
    if let Some(sanitized) = sanitize_url(&profile.database_url) {
        put(&mut section, "database_url", sanitized);
    }
    if let Some(redis) = profile.redis_url.as_deref() {
        if let Some(sanitized) = sanitize_url(redis) {
            put(&mut section, "redis_url", sanitized);
        }
    }
    if let Some(coord) = profile.coord_url.as_deref() {
        if let Some(sanitized) = sanitize_url(coord) {
            put(&mut section, "coord_url", sanitized);
        }
    }
    if let Some(blob) = profile.blob.as_ref() {
        // `kind` is a non-secret topology label (minio / s3). The endpoint is a
        // URL — sanitize it. Access/secret keys are NEVER emitted.
        put(&mut section, "blob_kind", blob.kind.clone());
        if let Some(endpoint) = blob.endpoint.as_deref() {
            if let Some(sanitized) = sanitize_url(endpoint) {
                put(&mut section, "blob_endpoint", sanitized);
            }
        }
        if let Some(bucket) = blob.bucket.as_deref() {
            put(&mut section, "blob_bucket", bucket.to_string());
        }
    }

    // Liveness probes of the known dev ports. Each probe is bounded at 200ms.
    for (name, port) in KNOWN_DEV_PORTS {
        let key = format!("port_{name}_{port}");
        put(&mut section, &key, probe_local_port(*port));
    }

    if section.is_empty() {
        None
    } else {
        Some(section)
    }
}

/// Collect the `db_schema` section from the live PG pool published by the
/// binary at boot (`super::publish_pg_pool`). Each query is `.ok()`-guarded so
/// a partial PG (e.g. no `alembic_version` table) still yields what it can.
///
/// THIS is the high-value collector — it catches stale-schema drift (a machine
/// running an old alembic head, or missing tables). Returns `None` if PG is
/// unreachable so the section is omitted entirely.
pub async fn collect_db_schema() -> Option<Section> {
    let pool = super::pg_pool()?;
    let client = pool.get().await.ok()?;

    let mut section = Section::new();

    // server_version — `SELECT version()`.
    if let Ok(row) = client.query_one("SELECT version()", &[]).await {
        if let Ok(v) = row.try_get::<_, String>(0) {
            put(&mut section, "server_version", v);
        }
    }

    // alembic_head — `SELECT version_num FROM alembic_version`. Table may be
    // absent on a bare PG; guarded.
    if let Ok(row) = client
        .query_one("SELECT version_num FROM alembic_version", &[])
        .await
    {
        if let Ok(v) = row.try_get::<_, String>(0) {
            put(&mut section, "alembic_head", v);
        }
    }

    // Schema list — names only.
    if let Ok(rows) = client
        .query(
            "SELECT schema_name FROM information_schema.schemata ORDER BY schema_name",
            &[],
        )
        .await
    {
        let names: Vec<String> = rows
            .iter()
            .filter_map(|r| r.try_get::<_, String>(0).ok())
            .collect();
        if !names.is_empty() {
            put(&mut section, "schemas", names.join(","));
        }
    }

    // Per-schema table counts — grouped from information_schema.tables. Values
    // are stringified counts. Key form: `tables_<schema>`.
    if let Ok(rows) = client
        .query(
            "SELECT table_schema, count(*)::bigint \
             FROM information_schema.tables \
             WHERE table_type = 'BASE TABLE' \
             GROUP BY table_schema ORDER BY table_schema",
            &[],
        )
        .await
    {
        for r in &rows {
            let schema: String = match r.try_get::<_, String>(0) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let count: i64 = r.try_get::<_, i64>(1).unwrap_or(0);
            put(&mut section, &format!("tables_{schema}"), count.to_string());
        }
    }

    if section.is_empty() {
        None
    } else {
        Some(section)
    }
}

/// Run a bounded `<cmd> --version` (3s budget) and return the trimmed first
/// line of stdout (falling back to stderr). Returns `None` on spawn failure,
/// timeout, or non-zero exit. Mirrors `fleet::detect_claude_code_now`'s bounded
/// subprocess pattern.
fn version_of(cmd: &str, args: &[&str]) -> Option<String> {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::Instant;

    let started = Instant::now();
    let mut child = crate::process_helpers::no_window(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    let deadline = started + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut out = String::new();
                if let Some(mut so) = child.stdout.take() {
                    let _ = so.read_to_string(&mut out);
                }
                if out.trim().is_empty() {
                    if let Some(mut se) = child.stderr.take() {
                        let _ = se.read_to_string(&mut out);
                    }
                }
                let first = out.lines().next().unwrap_or("").trim().to_string();
                return if first.is_empty() { None } else { Some(first) };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}

/// Pull a `name = "x.y.z"` value out of a `[section]` of a Cargo.toml string.
/// Tiny hand-roll to avoid a toml-parse dependency in the hot path; tolerant of
/// missing keys (returns `None`). Looks only within the requested top-level
/// `[table]`.
fn cargo_toml_value(contents: &str, table: &str, key: &str) -> Option<String> {
    let mut in_table = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_table = trimmed == format!("[{table}]");
            continue;
        }
        if in_table {
            if let Some(rest) = trimmed.strip_prefix(key) {
                let rest = rest.trim_start();
                if let Some(after_eq) = rest.strip_prefix('=') {
                    let val = after_eq.trim().trim_matches('"');
                    if !val.is_empty() {
                        return Some(val.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Resolve the directory of THIS crate's `Cargo.toml`. Prefer the compile-time
/// `CARGO_MANIFEST_DIR`; that points at `.../qontinui-runner/src-tauri` where
/// the runner's Cargo.toml lives.
fn runner_manifest_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Collect the `versions` section: rust/tauri from this crate's Cargo.toml,
/// node deps from the nearest package.json, python from the web-backend
/// pyproject.toml, plus bounded `--version` shells where the tools resolve.
/// Missing inputs → key absent, never an error.
pub fn collect_versions() -> Section {
    let mut section = Section::new();

    // ---- Cargo.toml (this crate) ----
    let manifest_dir = runner_manifest_dir();
    let cargo_toml = manifest_dir.join("Cargo.toml");
    if let Ok(contents) = std::fs::read_to_string(&cargo_toml) {
        if let Some(v) = cargo_toml_value(&contents, "package", "version") {
            put(&mut section, "runner_crate_version", v);
        }
        // tauri dependency line: `tauri = { version = "2.5", ... }` — pull the
        // version substring out of the dependency value.
        if let Some(tauri_ver) = dependency_version(&contents, "tauri") {
            put(&mut section, "tauri", tauri_ver);
        }
    }

    // ---- nearest package.json (walk up from the qontinui-web frontend if
    // present, else from the runner dir) ----
    if let Some(pkg) = nearest_package_json(&manifest_dir) {
        if let Ok(contents) = std::fs::read_to_string(&pkg) {
            if let Ok(json) = serde_json::from_str::<Value>(&contents) {
                if let Some(name) = json.get("name").and_then(|v| v.as_str()) {
                    put(&mut section, "node_package_name", name.to_string());
                }
                if let Some(ver) = json.get("version").and_then(|v| v.as_str()) {
                    put(&mut section, "node_package_version", ver.to_string());
                }
                // A couple of high-signal deps if present (names + declared
                // ranges — these are public version constraints, not secrets).
                for dep in ["next", "react", "typescript"] {
                    if let Some(range) = json
                        .get("dependencies")
                        .and_then(|d| d.get(dep))
                        .or_else(|| json.get("devDependencies").and_then(|d| d.get(dep)))
                        .and_then(|v| v.as_str())
                    {
                        put(&mut section, &format!("node_dep_{dep}"), range.to_string());
                    }
                }
            }
        }
    }

    // ---- web-backend pyproject.toml ----
    if let Some(pyproject) = web_backend_pyproject(&manifest_dir) {
        if let Ok(contents) = std::fs::read_to_string(&pyproject) {
            // `[tool.poetry.dependencies]` carries `python = "^3.12"`.
            if let Some(py) = cargo_toml_value(&contents, "tool.poetry.dependencies", "python") {
                put(&mut section, "python_constraint", py);
            }
        }
    }

    // ---- bounded `--version` shells (optional, 3s budget each) ----
    if let Some(v) = version_of("rustc", &["--version"]) {
        put(&mut section, "rustc", v);
    }
    if let Some(v) = version_of("node", &["--version"]) {
        put(&mut section, "node", v);
    }
    if let Some(v) = version_of("python", &["--version"]) {
        put(&mut section, "python", v);
    }

    section
}

/// Extract a `version = "x"` substring from a Cargo dependency line of the form
/// `name = { version = "x", ... }` or `name = "x"`. Best-effort.
fn dependency_version(contents: &str, dep: &str) -> Option<String> {
    let mut in_deps = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_deps = trimmed == "[dependencies]";
            continue;
        }
        if in_deps && trimmed.starts_with(dep) {
            // Match `dep =` (avoid prefix collisions like `tauri-build`).
            let after = trimmed[dep.len()..].trim_start();
            if !after.starts_with('=') {
                continue;
            }
            // Inline-table form: find `version = "..."`.
            if let Some(idx) = trimmed.find("version") {
                let tail = &trimmed[idx..];
                if let Some(eq) = tail.find('=') {
                    let v = tail[eq + 1..].trim();
                    let v = v.trim_start_matches('"');
                    if let Some(end) = v.find('"') {
                        return Some(v[..end].to_string());
                    }
                }
            }
            // Bare-string form: `dep = "x"`.
            let v = after.trim_start_matches('=').trim().trim_matches('"');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Walk up from `start` looking for a `package.json`. Bounded to 6 ancestors to
/// avoid scanning the whole filesystem. Returns the first hit.
fn nearest_package_json(start: &std::path::Path) -> Option<std::path::PathBuf> {
    // Prefer the qontinui-web frontend if reachable from a known root layout.
    // Otherwise walk up from the runner manifest dir.
    let mut cur = Some(start);
    for _ in 0..6 {
        let dir = cur?;
        let candidate = dir.join("package.json");
        if candidate.is_file() {
            return Some(candidate);
        }
        // Try a sibling qontinui-web/frontend/package.json under the parent
        // (covers the monorepo-root layout).
        let web_pkg = dir
            .join("qontinui-web")
            .join("frontend")
            .join("package.json");
        if web_pkg.is_file() {
            return Some(web_pkg);
        }
        cur = dir.parent();
    }
    None
}

/// Locate the web-backend `pyproject.toml` by walking up to a monorepo root and
/// probing `qontinui-web/backend/pyproject.toml`. Returns `None` when not
/// reachable (e.g. a production machine without the source checkout).
fn web_backend_pyproject(start: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut cur = Some(start);
    for _ in 0..6 {
        let dir = cur?;
        let candidate = dir
            .join("qontinui-web")
            .join("backend")
            .join("pyproject.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        cur = dir.parent();
    }
    None
}

/// Allowlisted env-var name prefixes. We keep ONLY names matching one of these;
/// the VALUE is structurally dropped (`"present"`). `DATABASE_URL` is an exact
/// name (no trailing prefix match) but listed here as a prefix so
/// `DATABASE_URL_REPLICA` etc. also match — still names only.
const ALLOWLIST_PREFIXES: &[&str] = &[
    "QONTINUI_",
    "COORD_",
    "REDIS_",
    "S3_",
    "MINIO_",
    "RUNNER_",
    "DATABASE_URL",
    "PG",
];

/// Collect the `env_contract` section: env var NAMES (allowlisted by prefix)
/// with value `"present"`. The VALUE is NEVER read — we iterate the names and
/// emit a constant, so a secret in `QONTINUI_SECRET_TOKEN` contributes only the
/// NAME, never the secret.
pub fn collect_env_contract() -> Section {
    let mut section = Section::new();
    for (name, _value) in std::env::vars() {
        if ALLOWLIST_PREFIXES.iter().any(|p| name.starts_with(p)) {
            // value DELIBERATELY discarded — names only.
            put(&mut section, &name, "present");
        }
    }
    section
}

// ============================================================================
// claude_accounts — SECRET-FREE roster topology
// ============================================================================
//
// Captures the machine's Claude Code account roster (which config dirs exist,
// the selection mode, and per-account credential/shortcut PRESENCE) so the
// backend can drift-check a machine's account wiring. This is a pure
// read + file-EXISTENCE-check collector: it never opens a credential file.
//
// CRATE-BOUNDARY NOTE: the roster module (`crate::claude_accounts`) lives in
// the BIN crate (declared in `main.rs`), so this LIB-crate collector cannot
// call it. We re-implement the tiny roster read inline against the same
// on-disk shape.

/// On-disk probe of `claude-accounts.json` — only the fields this collector
/// needs. Selection mode is read as a raw string (snake_case on disk) to avoid
/// depending on the bin-crate `AccountSelectionMode` enum.
#[derive(serde::Deserialize, Default)]
struct AccountsFileProbe {
    #[serde(default)]
    claude_config_dirs: Vec<String>,
    #[serde(default)]
    account_selection_mode: Option<String>,
    #[serde(default)]
    claude_account_launch_commands: HashMap<String, String>,
}

/// On-disk probe of the fallback `settings.json`. SHAPE DIFFERS from
/// `claude-accounts.json`: the selection mode is NESTED at
/// `ai.claude_cli.account_selection_mode` (mirrors `claude_accounts.rs`'s
/// migration `SettingsProbe`). A flat lookup here silently yields the default —
/// a bug the settings-fallback test guards against.
#[derive(serde::Deserialize, Default)]
struct SettingsCliProbe {
    #[serde(default)]
    account_selection_mode: Option<String>,
}
#[derive(serde::Deserialize, Default)]
struct SettingsAiProbe {
    #[serde(default)]
    claude_cli: SettingsCliProbe,
}
#[derive(serde::Deserialize, Default)]
struct SettingsFileProbe {
    #[serde(default)]
    claude_config_dirs: Vec<String>,
    #[serde(default)]
    claude_account_launch_commands: HashMap<String, String>,
    #[serde(default)]
    ai: SettingsAiProbe,
}

/// Derive an account NAME from a config-dir path: the basename with a leading
/// `.claude-` prefix stripped (`.../.claude-gmail` → `gmail`). Splits on BOTH
/// separators so Windows (`\`) and POSIX (`/`) paths behave identically. When
/// the basename has no `.claude-` prefix, the basename is used as-is.
fn account_name(config_dir: &str) -> String {
    let base = config_dir
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(config_dir);
    base.strip_prefix(".claude-").unwrap_or(base).to_string()
}

/// Best-effort read of the user's shell profiles (PowerShell + `~/.bashrc`),
/// concatenated. Returns `None` when NONE are readable — the caller then omits
/// the `shortcut_*` keys entirely (fail-open: never error, never guess).
fn read_shell_profiles() -> Option<String> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(docs) = dirs::document_dir() {
        candidates.push(
            docs.join("WindowsPowerShell")
                .join("Microsoft.PowerShell_profile.ps1"),
        );
        candidates.push(
            docs.join("PowerShell")
                .join("Microsoft.PowerShell_profile.ps1"),
        );
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".bashrc"));
        candidates.push(home.join(".zshrc"));
    }
    let mut combined = String::new();
    for path in candidates {
        if let Ok(text) = std::fs::read_to_string(&path) {
            combined.push_str(&text);
            combined.push('\n');
        }
    }
    if combined.is_empty() {
        None
    } else {
        Some(combined)
    }
}

/// Collect the `claude_accounts` section. Resolves the roster from
/// `dirs::config_dir()` and the user's shell profiles, then delegates to the
/// injectable core. Returns `None` when neither the accounts file nor any
/// config dir is found (so the isolation driver omits the section).
pub fn collect_claude_accounts() -> Option<Section> {
    let config_root = dirs::config_dir()?;
    let profiles = read_shell_profiles();
    collect_claude_accounts_from(&config_root, profiles.as_deref())
}

/// Injectable core so tests can drive a temp config root (and control the
/// shell-profile text) without touching the real environment.
///
/// SECRET-SAFETY INVARIANT (pinned by `secret_safety_claude_accounts_*`): this
/// function NEVER opens `.credentials.json` (or `.claude.json`) — it emits only
/// names/topology and `present`/`absent` EXISTENCE flags.
fn collect_claude_accounts_from(
    config_root: &Path,
    profiles_text: Option<&str>,
) -> Option<Section> {
    let runner_dir = config_root.join("com.qontinui.runner");
    let accounts_file = runner_dir.join("claude-accounts.json");
    let settings_file = runner_dir.join("settings.json");

    let mut config_dirs: Vec<String> = Vec::new();
    let mut selection_mode = String::from("least_usage");
    let mut launch_commands: HashMap<String, String> = HashMap::new();
    let mut accounts_file_found = false;

    // Primary source: claude-accounts.json (mode is top-level).
    if let Ok(contents) = std::fs::read_to_string(&accounts_file) {
        accounts_file_found = true;
        if let Ok(p) = serde_json::from_str::<AccountsFileProbe>(&contents) {
            config_dirs = p.claude_config_dirs;
            if let Some(m) = p.account_selection_mode.filter(|m| !m.is_empty()) {
                selection_mode = m;
            }
            launch_commands = p.claude_account_launch_commands;
        }
    } else if let Ok(contents) = std::fs::read_to_string(&settings_file) {
        // Fallback source: settings.json (mode is NESTED under ai.claude_cli).
        if let Ok(p) = serde_json::from_str::<SettingsFileProbe>(&contents) {
            config_dirs = p.claude_config_dirs;
            if let Some(m) =
                p.ai.claude_cli
                    .account_selection_mode
                    .filter(|m| !m.is_empty())
            {
                selection_mode = m;
            }
            launch_commands = p.claude_account_launch_commands;
        }
    }

    // Omit the section only when there is genuinely nothing to report.
    if !accounts_file_found && config_dirs.is_empty() {
        return None;
    }

    // (name, config_dir) pairs, sorted by name for stable output.
    let mut pairs: Vec<(String, String)> = config_dirs
        .iter()
        .map(|d| (account_name(d), d.clone()))
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));

    let mut section = Section::new();
    put(&mut section, "selection_mode", selection_mode);
    let names: Vec<&str> = pairs.iter().map(|(n, _)| n.as_str()).collect();
    put(&mut section, "accounts", names.join(","));

    // auth_<name>: EXISTENCE of <config_dir>/.credentials.json — NEVER opened.
    for (name, dir) in &pairs {
        let creds = Path::new(dir).join(".credentials.json");
        let flag = if creds.exists() { "present" } else { "absent" };
        put(&mut section, &format!("auth_{name}"), flag);
    }

    // shortcut_<name>: best-effort profile mention. FAIL-OPEN — when no profile
    // is readable we omit these keys rather than guessing `absent`.
    if let Some(text) = profiles_text {
        for (name, dir) in &pairs {
            let mentioned = text.contains(dir.as_str())
                || launch_commands
                    .get(dir)
                    .map(|c| !c.is_empty() && text.contains(c.as_str()))
                    .unwrap_or(false);
            let flag = if mentioned { "present" } else { "absent" };
            put(&mut section, &format!("shortcut_{name}"), flag);
        }
    }

    Some(section)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_url_strips_password() {
        let s = sanitize_url("postgres://user:supersecret@dbhost:5432/mydb").unwrap();
        assert!(!s.contains("supersecret"), "password leaked: {s}");
        assert!(!s.contains("user"), "username leaked: {s}");
        assert_eq!(s, "postgres://dbhost:5432");
    }

    #[test]
    fn sanitize_url_handles_no_port_no_userinfo() {
        let s = sanitize_url("redis://localhost").unwrap();
        assert_eq!(s, "redis://localhost");
    }

    #[test]
    fn sanitize_url_rejects_garbage() {
        assert!(sanitize_url("not a url").is_none());
    }

    #[test]
    fn cargo_toml_value_reads_package_version() {
        let toml = "[package]\nname = \"x\"\nversion = \"1.2.3\"\n[dependencies]\nfoo = \"9\"\n";
        assert_eq!(
            cargo_toml_value(toml, "package", "version").as_deref(),
            Some("1.2.3")
        );
    }

    #[test]
    fn dependency_version_inline_table() {
        let toml = "[dependencies]\ntauri = { version = \"2.5\", features = [] }\n";
        assert_eq!(dependency_version(toml, "tauri").as_deref(), Some("2.5"));
    }

    #[test]
    fn dependency_version_bare_string() {
        let toml = "[dependencies]\nserde_json = \"1\"\n";
        assert_eq!(dependency_version(toml, "serde_json").as_deref(), Some("1"));
    }

    /// Secret-safety: a secret-bearing env var contributes only its NAME, never
    /// the value, to the env_contract section.
    #[test]
    fn secret_safety_env_contract_emits_name_not_value() {
        let var = "QONTINUI_SECRET_TOKEN_TEST_UNIQUE";
        std::env::set_var(var, "supersecret123");
        let section = collect_env_contract();
        std::env::remove_var(var);

        // Name present, value "present".
        assert_eq!(
            section.get(var).and_then(|v| v.as_str()),
            Some("present"),
            "allowlisted var name should be captured as present"
        );
        // The secret value must appear NOWHERE in the serialized section.
        let json = serde_json::to_string(&section).unwrap();
        assert!(
            !json.contains("supersecret123"),
            "secret value leaked into env_contract: {json}"
        );
    }

    /// Secret-safety: a password-bearing DSN in the profile never leaks the
    /// password through the services-section URL sanitizer.
    #[test]
    fn secret_safety_dsn_password_never_leaks_via_sanitizer() {
        let sanitized = sanitize_url("postgres://u:pw@h:5432/db").unwrap();
        assert!(!sanitized.contains("pw"), "password leaked: {sanitized}");
        assert!(!sanitized.contains("u@"), "userinfo leaked: {sanitized}");
    }

    // ---- claude_accounts collector ----

    /// Write `claude-accounts.json` under `<root>/com.qontinui.runner/`.
    fn write_roster(runner_dir: &Path, json: &str) {
        std::fs::create_dir_all(runner_dir).unwrap();
        std::fs::write(runner_dir.join("claude-accounts.json"), json).unwrap();
    }

    /// Secret-safety: an OAuth-token-bearing `.credentials.json` must never be
    /// opened — only its EXISTENCE is reported. The token bytes (and any file
    /// contents) must appear NOWHERE in the serialized section.
    #[test]
    fn secret_safety_claude_accounts_never_leaks_token() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let runner = root.join("com.qontinui.runner");

        let acct = root.join(".claude-secretacct");
        std::fs::create_dir_all(&acct).unwrap();
        let token = "sk-ant-oauth-SUPERSECRET-TOKEN-VALUE";
        std::fs::write(
            acct.join(".credentials.json"),
            format!("{{\"access_token\":\"{token}\"}}"),
        )
        .unwrap();

        write_roster(
            &runner,
            &format!(
                "{{\"claude_config_dirs\":[{:?}],\"account_selection_mode\":\"least_usage\"}}",
                acct.to_string_lossy()
            ),
        );

        let section = collect_claude_accounts_from(root, None).expect("section present");
        let json = serde_json::to_string(&section).unwrap();

        assert_eq!(
            section.get("auth_secretacct").and_then(|v| v.as_str()),
            Some("present"),
            "credential existence must be reported: {json}"
        );
        assert!(json.contains("present"), "presence marker missing: {json}");
        assert!(!json.contains(token), "OAuth token leaked: {json}");
        assert!(
            !json.contains("access_token"),
            "credential file contents leaked: {json}"
        );
    }

    /// Behavior: sorted stripped names, correct mode, auth presence reflects the
    /// credential file, and `None` when neither the file nor any dir exists.
    #[test]
    fn claude_accounts_reflects_roster_and_auth_presence() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let runner = root.join("com.qontinui.runner");

        let gmail = root.join(".claude-gmail");
        let work = root.join(".claude-work");
        std::fs::create_dir_all(&gmail).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        // Credential present for gmail only.
        std::fs::write(gmail.join(".credentials.json"), "{}").unwrap();

        write_roster(
            &runner,
            &format!(
                // deliberately unsorted (work before gmail)
                "{{\"claude_config_dirs\":[{:?},{:?}],\"account_selection_mode\":\"manual\"}}",
                work.to_string_lossy(),
                gmail.to_string_lossy()
            ),
        );

        let section = collect_claude_accounts_from(root, None).unwrap();
        assert_eq!(
            section.get("accounts").and_then(|v| v.as_str()),
            Some("gmail,work"),
            "names must be stripped + sorted"
        );
        assert_eq!(
            section.get("selection_mode").and_then(|v| v.as_str()),
            Some("manual")
        );
        assert_eq!(
            section.get("auth_gmail").and_then(|v| v.as_str()),
            Some("present")
        );
        assert_eq!(
            section.get("auth_work").and_then(|v| v.as_str()),
            Some("absent")
        );

        // None when neither the accounts file nor any config dir is found.
        let empty = tempfile::tempdir().unwrap();
        assert!(collect_claude_accounts_from(empty.path(), None).is_none());
    }

    /// Vet caveat: with no `claude-accounts.json`, the mode must be read from
    /// the NESTED `ai.claude_cli.account_selection_mode` in `settings.json`. A
    /// flat lookup would silently return the default — this guards that bug.
    #[test]
    fn claude_accounts_settings_fallback_reads_nested_mode() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let runner = root.join("com.qontinui.runner");
        std::fs::create_dir_all(&runner).unwrap();

        let acct = root.join(".claude-personal");
        std::fs::create_dir_all(&acct).unwrap();

        // Only settings.json (no claude-accounts.json), mode nested.
        std::fs::write(
            runner.join("settings.json"),
            format!(
                "{{\"claude_config_dirs\":[{:?}],\
                  \"ai\":{{\"claude_cli\":{{\"account_selection_mode\":\"manual\"}}}}}}",
                acct.to_string_lossy()
            ),
        )
        .unwrap();

        let section = collect_claude_accounts_from(root, None).unwrap();
        assert_eq!(
            section.get("selection_mode").and_then(|v| v.as_str()),
            Some("manual"),
            "mode must come from nested ai.claude_cli, not a flat lookup"
        );
        assert_eq!(
            section.get("accounts").and_then(|v| v.as_str()),
            Some("personal")
        );
    }

    /// Shortcuts: detected via config-dir path OR launch-command mention in the
    /// profile text; omitted entirely when no profile is readable (fail-open).
    #[test]
    fn claude_accounts_shortcuts_from_profile_text() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let runner = root.join("com.qontinui.runner");

        let gmail = root.join(".claude-gmail");
        let work = root.join(".claude-work");
        std::fs::create_dir_all(&gmail).unwrap();
        std::fs::create_dir_all(&work).unwrap();

        write_roster(
            &runner,
            &format!(
                "{{\"claude_config_dirs\":[{:?},{:?}],\
                  \"claude_account_launch_commands\":{{{:?}:\"clw\"}}}}",
                gmail.to_string_lossy(),
                work.to_string_lossy(),
                work.to_string_lossy()
            ),
        );

        // Profile mentions gmail's config-dir path and work's launch command.
        let profile = format!(
            "$env:CLAUDE_CONFIG_DIR='{}'\nSet-Alias clw launch-work\nclw\n",
            gmail.to_string_lossy()
        );

        let section = collect_claude_accounts_from(root, Some(&profile)).unwrap();
        assert_eq!(
            section.get("shortcut_gmail").and_then(|v| v.as_str()),
            Some("present"),
            "gmail detected via config-dir path"
        );
        assert_eq!(
            section.get("shortcut_work").and_then(|v| v.as_str()),
            Some("present"),
            "work detected via launch command 'clw'"
        );

        // No profile text → shortcut keys omitted.
        let no_profile = collect_claude_accounts_from(root, None).unwrap();
        assert!(no_profile.get("shortcut_gmail").is_none());
        assert!(no_profile.get("shortcut_work").is_none());
    }

    #[test]
    fn account_name_strips_claude_prefix_both_separators() {
        assert_eq!(account_name("C:\\Users\\x\\.claude-gmail"), "gmail");
        assert_eq!(account_name("/home/user/.claude-work"), "work");
        assert_eq!(account_name("/home/user/plain"), "plain");
        assert_eq!(account_name("C:\\Users\\x\\.claude-hotmail\\"), "hotmail");
    }
}
