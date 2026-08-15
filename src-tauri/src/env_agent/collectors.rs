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
use tracing::warn;

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
/// into the `services` section — and, since P2b slice 2, the same choke point
/// the local apply redacts through ([`super::apply_services`]). One
/// implementation, so the capture and the apply can never disagree about what
/// counts as secret-free.
pub(crate) fn sanitize_url(raw: &str) -> Option<String> {
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

/// Sanitize a PostgreSQL connection string that may be in EITHER supported
/// form: a `postgres://` URL, or libpq's `key=value` DSN
/// (`host=... port=... user=... password=... dbname=...`).
///
/// # Why this is not just [`sanitize_url`]
///
/// It was, and that silently cost the `services` section its single most
/// important key. `url::Url::parse` rejects a `key=value` DSN outright, so
/// `sanitize_url` returned `None`, and the caller's `if let Some(_)` simply
/// skipped `database_url` — no warning, no marker, nothing. The section then
/// looked *in sync* on a box whose database topology had never been captured at
/// all. That is exactly how the operator box came to sit on a retired database
/// while its drift report showed no database difference to reconcile: the
/// runner's own `legacy_env_fallback` default is a `key=value` DSN, so the form
/// most likely to be in play was the one form that could not be captured.
///
/// `tokio_postgres::Config` parses both forms and is already the parser this
/// crate uses for exactly this string (`database/pg/mod.rs`,
/// `env_agent::publish_pg_pool_from_url`), so this reuses it rather than
/// hand-rolling a DSN scanner.
///
/// Secret-free by the same contract as [`sanitize_url`]: only host and port are
/// read off the parsed config — `user`, `password` and `dbname` are never
/// touched — and the result is rendered in the canonical `postgres://host:port`
/// shape so the two input forms converge on ONE comparable value. Without that
/// normalization two boxes spelling the same server differently would read as
/// permanent drift.
pub(crate) fn sanitize_database_url(raw: &str) -> Option<String> {
    // URL form first: it is the canonical spelling, and keeping it on the
    // shared choke point means capture and apply cannot disagree about it.
    if let Some(sanitized) = sanitize_url(raw) {
        return Some(sanitized);
    }

    // libpq `key=value` form.
    let config: tokio_postgres::Config = raw.parse().ok()?;
    let host = match config.get_hosts().first()? {
        tokio_postgres::config::Host::Tcp(h) => h.clone(),
        // `Host::Unix` is a `#[cfg(unix)]` variant, so this arm is unreachable
        // on Windows — which is exactly why it cannot be the only socket check.
        #[allow(unreachable_patterns)]
        _ => return None,
    };
    // libpq reads a host beginning with `/` as a socket DIRECTORY rather than a
    // network host, and on Windows that spelling parses into `Host::Tcp`
    // carrying a filesystem path — so without this the section would publish
    // `postgres:///var/run/postgresql:5432` as if it were a comparable server.
    // A socket path has no cross-box host:port topology and is operator-local,
    // so report nothing rather than something misleading.
    if host.starts_with('/') || host.starts_with('\\') {
        return None;
    }
    // `get_ports()` is empty when the DSN omits `port`; libpq's default is 5432
    // and being explicit keeps two boxes that differ only in explicitness from
    // reading as drift.
    let port = config.get_ports().first().copied().unwrap_or(5432);
    Some(format!("postgres://{host}:{port}"))
}

/// A git remote, normalized to one canonical secret-free
/// `https://<host>/<owner>/<name>` rendering. Returns `None` when the input
/// does not name a host and a two-or-more segment path.
///
/// # Why this is not [`sanitize_url`]
///
/// [`sanitize_url`] returns `scheme://host[:port]` and **discards the path** —
/// which is the whole point for a DSN (the path is the database name, and the
/// comparable fact is the server) and exactly wrong here: a repository's
/// identity IS its path. Reusing it would collapse every GitHub remote on a box
/// to the single value `https://github.com`.
///
/// # Why normalization is load-bearing rather than cosmetic
///
/// `git@github.com:qontinui/qontinui-runner.git`,
/// `https://github.com/qontinui/qontinui-runner.git` and
/// `https://github.com/qontinui/qontinui-runner` are the same repository. Emit
/// them verbatim and two boxes that merely *cloned differently* read as
/// permanent drift that no apply can ever clear — the same failure mode
/// [`sanitize_database_url`] was written to avoid, where two spellings of one
/// server had to converge on ONE comparable value.
///
/// Secret-free by the same contract as its two siblings: userinfo is dropped
/// structurally, so a token-bearing remote
/// (`https://x-access-token:<pat>@github.com/owner/name`) cannot reach the
/// envelope. Only scheme-less SCP-style and `ssh://`/`git://`/`http(s)://`
/// remotes are recognised; anything else returns `None` for the caller to WARN
/// about rather than silently drop.
pub(crate) fn sanitize_git_remote(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    // SCP-style (`[user@]host:owner/name`) is not a URL and `url::Url` rejects
    // it, so rewrite it into one first. Detected by "has a colon that is not
    // followed by `//`" — `git@github.com:qontinui/x` vs `ssh://git@…`.
    let as_url = match trimmed.find("://") {
        Some(_) => trimmed.to_string(),
        None => {
            let (host_part, path) = trimmed.split_once(':')?;
            // Strip `user@`; the userinfo is dropped for the same reason
            // `sanitize_url` drops it, and `git@` carries no information anyway.
            let host = host_part.rsplit('@').next()?;
            if host.is_empty() || path.is_empty() {
                return None;
            }
            format!("ssh://{host}/{}", path.trim_start_matches('/'))
        }
    };

    let mut parsed = url::Url::parse(&as_url).ok()?;
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    let host = parsed.host_str()?.to_string();

    // Keep every path segment, so a self-hosted forge that nests groups
    // (`https://git.example.com/team/sub/name`) is not silently truncated to
    // its last two. Drop the `.git` suffix — the single most common spelling
    // difference between two clones of one repository.
    let segments: Vec<&str> = parsed
        .path()
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if segments.len() < 2 {
        return None;
    }
    let mut path = segments.join("/");
    if let Some(stripped) = path.strip_suffix(".git") {
        path = stripped.to_string();
    }
    if path.is_empty() {
        return None;
    }

    // Rendered as `https://` regardless of the input scheme: the scheme is a
    // property of how THIS box chose to clone, not of the repository, so
    // preserving it would reintroduce the drift this function exists to remove.
    // The port is preserved when the remote states one, since a forge on a
    // non-default port is a genuinely different host:port.
    match parsed.port() {
        Some(port) => Some(format!("https://{host}:{port}/{path}")),
        None => Some(format!("https://{host}/{path}")),
    }
}

/// The `repo_<owner>_<name>` key for a canonical remote, with every character
/// outside `[A-Za-z0-9._-]` folded to `_` so the key is a stable, flat envelope
/// key rather than a URL.
///
/// Built from the LAST TWO path segments, which is the owner/name pair on every
/// forge this project uses; a deeper self-hosted path still yields a stable key
/// because the full canonical URL remains the key's VALUE.
fn repo_key(canonical_url: &str) -> Option<String> {
    let path = canonical_url.split_once("://")?.1.split_once('/')?.1;
    let mut segments = path.rsplit('/');
    let name = segments.next()?;
    let owner = segments.next()?;
    if name.is_empty() || owner.is_empty() {
        return None;
    }
    let safe = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    Some(format!("repo_{}_{}", safe(owner), safe(name)))
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
    match sanitize_database_url(&profile.database_url) {
        Some(sanitized) => put(&mut section, "database_url", sanitized),
        // Do NOT drop this silently. A missing key is indistinguishable from
        // "in sync" downstream, which is precisely how a box on the wrong
        // database reported no database drift at all.
        None => warn!(
            "env capture: database_url is neither a URL nor a libpq key=value DSN with a \
             TCP host — omitting it from the services section, so this box's database \
             topology cannot be compared against canonical"
        ),
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

/// Why a configured `scope_root` was NOT used. Every variant means the operator
/// declared a capture scope and the agent measured somewhere else — the one
/// situation where falling back silently would be worse than the stale value,
/// because the resulting drift reading looks legitimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeRootRejection {
    /// The value was empty or all whitespace — indistinguishable from unset.
    Blank,
    /// The value was a RELATIVE path. Rejected because resolving it against the
    /// process's cwd would reintroduce the exact launch-dependence this whole
    /// mechanism exists to remove: the probe would land somewhere different
    /// depending on where the runner was started from.
    Relative,
    /// The path is absolute but is not an existing directory (missing, or a
    /// file). Handing it back as a cwd would make every probe spawn fail and
    /// silently zero the whole `versions` section.
    NotADirectory,
}

impl ScopeRootRejection {
    /// Operator-facing reason, used verbatim in the capture WARN and in
    /// `env show`'s `scope_root_status`.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Blank => "value is empty",
            Self::Relative => {
                "path is relative — a relative scope root resolves against the runner's \
                 launch directory, which is the non-determinism this setting exists to remove"
            }
            Self::NotADirectory => "path is not an existing directory",
        }
    }
}

/// WHICH kind of scope a capture's toolchain probes ran in.
///
/// The PATH is not comparable across boxes — a Windows home directory and a
/// Linux one differ while meaning exactly the same thing — but the KIND is.
/// That is what makes it safe to ship in the envelope and to compare: two
/// boxes both on [`Self::Default`] measured the same concept (their default
/// toolchain), whereas a [`Self::Declared`] box measured one specific tree and
/// is not describing the same quantity at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeScopeKind {
    /// An operator-declared `scope_root` was honoured — the numbers describe
    /// that tree, not the box.
    Declared,
    /// The home directory: the box's DEFAULT toolchain, i.e. what shim-based
    /// managers answer outside any project tree. Also the outcome when a
    /// configured value had to be dropped.
    Default,
    /// No home directory at all, so the probes inherited the runner's cwd.
    /// Not comparable with anything.
    Inherited,
}

impl ProbeScopeKind {
    /// The stable wire string carried in `versions.probe_scope_kind`.
    pub fn wire(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Default => "default",
            Self::Inherited => "inherited",
        }
    }
}

/// The resolved capture scope plus, when applicable, the configured value that
/// was dropped to reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeScope {
    /// Directory the probes run in. `None` means "spawn without setting a cwd"
    /// (inherit) — only reachable when there is no home directory at all.
    pub root: Option<std::path::PathBuf>,
    /// `Some` iff a `scope_root` was configured but NOT honoured.
    pub rejected: Option<ScopeRootRejection>,
    /// Which KIND of scope this is — the part that is comparable across boxes.
    pub kind: ProbeScopeKind,
}

/// Pure resolution of the declared capture scope from a configured value.
///
/// Resolution order, first hit wins:
/// 1. The configured `scope_root`, when it is non-blank, **absolute**, and an
///    existing directory.
/// 2. The user's home directory — the box's DEFAULT toolchain, i.e. what
///    shim-based managers answer outside any project tree.
/// 3. `None` (inherit). Only when there is no home directory at all.
///
/// An unusable configured value FALLS THROUGH rather than failing the capture:
/// the collectors are fail-open by construction, and a stale config entry must
/// not silently zero the whole `versions` section. It is reported via
/// [`ProbeScope::rejected`] so the fall-through is never invisible.
///
/// **Deliberately NOT the compile-time `CARGO_MANIFEST_DIR`.** That path
/// measures which source tree the binary was BUILT from, not the box — the same
/// confusion that makes `runner_crate_version` and friends repo-derived rather
/// than appliable. Anchoring the probe there would reintroduce that bug on the
/// observed keys, which are the only ones a P2b apply can actually move.
///
/// **Deliberately NOT the inherited cwd.** That is the defect this exists to
/// fix: it makes the captured version a function of how the runner was
/// launched, so the drift oracle cannot be trusted to compare like with like.
/// A relative configured path is rejected for the same reason — it is the
/// inherited cwd wearing a configured value's clothes.
pub fn resolve_probe_scope(configured: Option<&str>) -> ProbeScope {
    let home = || dirs::home_dir().filter(|h| h.is_dir());
    let Some(raw) = configured else {
        let root = home();
        return ProbeScope {
            kind: kind_for(&root),
            root,
            rejected: None,
        };
    };

    let trimmed = raw.trim();
    let rejection = if trimmed.is_empty() {
        Some(ScopeRootRejection::Blank)
    } else {
        let path = std::path::PathBuf::from(trimmed);
        if !path.is_absolute() {
            Some(ScopeRootRejection::Relative)
        } else if !path.is_dir() {
            Some(ScopeRootRejection::NotADirectory)
        } else {
            return ProbeScope {
                root: Some(path),
                rejected: None,
                kind: ProbeScopeKind::Declared,
            };
        }
    };

    // A REJECTED configured value lands on the home directory, so it reports
    // `default` — which is the truth about what was measured. The rejection
    // itself is reported separately (`rejected`, the capture WARN, `env show`);
    // conflating the two here would make the provenance lie about the
    // measurement in order to preserve the operator's intent.
    let root = home();
    ProbeScope {
        kind: kind_for(&root),
        root,
        rejected: rejection,
    }
}

/// Classify a root that came from the FALLBACK chain. An honoured declaration
/// is stamped [`ProbeScopeKind::Declared`] at its own return site and never
/// reaches here.
fn kind_for(root: &Option<std::path::PathBuf>) -> ProbeScopeKind {
    if root.is_some() {
        ProbeScopeKind::Default
    } else {
        ProbeScopeKind::Inherited
    }
}

/// Resolve the declared capture scope from the on-disk config, WARNing when a
/// configured value had to be dropped.
///
/// The warn is the point: a dropped scope root produces a capture that is
/// perfectly well-formed and quietly measured somewhere the operator did not
/// choose, so it cannot be caught by looking at the envelope. See
/// [`resolve_probe_scope`] for the resolution order.
pub fn probe_scope() -> ProbeScope {
    let configured = super::config::EnvAgentConfig::load().and_then(|c| c.scope_root);
    let scope = resolve_probe_scope(configured.as_deref());
    if let Some(rejection) = scope.rejected {
        tracing::warn!(
            "env_agent: ignoring configured scope_root {:?} ({}) — probing in {} instead; \
             fix with `qontinui-runner env scope-root --path <absolute-dir>`",
            configured.as_deref().unwrap_or(""),
            rejection.reason(),
            scope
                .root
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "the inherited working directory".to_string()),
        );
    }
    scope
}

/// The directory the toolchain `--version` probes run in. Thin accessor over
/// [`probe_scope`] for callers that only need the path.
pub fn probe_scope_root() -> Option<std::path::PathBuf> {
    probe_scope().root
}

/// Wall-clock budget for one `<cmd> --version` probe during CAPTURE.
///
/// Deliberately tight: capture runs on a 15-minute loop against three tools, and
/// a wedged shim must cost the runner seconds, not minutes. A probe that exceeds
/// it simply omits its key — the collectors are fail-open.
const CAPTURE_PROBE_BUDGET: Duration = Duration::from_secs(3);

/// Run a bounded `<cmd> --version` and return the trimmed first line of stdout
/// (falling back to stderr). Returns `None` on spawn failure, timeout, or
/// non-zero exit. Mirrors `fleet::detect_claude_code_now`'s bounded subprocess
/// pattern.
///
/// `cwd` is the declared capture scope (see [`probe_scope_root`]). `None`
/// inherits the parent's cwd — reserved for the no-home-directory case, since
/// an inherited cwd is exactly what makes this probe non-deterministic.
fn version_of(cmd: &str, args: &[&str], cwd: Option<&Path>) -> Option<String> {
    version_of_within(cmd, args, cwd, CAPTURE_PROBE_BUDGET)
}

/// [`version_of`] with an explicit budget.
///
/// The budget is a parameter so the cwd-plumbing TESTS can use a generous one.
/// They assert (rather than skip) that the probe produced output — the right
/// call, since a silently-returning test prints the same `ok` as a real pass —
/// but with the capture budget baked in, that assertion also fires whenever the
/// machine is merely too busy to start a shell inside 3s. Observed on a box
/// running ~20 concurrent cargo builds: two consecutive runs of the same binary
/// failed on *different* tests of the pair, and passed on the run before.
/// A test budget of [`TEST_PROBE_BUDGET`] keeps "the probe is broken" failing
/// while letting "the box is slow" pass, so the assertion still means what it
/// claims to.
fn version_of_within(
    cmd: &str,
    args: &[&str],
    cwd: Option<&Path>,
    budget: Duration,
) -> Option<String> {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::Instant;

    let started = Instant::now();
    let mut command = crate::process_helpers::no_window(cmd);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    let mut child = command.spawn().ok()?;

    let deadline = started + budget;
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

/// Re-run the EXACT probe [`collect_versions`] uses, with an explicit budget.
///
/// The `versions` apply (P2b slice 3) verifies its own work by re-probing after
/// driving a version manager. That verification is only worth anything if it
/// asks the same question the capture asks — same command, same args, same cwd
/// resolution — so it goes through this one function rather than a lookalike.
/// The budget is a parameter because an apply can afford to wait longer than a
/// 15-minute capture loop can.
pub(crate) fn probe_tool_version(
    cmd: &str,
    cwd: Option<&Path>,
    budget: Duration,
) -> Option<String> {
    version_of_within(cmd, &["--version"], cwd, budget)
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

// ============================================================================
// Workspace-root bridge (lib ↔ binary)
// ============================================================================
//
// [`collect_versions`] reaches into a SIBLING repo checkout (`qontinui-web`)
// for its python key, and into this repo's own checkout for the node keys, so
// it needs the answer to "where do the Qontinui repo checkouts live on this
// box?".
//
// The runner's ONE door to that question is `crate::workspace_paths` (plan
// `2026-08-04-remove-hardcoded-machine-paths-from-product-code`, slice 1) — but
// that module lives in the BINARY crate and `env_agent` lives in the LIB crate,
// so this module cannot call it. Same crate boundary the PG pool already
// crosses; same bridge shape, deliberately (see `super::publish_pg_pool`): the
// binary publishes the one door here at boot.
//
// What is published is the DOOR ITSELF — a function pointer — not the answer it
// gave at boot. `workspace_paths`'s module header records that a process-global
// memo of the root would be **wrong**, not merely unnecessary: `paths.workspace_root`
// is operator-editable and fleet policy forbids restarting the runner to pick up
// a correction, so a cached value would stay frozen for the process lifetime
// while every other consumer self-corrected on its next cycle. Publishing the
// function keeps this collector on exactly the same resolution the rest of the
// crate sees, re-read per capture. It also removes the second failure a value
// bridge had: with a value, a boot on which nothing resolved published NOTHING,
// permanently, so an operator who then set the setting would never see the
// sibling-tree keys appear.
//
// Until published, resolution falls back to the SHARED resolver in
// `qontinui_types::paths` that the one door itself wraps, minus the single rung
// the lib crate provably cannot read (the runner's `paths.workspace_root`
// setting, which lives behind the bin-crate settings facade). That covers the
// standalone `qontinui_profile env capture` CLI, which has no settings store of
// its own — exactly the reason `publish_pg_pool_from_url` exists next door. An
// unresolved root simply omits the repo-tree keys, which is this section's
// stated contract: missing inputs → key absent, never an error.

/// This repo's checkout directory name. Mirrors
/// `workspace_paths::RUNNER_REPO_DIR`; used both for the fallback anchor walk
/// (whose predicate looks for `<candidate>/qontinui-runner/.git`) and for the
/// `node_*` keys' `<root>/qontinui-runner/package.json` anchor.
const RUNNER_REPO_DIR: &str = "qontinui-runner";

/// The sibling checkout the python constraint key is read from.
const WEB_REPO_DIR: &str = "qontinui-web";

/// The bin crate's workspace-root door, published as a FUNCTION so it is
/// re-evaluated on every capture. See the section header for why a published
/// value would be wrong.
///
/// It hands over the **whole** [`WorkspaceRoot`], not just the path, because
/// two sections now read it and they need different parts of the same answer:
/// [`collect_versions`] wants only the path, while [`collect_repos`] must also
/// publish WHICH rung resolved it (`repos_scope_kind`) so a peer can tell
/// whether two boxes' repo readings are comparable at all. Resolving twice
/// would let those two answers disagree — and the drift oracle would then be
/// comparing a reading taken under one root against one taken under another,
/// silently, which is precisely the class of bug `probe_scope_kind` exists to
/// prevent on the `versions` side.
pub type WorkspaceRootFn = fn() -> qontinui_types::paths::WorkspaceRoot;

static WORKSPACE_ROOT_DOOR: std::sync::OnceLock<WorkspaceRootFn> = std::sync::OnceLock::new();

/// Publish the runner's one workspace-root door
/// (`crate::workspace_paths::workspace_root`) so [`collect_versions`] can read
/// the repo checkouts. Called unconditionally by the binary at boot, next to
/// [`super::publish_pg_pool`]. Idempotent — a second call is ignored.
///
/// Takes the door, not its answer: the door re-reads the operator-editable
/// `paths.workspace_root` setting each call, so a correction made while the
/// runner is up is picked up on the next capture rather than at the next
/// restart — which fleet policy forbids anyway. Publishing unconditionally also
/// means a boot on which nothing resolves does not permanently silence the
/// repo-tree keys.
pub fn publish_workspace_root(door: WorkspaceRootFn) {
    let _ = WORKSPACE_ROOT_DOOR.set(door);
}

/// Where the Qontinui repo checkouts live — the published door's answer when
/// the binary supplied one, else the shared resolver's answer with the settings
/// rung unavailable. See the section header for why there are two arms rather
/// than one.
///
/// Returns the full [`WorkspaceRoot`] so a caller can read the resolution's
/// KIND as well as its path; [`collect_versions`] discards the kind with
/// `.into_root()`, [`collect_repos`] publishes it.
fn workspace_root() -> qontinui_types::paths::WorkspaceRoot {
    if let Some(&door) = WORKSPACE_ROOT_DOOR.get() {
        return door();
    }
    // Standalone `qontinui_profile env capture`: no bin crate booted, so no
    // door. This arm provably CANNOT read `paths.workspace_root` (the setting
    // lives behind the bin-crate settings facade), which is why it passes
    // `configured: None` rather than pretending to have looked.
    let exe = std::env::current_exe().ok();
    qontinui_types::paths::qontinui_workspace_root(
        None,
        exe.as_deref()
            .map(|e| qontinui_types::paths::WorkspaceAnchor::new(e, RUNNER_REPO_DIR)),
    )
}

/// The two versions that are properties of THIS BINARY, baked at compile time.
///
/// Both used to be read at RUNTIME out of
/// `env!("CARGO_MANIFEST_DIR")/Cargo.toml` — the manifest of the source tree the
/// binary was BUILT from, on the BUILD host, at a path that need not exist (or,
/// worse, may exist with *different* contents) on the box actually running the
/// binary. That is the hardcoded machine path wearing a macro's clothes, exactly
/// as this module's own `resolve_probe_scope` doc already says; it was invisible
/// to any grep for a `D:` literal. Plan
/// `2026-08-04-remove-hardcoded-machine-paths-from-product-code`, slice 5
/// Phase 8.
///
/// - `runner_crate_version` ← `CARGO_PKG_VERSION`. A genuinely correct bake: it
///   is a property of the binary, not of the box.
/// - `tauri` ← [`tauri::VERSION`], the version of the tauri crate this binary is
///   actually LINKED against. Strictly better than the old value, which was the
///   *declared range* (`"2.5"`) parsed out of the build host's manifest — a
///   constraint, not a fact about the running binary.
///
/// Neither key can go absent, so both are `put` unconditionally.
fn put_binary_versions(section: &mut Section) {
    put(section, "runner_crate_version", env!("CARGO_PKG_VERSION"));
    put(section, "tauri", tauri::VERSION);
}

/// The keys read out of repo checkouts under `workspace_root`: the `node_*` set
/// from this repo's own `package.json` ([`workspace_package_json`]) and
/// `python_constraint` from the sibling `qontinui-web` backend
/// ([`web_backend_pyproject`]).
///
/// The walk origin used to be `env!("CARGO_MANIFEST_DIR")` — see
/// [`put_binary_versions`] for why that was wrong. It now comes from the
/// workspace-root resolution, injected here so the lookup rules are testable
/// against a synthetic tree with no environment read (plan
/// `2026-08-04-remove-hardcoded-machine-paths-from-product-code`, slice 5
/// Phase 8).
fn put_sibling_tree_versions(section: &mut Section, workspace_root: &Path) {
    if let Some(pkg) = workspace_package_json(workspace_root) {
        if let Ok(contents) = std::fs::read_to_string(&pkg) {
            if let Ok(json) = serde_json::from_str::<Value>(&contents) {
                if let Some(name) = json.get("name").and_then(|v| v.as_str()) {
                    put(section, "node_package_name", name.to_string());
                }
                if let Some(ver) = json.get("version").and_then(|v| v.as_str()) {
                    put(section, "node_package_version", ver.to_string());
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
                        put(section, &format!("node_dep_{dep}"), range.to_string());
                    }
                }
            }
        }
    }

    if let Some(pyproject) = web_backend_pyproject(workspace_root) {
        if let Ok(contents) = std::fs::read_to_string(&pyproject) {
            // `[tool.poetry.dependencies]` carries `python = "^3.12"`.
            if let Some(py) = cargo_toml_value(&contents, "tool.poetry.dependencies", "python") {
                put(section, "python_constraint", py);
            }
        }
    }
}

/// Collect the `versions` section: this binary's own crate + tauri versions,
/// node deps from this repo's own `package.json` under the workspace root,
/// python from the web-backend pyproject.toml, plus bounded `--version` shells
/// where the tools resolve. Missing inputs → key absent, never an error.
pub fn collect_versions() -> Section {
    let mut section = Section::new();

    put_binary_versions(&mut section);

    // An unresolved workspace root omits the sibling-tree keys rather than
    // guessing at a layout — see the workspace-root bridge above. This section
    // reads only the PATH; the resolution's kind is `repos`' provenance, not
    // this section's (which carries `probe_scope_kind` for the toolchain scope
    // instead — a different quantity, see `collect_repos`).
    if let Some(root) = workspace_root().into_root() {
        put_sibling_tree_versions(&mut section, &root);
    }

    // ---- bounded `--version` shells (optional, 3s budget each) ----
    //
    // These three are the ONLY keys in this section that describe the box rather
    // than the source tree the binary was built from, and (post web #808 /
    // runner #818 derived-key filtering) the only ones a P2b apply can move. All
    // three run in the DECLARED scope root so the answer does not depend on how
    // the runner process was launched.
    let scope = probe_scope();
    // Capture PROVENANCE. The three keys below are only comparable across boxes
    // that measured the same KIND of scope; without this the drift oracle
    // silently compares a project tree's toolchain against another box's
    // default one and calls the difference drift. The runner's `versions` apply
    // refuses on a mismatch rather than installing a version that was observed
    // somewhere else. The PATH is deliberately NOT emitted: it differs between
    // any two boxes while meaning the same thing, and it is operator-local.
    put(&mut section, "probe_scope_kind", scope.kind.wire());
    let scope = scope.root;
    let scope_ref = scope.as_deref();
    if let Some(v) = version_of("rustc", &["--version"], scope_ref) {
        put(&mut section, "rustc", v);
    }
    if let Some(v) = version_of("node", &["--version"], scope_ref) {
        put(&mut section, "node", v);
    }
    if let Some(v) = version_of("python", &["--version"], scope_ref) {
        put(&mut section, "python", v);
    }

    section
}

// ============================================================================
// repos — which repositories this box has cloned
// ============================================================================
//
// The section answers "which repositories does this environment require", so a
// developer is TOLD which ones they are missing (with the clone URL) instead of
// discovering it by hand. Plan `2026-08-06-devenv-repos-section`, P1.
//
// ## The anchor is the workspace root, NOT the probe scope
//
// `probe_scope_root` resolves where the toolchain `--version` shells run, and
// its fallback rung is the HOME directory — deliberately, because "the box's
// default toolchain" is what a shim-based manager answers outside any project
// tree. On a box that declares no `scope_root` that is `~`, which holds no
// checkouts at all: enumerating there would emit an empty section, `add_section`
// would drop it, and the twin would be told nothing about repositories on the
// very machine that needed telling. `workspace_root` is the resolution that
// answers "where do the repo checkouts live", and it is the one used here.
//
// ## Depth 1, and not as an approximation
//
// Every repo that matters is a depth-1 child of the workspace root, because the
// repos in this project depend on each other by RELATIVE path
// (`src-tauri/Cargo.toml`'s `../../qontinui-schemas/rust`,
// `generate_types.sh`'s `$PROJECT_ROOT/../../qontinui-schemas`). A checkout
// somewhere else cannot satisfy those, so reporting it as present would be
// wrong, not generous. Recursing would also descend into `_wt/`,
// `agent-worktrees/`, `node_modules/` and `target/` for nothing.

/// How many depth-1 entries to examine before giving up.
///
/// Not a performance knob — a runaway guard. The operator box carries 624
/// depth-1 entries under its workspace root, so the real cost is bounded and
/// small; this exists so a root that accidentally resolves somewhere enormous
/// (a home directory, a drive root) cannot stall a capture that runs on a
/// 15-minute loop. Exceeding it WARNs rather than truncating silently.
const REPO_SCAN_ENTRY_BUDGET: usize = 4096;

/// The provenance key: WHICH KIND of workspace-root resolution these repo
/// observations were taken under.
pub(crate) const REPOS_SCOPE_KEY: &str = "repos_scope_kind";

/// The workspace root, for the `repos` APPLY.
///
/// The same door the capture reads, exposed deliberately rather than letting
/// `apply_repos` resolve its own: the apply must clone into the root the capture
/// enumerated, or it would report against one tree and write into another — and
/// the drift would never clear no matter how many times it ran.
pub(crate) fn workspace_root_for_apply() -> qontinui_types::paths::WorkspaceRoot {
    workspace_root()
}

/// Read `origin` from a checkout at `dir`, returning its canonical secret-free
/// URL. `None` when the directory is not a canonical checkout, is a linked
/// worktree, has no `origin`, or has an unparseable one.
///
/// Uses `git2` (already a dependency, and already how this crate opens
/// repositories in `agent_worktree` and `trigger_system`) rather than shelling
/// `git`: capture runs on a 15-minute loop over hundreds of directories, and a
/// process spawn per directory is the one cost that would make this collector
/// too expensive to run.
fn origin_of_checkout(dir: &Path) -> Result<Option<String>, String> {
    let repo = git2::Repository::open(dir).map_err(|e| e.message().to_string())?;

    // A linked worktree is NOT an independent clone: it shares its canonical
    // checkout's object store and dies with it. Reporting one as "this repo is
    // present" would tell a developer they have something they do not. On the
    // operator box these outnumber canonical checkouts 239 to 37, so this is
    // the common case rather than an edge one.
    if repo.is_worktree() {
        return Ok(None);
    }

    let remote = match repo.find_remote("origin") {
        Ok(r) => r,
        // No `origin` is a legitimate state (a local-only scratch repo), not a
        // failure — distinguish it from an unparseable one so only the latter
        // WARNs.
        Err(_) => return Ok(None),
    };
    let Some(url) = remote.url() else {
        return Err("origin has no UTF-8 url".to_string());
    };
    match sanitize_git_remote(url) {
        Some(canonical) => Ok(Some(canonical)),
        // Deliberately an Err, not a silent None: an unparseable remote is the
        // exact shape of failure that cost the `services` section its
        // `database_url` key — see `sanitize_database_url`'s header. The caller
        // WARNs; it never drops it quietly.
        None => Err(format!(
            "origin url is not a recognisable git remote ({} chars)",
            url.len()
        )),
    }
}

/// Pure core of [`collect_repos`]: enumerate depth-1 checkouts under `root` and
/// emit one `repo_<owner>_<name>` → canonical-URL key each.
///
/// `owner_allowlist` filters by the remote's owner segment. An EMPTY allowlist
/// means "no filter" — the honest reading of an unconfigured environment, since
/// an empty allowlist that suppressed everything would make an unconfigured box
/// publish silence, and silence is what this whole section exists to end.
///
/// Injected inputs and no environment reads, so it is testable against a
/// synthetic tree — the same split `resolve_probe_scope` uses.
fn collect_repos_under(root: &Path, owner_allowlist: &[String]) -> Section {
    let mut section = Section::new();

    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) => {
            warn!(
                "env_agent: repos scan cannot read {} ({e}) — section will carry provenance only",
                root.display()
            );
            return section;
        }
    };

    let mut examined = 0usize;
    let mut budget_exceeded = false;
    for entry in entries.flatten() {
        examined += 1;
        if examined > REPO_SCAN_ENTRY_BUDGET {
            budget_exceeded = true;
            break;
        }
        let path = entry.path();
        // Cheap prefilter: most depth-1 entries under a workspace root are not
        // repositories at all (348 of 624 on the operator box), and this avoids
        // handing every one of them to libgit2.
        if !path.join(".git").exists() {
            continue;
        }
        match origin_of_checkout(&path) {
            Ok(Some(canonical)) => {
                let Some(key) = repo_key(&canonical) else {
                    warn!(
                        "env_agent: cannot derive a repo key from {canonical} — skipping this entry"
                    );
                    continue;
                };
                if !owner_allowlist.is_empty() && !owner_matches(&canonical, owner_allowlist) {
                    continue;
                }
                // Two entries can legitimately resolve to the same repository
                // (a checkout plus a differently-spelled second clone); they
                // normalize to the same key AND the same value, so last-write
                // is not a conflict.
                put(&mut section, &key, canonical);
            }
            Ok(None) => {}
            Err(reason) => warn!(
                "env_agent: skipping {} — {reason}",
                path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string())
            ),
        }
    }

    if budget_exceeded {
        warn!(
            "env_agent: repos scan stopped after {REPO_SCAN_ENTRY_BUDGET} entries under {} — \
             the section is INCOMPLETE; is the workspace root pointing somewhere unexpected?",
            root.display()
        );
    }

    section
}

/// Whether a canonical URL's owner segment is in the allowlist (case-insensitive
/// — forges treat owner names case-insensitively, and two boxes spelling one
/// owner differently must not read as different repositories).
fn owner_matches(canonical_url: &str, allowlist: &[String]) -> bool {
    let Some(path) = canonical_url
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once('/'))
        .map(|(_, p)| p)
    else {
        return false;
    };
    let mut segments = path.rsplit('/');
    let _name = segments.next();
    let Some(owner) = segments.next() else {
        return false;
    };
    allowlist.iter().any(|a| a.eq_ignore_ascii_case(owner))
}

/// Collect the `repos` section: which repositories are cloned on this box, keyed
/// by identity and valued by the canonical clone URL, plus the provenance of the
/// root they were enumerated under.
///
/// Returns `None` only when the workspace root does not resolve — there is then
/// no observation to report, and publishing a bare provenance key saying
/// "unresolved" would assert a reading that was never taken. Everything else
/// returns `Some`, INCLUDING a resolved root under which nothing matched: that
/// is a real, comparable observation ("this box has none of the environment's
/// repos") and the section must carry it rather than vanish. `add_section`
/// drops empty sections, so the provenance key is also what keeps a
/// legitimately-empty result visible instead of silent.
pub fn collect_repos() -> Option<Section> {
    let resolved = workspace_root();
    if let Some(rejected) = resolved.rejected {
        warn!(
            "env_agent: repos capture — {} — continuing with the next resolution rung",
            rejected.describe()
        );
    }
    let kind = resolved.kind;
    let root = resolved.into_root()?;

    let allowlist = super::config::EnvAgentConfig::load()
        .map(|c| c.repo_owner_allowlist)
        .unwrap_or_default();

    let mut section = collect_repos_under(&root, &allowlist);
    // Written LAST and unconditionally: it is what makes an empty result a
    // stated observation rather than a dropped section.
    put(&mut section, REPOS_SCOPE_KEY, kind.wire());
    Some(section)
}

/// The `package.json` this capture reports: **this repo's own**, at
/// `<workspace-root>/qontinui-runner/package.json`.
///
/// It resolves the runner's node package because that is what the `node_*` keys
/// have always described. The old form's doc claimed it preferred the
/// `qontinui-web` frontend manifest, but that branch was **unreachable**: the
/// walk started at `env!("CARGO_MANIFEST_DIR")` (`<root>/qontinui-runner/src-tauri`)
/// and probed `dir/package.json` FIRST on each iteration, so it hit
/// `<root>/qontinui-runner/package.json` on the second step and returned. Reading
/// the frontend manifest instead would silently retarget five keys
/// (`node_package_name`, `node_package_version`, `node_dep_*`) at a sibling repo,
/// leaving `node_package_name` describing a different package from the
/// `runner_crate_version` sitting beside it in the same section. Both must
/// describe THIS binary.
///
/// Anchored AT the resolved workspace root with **no upward walk**. The old form
/// walked up to 6 ancestors only because its origin was deep inside the
/// checkout; starting at the root, a climb can only leave the workspace and
/// report a stranger's `package.json` as this machine's — the same loose-anchor
/// class the plan's resolver predicate exists to kill (plan
/// `2026-08-04-remove-hardcoded-machine-paths-from-product-code`, slice 5
/// Phase 8).
fn workspace_package_json(workspace_root: &Path) -> Option<std::path::PathBuf> {
    let own = workspace_root.join(RUNNER_REPO_DIR).join("package.json");
    own.is_file().then_some(own)
}

/// The web-backend `pyproject.toml` under the resolved workspace root. `None`
/// when not present (e.g. a production machine without the source checkout) —
/// the key is then simply absent. Anchored, not walked; see
/// [`workspace_package_json`].
fn web_backend_pyproject(workspace_root: &Path) -> Option<std::path::PathBuf> {
    let candidate = workspace_root
        .join(WEB_REPO_DIR)
        .join("backend")
        .join("pyproject.toml");
    candidate.is_file().then_some(candidate)
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

    use crate::test_env::env_lock;

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

    /// The regression this exists for: the runner's own `legacy_env_fallback`
    /// default is a libpq `key=value` DSN, and `sanitize_url` returns None for
    /// it — so `database_url` was silently dropped from every capture made on a
    /// box using the default, and the section read as in-sync.
    #[test]
    fn sanitize_database_url_accepts_the_libpq_keyvalue_default() {
        let legacy = "host=localhost port=5432 user=qontinui_user \
                      password=qontinui_dev_password dbname=qontinui_db";
        assert!(
            sanitize_url(legacy).is_none(),
            "precondition: plain URL parsing cannot read this form"
        );
        let s = sanitize_database_url(legacy).expect("key=value DSN must be captured");
        assert_eq!(s, "postgres://localhost:5432");
        assert!(!s.contains("qontinui_dev_password"), "password leaked: {s}");
        assert!(!s.contains("qontinui_user"), "username leaked: {s}");
        assert!(!s.contains("qontinui_db"), "dbname leaked: {s}");
    }

    /// Both spellings of the same server must converge on ONE value, or two
    /// boxes that merely write the DSN differently would read as permanent
    /// drift that no apply could ever clear.
    #[test]
    fn sanitize_database_url_normalizes_both_forms_to_the_same_value() {
        let url_form = sanitize_database_url("postgres://u:p@localhost:5433/qontinui_db").unwrap();
        let kv_form =
            sanitize_database_url("host=localhost port=5433 user=u password=p dbname=qontinui_db")
                .unwrap();
        assert_eq!(url_form, kv_form);
        assert_eq!(url_form, "postgres://localhost:5433");
    }

    /// libpq defaults the port to 5432 when omitted. Rendering it explicitly
    /// keeps "port omitted" and "port written out" from reading as drift.
    #[test]
    fn sanitize_database_url_defaults_the_omitted_port() {
        let s = sanitize_database_url("host=db.internal user=svc").unwrap();
        assert_eq!(s, "postgres://db.internal:5432");
    }

    /// A Unix-socket DSN has no cross-box host:port topology, and the path is
    /// operator-local — report nothing rather than something misleading.
    #[test]
    fn sanitize_database_url_omits_unix_socket_dsns() {
        assert!(sanitize_database_url("host=/var/run/postgresql user=svc").is_none());
    }

    #[test]
    fn sanitize_database_url_still_rejects_garbage() {
        assert!(sanitize_database_url("not a dsn at all ===").is_none());
    }

    // ---- repos ----------------------------------------------------------

    /// The three spellings of ONE repository must converge on ONE value.
    /// Without this, two boxes that merely cloned differently read as permanent
    /// drift that no apply can ever clear.
    #[test]
    fn sanitize_git_remote_converges_every_spelling_of_one_repo() {
        let canonical = "https://github.com/qontinui/qontinui-runner";
        for spelling in [
            "git@github.com:qontinui/qontinui-runner.git",
            "git@github.com:qontinui/qontinui-runner",
            "https://github.com/qontinui/qontinui-runner.git",
            "https://github.com/qontinui/qontinui-runner",
            "ssh://git@github.com/qontinui/qontinui-runner.git",
            "git://github.com/qontinui/qontinui-runner.git",
            "  https://github.com/qontinui/qontinui-runner/  ",
        ] {
            assert_eq!(
                sanitize_git_remote(spelling).as_deref(),
                Some(canonical),
                "{spelling} must normalize to {canonical}"
            );
        }
    }

    /// A token-bearing remote must never reach the envelope — the same
    /// structural userinfo strip `sanitize_url` performs.
    #[test]
    fn sanitize_git_remote_strips_a_token() {
        let got =
            sanitize_git_remote("https://x-access-token:ghp_secret@github.com/qontinui/x.git");
        assert_eq!(got.as_deref(), Some("https://github.com/qontinui/x"));
        assert!(!got.unwrap().contains("ghp_secret"));
    }

    /// A non-GitHub forge is preserved verbatim (host, port, and every path
    /// segment) — truncating a nested group would collapse two different
    /// repositories onto one key.
    #[test]
    fn sanitize_git_remote_preserves_other_forges_and_nested_groups() {
        assert_eq!(
            sanitize_git_remote("git@gitlab.example.com:team/sub/name.git").as_deref(),
            Some("https://gitlab.example.com/team/sub/name")
        );
        assert_eq!(
            sanitize_git_remote("ssh://git@forge.example.com:2222/owner/name.git").as_deref(),
            Some("https://forge.example.com:2222/owner/name")
        );
    }

    /// Unparseable input returns `None` so the CALLER can WARN. Silently
    /// dropping it is the failure `sanitize_database_url`'s header documents,
    /// where a quiet `None` cost the `services` section its most important key.
    #[test]
    fn sanitize_git_remote_rejects_what_it_cannot_parse() {
        for bad in [
            "",
            "   ",
            "not a remote",
            "https://github.com",
            "github.com:",
        ] {
            assert!(
                sanitize_git_remote(bad).is_none(),
                "{bad:?} must not produce a value"
            );
        }
    }

    #[test]
    fn repo_key_is_owner_and_name_with_unsafe_chars_folded() {
        assert_eq!(
            repo_key("https://github.com/qontinui/qontinui-runner").as_deref(),
            Some("repo_qontinui_qontinui-runner")
        );
        // Deeper paths still key on the trailing owner/name pair; the full URL
        // stays the VALUE, so nothing is lost.
        assert_eq!(
            repo_key("https://gitlab.example.com/team/sub/name").as_deref(),
            Some("repo_sub_name")
        );
    }

    #[test]
    fn owner_allowlist_matches_case_insensitively() {
        let allow = vec!["QonTinui".to_string()];
        assert!(owner_matches(
            "https://github.com/qontinui/qontinui-stack",
            &allow
        ));
        assert!(!owner_matches("https://github.com/someone-else/x", &allow));
    }

    /// A synthetic workspace root: one canonical checkout, one LINKED WORKTREE
    /// of it, and one directory that is not a repository at all.
    fn repos_fixture(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "qontinui_repos_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        // A real checkout with an `origin`.
        let repo = root.join("qontinui-runner");
        let r = git2::Repository::init(&repo).unwrap();
        r.remote("origin", "git@github.com:qontinui/qontinui-runner.git")
            .unwrap();

        // A LINKED WORKTREE of it — created through git's own worktree API, not
        // by hand-writing a `.git` file. On the operator box these outnumber
        // canonical checkouts 242 to 37, so this is the common case.
        //
        // The hand-written form is worse than useless here: pointing the marker
        // at the MAIN `.git` makes `Repository::open` return the main repo, so
        // `is_worktree()` is false and the entry is reported — with the same key
        // and value as the real checkout, which dedupes to one key and makes the
        // assertion below pass for entirely the wrong reason. A worktree needs a
        // commit to be created from, hence the empty initial commit.
        {
            let sig = git2::Signature::now("t", "t@t").unwrap();
            let tree = r.find_tree(r.index().unwrap().write_tree().unwrap()).unwrap();
            r.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }
        let wt_path = root.join("qontinui-runner-wt-something");
        let mut opts = git2::WorktreeAddOptions::new();
        r.worktree("wt-something", &wt_path, Some(&mut opts))
            .unwrap();
        // Guard the guard: if this ever stops being a worktree, the exclusion
        // test below is vacuous and must fail loudly rather than quietly pass.
        assert!(
            git2::Repository::open(&wt_path).unwrap().is_worktree(),
            "the fixture must produce a genuine linked worktree"
        );

        // Not a repository at all — the majority case under a real root.
        std::fs::create_dir_all(root.join("_cargo-targets")).unwrap();

        root
    }

    /// The whole point of the section: a canonical checkout is reported, and a
    /// linked worktree of it is NOT. A worktree shares its checkout's object
    /// store and dies with it, so counting one as "this repo is present" would
    /// tell a developer they have something they do not.
    #[test]
    fn collect_repos_reports_checkouts_and_excludes_linked_worktrees() {
        let root = repos_fixture("basic");
        let section = collect_repos_under(&root, &[]);

        assert_eq!(
            section
                .get("repo_qontinui_qontinui-runner")
                .and_then(Value::as_str),
            Some("https://github.com/qontinui/qontinui-runner"),
            "the canonical checkout must be reported, normalized"
        );
        assert_eq!(
            section.len(),
            1,
            "the linked worktree and the non-repo must contribute nothing: {section:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An EMPTY allowlist means no filter. An empty allowlist that suppressed
    /// everything would make an unconfigured box publish silence — and an absent
    /// fact being indistinguishable from "in sync" is what this section exists
    /// to end.
    #[test]
    fn an_empty_allowlist_filters_nothing() {
        let root = repos_fixture("empty_allow");
        assert_eq!(collect_repos_under(&root, &[]).len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A non-empty allowlist drops non-matching owners — the filter that keeps
    /// a developer's personal checkouts from breaking `in_sync` server-side.
    #[test]
    fn a_non_matching_allowlist_drops_the_repo() {
        let root = repos_fixture("allow_filters");
        assert!(collect_repos_under(&root, &["someone-else".to_string()]).is_empty());
        assert_eq!(
            collect_repos_under(&root, &["qontinui".to_string()]).len(),
            1
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A checkout with no `origin` is a legitimate local-only repo, not an
    /// error: it contributes no key and does not abort the scan.
    #[test]
    fn a_checkout_without_an_origin_is_skipped_not_fatal() {
        let root = repos_fixture("no_origin");
        let orphan = root.join("local-only");
        git2::Repository::init(&orphan).unwrap();
        let section = collect_repos_under(&root, &[]);
        assert_eq!(
            section.len(),
            1,
            "only the origin-bearing repo: {section:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A root with no repositories under it yields an EMPTY section rather than
    /// an error — `collect_repos` then adds the provenance key, which is what
    /// stops `add_section` dropping a legitimately-empty observation.
    #[test]
    fn an_empty_root_yields_an_empty_section_not_a_failure() {
        let root = std::env::temp_dir().join(format!("qontinui_repos_bare_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        assert!(collect_repos_under(&root, &[]).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cargo_toml_value_reads_package_version() {
        let toml = "[package]\nname = \"x\"\nversion = \"1.2.3\"\n[dependencies]\nfoo = \"9\"\n";
        assert_eq!(
            cargo_toml_value(toml, "package", "version").as_deref(),
            Some("1.2.3")
        );
    }

    // -----------------------------------------------------------------
    // `versions` — the build-host manifest read is gone (plan
    // `2026-08-04-remove-hardcoded-machine-paths-from-product-code`,
    // slice 5 Phase 8).
    // -----------------------------------------------------------------

    /// A synthetic workspace root holding this repo's checkout and a
    /// `qontinui-web` sibling tree.
    ///
    /// Never the real machine layout, and pid/counter-scoped because this fleet
    /// runs `cargo test` from several worktrees at once. Cleanup is a `Drop`
    /// guard so a failing assertion does not leak the tree.
    struct WorkspaceFixture {
        root: std::path::PathBuf,
    }

    impl Drop for WorkspaceFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn workspace_fixture() -> WorkspaceFixture {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "qontinui_env_agent_versions_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        WorkspaceFixture { root }
    }

    impl WorkspaceFixture {
        /// This repo's own checkout, carrying the `package.json` the `node_*`
        /// keys describe.
        fn with_runner_tree(self) -> Self {
            let runner = self.root.join(RUNNER_REPO_DIR);
            std::fs::create_dir_all(&runner).unwrap();
            std::fs::write(
                runner.join("package.json"),
                r#"{"name":"synthetic-runner","version":"9.9.9",
                    "dependencies":{"next":"^15.0.0"},
                    "devDependencies":{"typescript":"^5.4.0"}}"#,
            )
            .unwrap();
            self
        }

        /// The `qontinui-web` sibling, carrying the backend pyproject the
        /// `python_constraint` key is read from — and a frontend `package.json`
        /// that must NOT be picked up for the `node_*` keys.
        fn with_web_tree(self) -> Self {
            let frontend = self.root.join(WEB_REPO_DIR).join("frontend");
            let backend = self.root.join(WEB_REPO_DIR).join("backend");
            std::fs::create_dir_all(&frontend).unwrap();
            std::fs::create_dir_all(&backend).unwrap();
            std::fs::write(
                frontend.join("package.json"),
                r#"{"name":"synthetic-frontend","version":"0.0.1",
                    "dependencies":{"next":"^99.0.0"}}"#,
            )
            .unwrap();
            std::fs::write(
                backend.join("pyproject.toml"),
                "[tool.poetry.dependencies]\npython = \"^3.12\"\n",
            )
            .unwrap();
            self
        }
    }

    /// `runner_crate_version` and `tauri` are properties of the BINARY, baked at
    /// compile time. They used to be parsed at runtime out of the build host's
    /// `Cargo.toml` — a path that need not exist on the box running the binary.
    /// Neither key may go absent, and neither may depend on any file.
    #[test]
    fn binary_versions_are_baked_and_never_read_from_a_manifest_on_disk() {
        let mut section = Section::new();
        put_binary_versions(&mut section);

        let crate_version = section
            .get("runner_crate_version")
            .and_then(|v| v.as_str())
            .expect("runner_crate_version must always be present");
        assert!(
            crate_version.contains('.'),
            "expected a semver-ish crate version, got {crate_version:?}"
        );

        let tauri_version = section
            .get("tauri")
            .and_then(|v| v.as_str())
            .expect("tauri must always be present");
        assert!(
            tauri_version.contains('.'),
            "expected the LINKED tauri version, got {tauri_version:?}"
        );
        // The old value was the declared RANGE out of the build host's manifest
        // (`"2.5"` — two components). The linked crate always reports a concrete
        // major.minor.patch.
        assert!(
            tauri_version.split('.').count() >= 3,
            "must be the linked crate's version, not a declared range: {tauri_version:?}"
        );
    }

    /// The repo-tree keys come from the INJECTED workspace root, never from
    /// the tree the binary was built in.
    #[test]
    fn sibling_tree_versions_read_the_injected_workspace_root() {
        let f = workspace_fixture().with_runner_tree().with_web_tree();
        let mut section = Section::new();
        put_sibling_tree_versions(&mut section, &f.root);

        assert_eq!(
            section.get("node_package_name").and_then(|v| v.as_str()),
            Some("synthetic-runner")
        );
        assert_eq!(
            section.get("node_package_version").and_then(|v| v.as_str()),
            Some("9.9.9")
        );
        assert_eq!(
            section.get("node_dep_next").and_then(|v| v.as_str()),
            Some("^15.0.0")
        );
        assert_eq!(
            section.get("node_dep_typescript").and_then(|v| v.as_str()),
            Some("^5.4.0"),
            "devDependencies are a fallback source for the high-signal deps"
        );
        assert_eq!(
            section.get("python_constraint").and_then(|v| v.as_str()),
            Some("^3.12")
        );
    }

    /// A machine with no source checkout under the root omits every
    /// sibling-tree key — the section's contract is "missing inputs → key
    /// absent, never an error".
    ///
    /// This also pins the no-upward-walk property: the fixture root lives inside
    /// the system temp dir, so a walk would happily report some ancestor's
    /// `package.json` as this machine's.
    #[test]
    fn sibling_tree_versions_omit_every_key_when_the_root_has_no_checkouts() {
        let f = workspace_fixture();
        let mut section = Section::new();
        put_sibling_tree_versions(&mut section, &f.root);
        assert!(
            section.is_empty(),
            "an empty workspace root must contribute nothing, got {section:?}"
        );
    }

    /// The `node_*` keys describe THIS binary's node package, so the manifest is
    /// this repo's own — never the `qontinui-web` frontend's (whose values would
    /// leave `node_package_name` naming a different package from the
    /// `runner_crate_version` beside it) and never a `package.json` sitting at
    /// the workspace root. Neither probe ever leaves the root.
    #[test]
    fn workspace_package_json_is_this_repos_own_and_never_walks_up() {
        let f = workspace_fixture().with_runner_tree().with_web_tree();
        std::fs::write(f.root.join("package.json"), r#"{"name":"root-level"}"#).unwrap();
        assert_eq!(
            workspace_package_json(&f.root),
            Some(f.root.join(RUNNER_REPO_DIR).join("package.json"))
        );

        // A workspace with the web sibling but NO runner checkout yields no
        // manifest at all — the frontend's is never a substitute.
        let web_only = workspace_fixture().with_web_tree();
        assert_eq!(workspace_package_json(&web_only.root), None);

        let bare = workspace_fixture();
        assert_eq!(
            workspace_package_json(&bare.root),
            None,
            "no manifest under the root means no key — never an ancestor's"
        );
        assert_eq!(web_backend_pyproject(&bare.root), None);
    }

    /// Secret-safety: a secret-bearing env var contributes only its NAME, never
    /// the value, to the env_contract section.
    #[test]
    fn secret_safety_env_contract_emits_name_not_value() {
        let _env_lock = env_lock();
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

    // ---- probe scope root (declared capture scope) ----

    /// Budget for the cwd-plumbing probes below.
    ///
    /// Generous ON PURPOSE. These tests assert the probe produced output rather
    /// than skipping when it did not, which is correct — but that assertion is
    /// only meaningful if a failure means "the probe is broken", not "the box
    /// was too busy to start a shell in three seconds". The capture budget is a
    /// production latency choice; borrowing it here made the pair flaky under
    /// parallel load. Long enough that only a genuinely missing or wedged shell
    /// trips it; short enough that a wedged one still fails the run promptly.
    const TEST_PROBE_BUDGET: Duration = Duration::from_secs(60);

    /// Spawn a shell that prints its own cwd, through `version_of`'s plumbing.
    /// `version_of` returns the first stdout line, so `pwd` / `cd` round-trips.
    fn cwd_probe(cwd: Option<&Path>) -> Option<String> {
        #[cfg(windows)]
        let (cmd, args) = ("cmd", ["/c", "cd"]);
        #[cfg(not(windows))]
        let (cmd, args) = ("sh", ["-c", "pwd"]);
        version_of_within(cmd, &args, cwd, TEST_PROBE_BUDGET)
    }

    /// The load-bearing property: the probe runs in the directory it is GIVEN,
    /// not the one the runner process happens to be sitting in. Without this,
    /// `node`/`python`/`rustc` are a function of how the runner was launched and
    /// the drift oracle they feed compares unlike with unlike.
    #[test]
    fn version_of_runs_in_the_supplied_scope_root() {
        let dir = tempfile::tempdir().unwrap();
        // Distinctive component: Windows may report an 8.3-shortened or
        // case-differing prefix, so assert on the leaf rather than equality.
        let scoped = dir.path().join("qontinui_scope_probe_marker");
        std::fs::create_dir_all(&scoped).unwrap();

        // Deliberately NOT a silent skip on `None`. Every platform this builds on
        // ships the shell used here (`cmd` on Windows, `sh` elsewhere), so a
        // `None` means the probe plumbing is broken — and a test that quietly
        // returns instead would report the same "ok" as a real pass.
        let out = cwd_probe(Some(&scoped)).expect("cwd probe produced no output — shell missing?");
        assert!(
            out.to_lowercase().contains("qontinui_scope_probe_marker"),
            "probe did not run in the supplied scope root; reported cwd: {out}"
        );
    }

    /// Two probes with DIFFERENT scope roots must report different cwds — the
    /// direct expression of "capture is scope-determined". A probe that ignored
    /// its cwd argument would pass the test above by accident if the process
    /// already sat in the right place; this one cannot be passed that way.
    #[test]
    fn version_of_scope_root_actually_varies_the_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("scope_a");
        let b = dir.path().join("scope_b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();

        let out_a = cwd_probe(Some(&a)).expect("cwd probe (a) produced no output");
        let out_b = cwd_probe(Some(&b)).expect("cwd probe (b) produced no output");
        assert_ne!(
            out_a.to_lowercase(),
            out_b.to_lowercase(),
            "probe reported the same cwd for two different scope roots"
        );
        assert!(out_a.to_lowercase().ends_with("scope_a"), "got {out_a}");
        assert!(out_b.to_lowercase().ends_with("scope_b"), "got {out_b}");
    }

    /// A configured scope root that does not exist must FALL THROUGH to the home
    /// directory, not be returned and not abort the capture. The collectors are
    /// fail-open: a stale config entry must never silently zero the `versions`
    /// section, which is what returning a nonexistent cwd would do (every spawn
    /// would fail and every observed key would go missing).
    #[test]
    fn probe_scope_root_falls_through_when_configured_path_is_missing() {
        let _env_lock = env_lock();
        let home = tempfile::tempdir().unwrap();
        let prior_home = std::env::var("HOME").ok();
        let prior_profile = std::env::var("USERPROFILE").ok();
        std::env::set_var("HOME", home.path());
        std::env::set_var("USERPROFILE", home.path());

        // Enrolled config naming a scope root that is not there.
        let qdir = home.path().join(".qontinui");
        std::fs::create_dir_all(&qdir).unwrap();
        let missing = home.path().join("definitely_not_created");
        std::fs::write(
            qdir.join("env-agent.json"),
            serde_json::json!({
                "backend_url": "http://localhost:8000",
                "machine_id": "m",
                "environment_id": "e",
                "scope_root": missing.to_string_lossy(),
            })
            .to_string(),
        )
        .unwrap();

        let resolved = probe_scope_root();

        match prior_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prior_profile {
            Some(v) => std::env::set_var("USERPROFILE", v),
            None => std::env::remove_var("USERPROFILE"),
        }

        let resolved = resolved.expect("should fall back to home, not return None");
        assert_ne!(
            resolved, missing,
            "returned a nonexistent configured scope root"
        );
        assert!(resolved.is_dir(), "fallback must be a real directory");
    }

    /// An absolute, existing configured root is honoured verbatim and reports no
    /// rejection — the baseline the reject cases below are measured against.
    #[test]
    fn resolve_probe_scope_honours_absolute_existing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let scope = resolve_probe_scope(Some(&dir.path().to_string_lossy()));
        assert_eq!(scope.root.as_deref(), Some(dir.path()));
        assert!(scope.rejected.is_none(), "got {:?}", scope.rejected);
    }

    /// A RELATIVE configured root must be rejected, not resolved.
    ///
    /// This is the hole the absolute-path check closes: `is_dir()` on a relative
    /// path is evaluated against the runner's cwd, and handing that same relative
    /// path to `current_dir()` resolves it against the spawned child's inherited
    /// cwd — so a relative `scope_root` makes the probe launch-dependent again,
    /// which is precisely the defect the declared capture scope exists to remove.
    /// It has to be caught by SHAPE, because on the box where the operator typed
    /// it the path very often does resolve, and the capture then looks correct.
    #[test]
    fn resolve_probe_scope_rejects_a_relative_configured_root() {
        // `src` exists relative to the package root the tests run in, so the
        // rejection here is decided by SHAPE (relative), not by the directory
        // being absent — which is the distinction that matters: a relative path
        // that resolves is the dangerous case, not the harmless one.
        let scope = resolve_probe_scope(Some("src"));
        assert_eq!(
            scope.rejected,
            Some(ScopeRootRejection::Relative),
            "a relative scope root must be rejected as relative"
        );
        assert_ne!(
            scope.root.as_deref(),
            Some(Path::new("src")),
            "the relative path must not be used as the probe cwd"
        );
    }

    /// A blank value is indistinguishable from unset and must not be treated as
    /// a path (`PathBuf::from("")` is neither absolute nor a directory, so it
    /// would otherwise be misreported as `NotADirectory`).
    #[test]
    fn resolve_probe_scope_rejects_blank_as_blank() {
        assert_eq!(
            resolve_probe_scope(Some("   ")).rejected,
            Some(ScopeRootRejection::Blank)
        );
    }

    /// An absolute path that is a FILE, not a directory, must fall through the
    /// same way a missing one does — a file cwd fails every spawn.
    #[test]
    fn resolve_probe_scope_rejects_an_absolute_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not_a_dir");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(
            resolve_probe_scope(Some(&file.to_string_lossy())).rejected,
            Some(ScopeRootRejection::NotADirectory)
        );
    }

    /// No configured value is the common case and is NOT a rejection — only a
    /// dropped declaration is, since that is the case an operator needs told.
    #[test]
    fn resolve_probe_scope_reports_no_rejection_when_unconfigured() {
        assert!(resolve_probe_scope(None).rejected.is_none());
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
