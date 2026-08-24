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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
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

/// Wall-clock budget for the FIRST `<cmd> --version` probe during CAPTURE.
///
/// Deliberately tight: capture runs on a loop against three tools (interval from
/// `QONTINUI_ENV_CAPTURE_INTERVAL_SECS`, default 900s but **floored at 60s**, so
/// the tick this has to fit inside is operator-configurable and can be one
/// minute — see [`super::capture_interval_secs`]), and a wedged shim must cost
/// the runner seconds, not minutes. The budget itself is NOT the thing to relax
/// — what a box under load needs is for *exceeding* it to be represented
/// honestly (see [`ProbeOutcome::TimedOut`]), not for the runner to sit on a
/// wedged shim for minutes.
///
/// Exceeding it is no longer a verdict on its own: it opens the confirmation
/// ladder in [`version_of_confirmed`], and only a key that survives the whole
/// ladder is reported unmeasured.
const CAPTURE_PROBE_BUDGET: Duration = Duration::from_secs(3);

/// Budget for the SECOND, confirming `<cmd> --version` attempt, made only after
/// the first one timed out and the resolved binary did not prove itself a stub.
///
/// **Composition — this pair, not [`CAPTURE_PROBE_BUDGET`] alone, is what bounds
/// a capture.** A key is probed at most twice and only the timeout arm retries,
/// so the worst case for one key is `CAPTURE_PROBE_BUDGET + this` = 13s, and for
/// the whole `versions` section (three keys, by construction — see
/// [`super::apply_versions::APPLIABLE_TOOLS`]) 39s.
///
/// **Where that 39s actually lands.** At the 900s default interval it is 4% of a
/// tick; at the 60s FLOOR an operator can configure
/// (`QONTINUI_ENV_CAPTURE_INTERVAL_SECS`) it is 65% of one. So the honest claim
/// is bounded-and-known, not negligible: the capture is a `spawn_blocking` task
/// (see `super::build_envelope`) precisely because 39s is long enough to matter,
/// and an operator who shortens the interval towards the floor is choosing to
/// spend most of a tick on probes in the worst case. The prose this replaced
/// said "seconds, not minutes against a 15-minute loop", which was true only of
/// the default and only if nobody ever set the variable.
///
/// Why a longer budget rather than a second one at 3s: the observed failure is a
/// box under load, where `rustc --version` has been measured between 173 ms and
/// 6858 ms at 14% CPU idle. A repeat at the same budget would re-lose the same
/// races; a genuinely-present-but-slow tool answers inside 10s, while a wedged
/// shim does not answer at any budget worth paying on a periodic loop.
const CAPTURE_PROBE_RETRY_BUDGET: Duration = Duration::from_secs(10);

/// After this many CONSECUTIVE captures reporting the same key unmeasured, the
/// per-capture warn escalates from "the box was busy" to "this probe is wedged
/// and will not self-resolve". Four captures spans three intervals — ~45 minutes
/// at the 900s default, ~3 at the 60s floor — long enough at the default that
/// transient load has had every chance to clear. The warn carries the REAL
/// elapsed time rather than a hardcoded one; see [`unknown_streak_message`].
///
/// It deliberately does NOT decay to [`ProbeOutcome::Failed`]: "we still cannot
/// read it" never becomes "you do not have it", because the action that
/// classification unlocks is installing over a toolchain nobody measured — the
/// exact defect this whole path exists to prevent. The escalation is louder, not
/// more permissive.
const UNKNOWN_STREAK_ALARM: u32 = 4;

/// What one bounded `<cmd> --version` probe actually established.
///
/// The two failure arms are NOT interchangeable, and collapsing them into a
/// single `None` is what made a busy box look like a box missing its toolchain:
///
/// - [`Failed`](Self::Failed) — spawn error, non-zero exit, or empty output. The
///   tool is genuinely not usable here, so a canonical key this box lacks really
///   IS missing and really IS something an apply should install.
/// - [`TimedOut`](Self::TimedOut) — the child was still running when the budget
///   expired and was killed. This says nothing whatsoever about the installed
///   version; it says the box was busy. `rustc --version` has been measured on
///   this fleet between 173 ms and 6858 ms at 14% CPU idle, so this arm is a
///   routine outcome, not a pathological one.
///
/// From [`version_of_within`] this is a CANDIDATE verdict, not a finding:
/// [`version_of_confirmed`] must rule out a stub binary and spend a second,
/// longer attempt before it stands. "We do not know" is the answer with the
/// widest blast radius here (it suppresses an apply), so it is the one that has
/// to be earned rather than defaulted into.
///
/// Callers that genuinely do not care (the apply's post-install re-probe, which
/// treats "no answer" as "verification failed" either way) collapse it with
/// [`into_version`](Self::into_version).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProbeOutcome {
    /// The probe finished inside its budget; carries the trimmed first line.
    Measured(String),
    /// The probe was still running at the deadline and was killed. The value is
    /// UNKNOWN — never treat this as "the tool is absent".
    TimedOut,
    /// Spawn failure, non-zero exit, or no output: genuinely not usable here.
    Failed,
}

impl ProbeOutcome {
    /// The measured version, discarding WHY there wasn't one. Only for callers
    /// that treat both failure arms identically.
    pub(crate) fn into_version(self) -> Option<String> {
        match self {
            Self::Measured(v) => Some(v),
            Self::TimedOut | Self::Failed => None,
        }
    }
}

/// Run a bounded `<cmd> --version` and report what it established, taking every
/// cheap step available to keep [`ProbeOutcome::TimedOut`] an ESTABLISHED FACT
/// rather than the residue left when nothing else matched. Mirrors
/// `fleet::detect_claude_code_now`'s bounded subprocess pattern.
///
/// `cwd` is the declared capture scope (see [`probe_scope_root`]). `None`
/// inherits the parent's cwd — reserved for the no-home-directory case, since
/// an inherited cwd is exactly what makes this probe non-deterministic.
fn version_of(cmd: &str, args: &[&str], cwd: Option<&Path>) -> ProbeOutcome {
    version_of_confirmed(
        cmd,
        args,
        cwd,
        CAPTURE_PROBE_BUDGET,
        CAPTURE_PROBE_RETRY_BUDGET,
    )
}

/// The confirmation ladder behind [`version_of`], with explicit budgets so the
/// whole ladder is testable in milliseconds instead of seconds.
///
/// `TimedOut` is the ONLY outcome that says "we do not know", and an outcome
/// that means "we do not know" must be earned, not defaulted into. Three things
/// stand between a slow first probe and that verdict:
///
/// 1. [`version_of_within`] drains both pipes concurrently, so a shim that
///    writes more than a pipe buffer (~4 KB) before exiting — an nvm-windows
///    "no version selected" dump, a corepack/volta banner — exits normally and
///    classifies on its exit status instead of blocking on its own write.
/// 2. Anything that timed out gets ONE more attempt at `retry_budget`. A
///    present-but-slow tool answers there; a wedged one does not.
/// 3. ONLY THEN is the resolved binary inspected: an App Execution Alias
///    placeholder spawns cleanly and hangs forever, which is indistinguishable
///    from a busy box by the clock alone, so proving one is POSITIVE evidence of
///    absence. See [`is_non_executable_stub`].
///
/// **Step 3 runs LAST, and that ordering is the fix for a live regression.** It
/// used to sit between steps 1 and 2, so a tool the stub check misjudged never
/// got its confirming attempt at all — the single mechanism that rescues a
/// slow-but-present tool was spent nowhere on exactly the boxes where the check
/// misfires. `rustc --version` has been measured on this fleet at 6858 ms, well
/// past the 3s first budget, so reaching step 3 is routine rather than
/// pathological and a wrong answer there installs over a live toolchain. Every
/// probe now gets its confirming attempt before ANY negative verdict; a check
/// that needs to pre-empt the retry to be useful is a check that is deciding on
/// too little.
///
/// Only a key that survives all three is reported unmeasured.
fn version_of_confirmed(
    cmd: &str,
    args: &[&str],
    cwd: Option<&Path>,
    budget: Duration,
    retry_budget: Duration,
) -> ProbeOutcome {
    version_of_confirmed_with(
        cmd,
        args,
        cwd,
        budget,
        retry_budget,
        &is_non_executable_stub,
    )
}

/// [`version_of_confirmed`] with an injectable stub check, so the LADDER ORDER
/// is testable without an App Execution Alias — a shape no test can create.
///
/// Injecting a check that condemns everything and still getting `Measured` out
/// of a tool that answers on the retry is the direct pin on step 3 running last.
fn version_of_confirmed_with(
    cmd: &str,
    args: &[&str],
    cwd: Option<&Path>,
    budget: Duration,
    retry_budget: Duration,
    is_stub: &dyn Fn(&Path) -> bool,
) -> ProbeOutcome {
    let first = version_of_within(cmd, args, cwd, budget);
    if first != ProbeOutcome::TimedOut {
        return first;
    }

    let second = version_of_within(cmd, args, cwd, retry_budget);
    if second != ProbeOutcome::TimedOut {
        return second;
    }

    if let Some(path) = resolve_executable(cmd) {
        if is_stub(&path) {
            warn!(
                "env_agent: `{cmd}` resolves to {} — a Windows App Execution Alias placeholder, \
                 which spawns cleanly and then never answers. It answered at neither budget, so \
                 classifying it as ABSENT (a definite negative), not as unmeasured.",
                path.display(),
            );
            return ProbeOutcome::Failed;
        }
    }

    second
}

/// Resolve a command to the file [`std::process::Command::spawn`] would
/// actually execute. Used ONLY to inspect a binary after its probe timed out —
/// a bare `Command::spawn` tells us nothing about WHAT it spawned, and that is
/// exactly what separates "absent tool wearing an alias" from "busy box".
///
/// **It must mirror `spawn`, not PATH folklore.** Inspecting a DIFFERENT file
/// from the one that ran is worse than not inspecting at all: the verdict it
/// feeds ([`is_non_executable_stub`] → `Failed` → installable) is then a
/// statement about a file that never executed. Rust std's Windows `resolve_exe`
/// is the spec here, and it is narrower than PATH+PATHEXT lore in three ways
/// this function used to get wrong:
///
/// - **`.exe` only, and only when the name carries no dot of its own.** std does
///   NOT consult `PATHEXT`, so a `python.cmd` / `.bat` / `.com` on PATH is
///   something `spawn` will refuse to run — returning it meant stubbing-out a
///   file the probe never touched.
/// - **No bare-name candidate on Windows.** An extensionless `python` (a
///   Git-for-Windows `usr/bin` shell script, say) sitting ahead of the real
///   `python.exe` was tried FIRST and returned, though std would never run it.
/// - **The running EXE's own directory is searched BEFORE PATH.** That is where
///   std looks first, so a name shadowed there resolves to the runner's copy.
///
/// Empty `PATH` entries (a `;;` or a trailing `;`) are dropped: `split_paths`
/// yields an empty `PathBuf` for them, and `dir.join(cmd)` would then produce a
/// RELATIVE path resolved against the runner process's cwd — a silent second cwd
/// in a module whose whole scope story is that the probe runs where it was told.
///
/// Uses `symlink_metadata` throughout: a Windows App Execution Alias is a
/// reparse point whose target does not resolve, so `Path::is_file` (which
/// follows) can report `false` for the very file we need to look at.
fn resolve_executable(cmd: &str) -> Option<PathBuf> {
    fn is_existing_file(path: &Path) -> bool {
        std::fs::symlink_metadata(path)
            .map(|m| !m.is_dir())
            .unwrap_or(false)
    }

    let raw = Path::new(cmd);
    // Already a path, not a name to look up.
    if raw.components().count() > 1 {
        return is_existing_file(raw).then(|| raw.to_path_buf());
    }

    // The candidate FILE NAMES, in the order the loader tries them.
    let names: Vec<String> = if cfg!(windows) {
        if cmd.contains('.') {
            vec![cmd.to_string()]
        } else {
            vec![format!("{cmd}.exe")]
        }
    } else {
        vec![cmd.to_string()]
    };

    // The DIRECTORIES, in the order the loader searches them.
    let mut dirs: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        if let Some(own_dir) = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
        {
            dirs.push(own_dir);
        }
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path_var).filter(|d| !d.as_os_str().is_empty()));
    }

    for dir in dirs {
        for name in &names {
            let candidate = dir.join(name);
            if is_existing_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// `IO_REPARSE_TAG_APPEXECLINK` — a Windows App Execution Alias. THE positive
/// fact this check is built on.
const IO_REPARSE_TAG_APPEXECLINK: u32 = 0x8000_001B;
/// `IO_REPARSE_TAG_SYMLINK` — an ordinary NTFS symbolic link, to be FOLLOWED.
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
/// `IO_REPARSE_TAG_MOUNT_POINT` — a junction; also a thing to follow.
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

/// The on-disk shape of one resolved path, as far as the filesystem can tell.
/// Split out from the filesystem so [`stub_verdict`] can be unit-tested over
/// shapes this box may not be able to CREATE (a symlink needs Developer Mode or
/// admin; an App Execution Alias cannot be forged at all).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileShape {
    is_dir: bool,
    len: u64,
    /// The Windows reparse tag, when this file is a reparse point AND the tag
    /// could be read. `None` covers both "an ordinary file" and "we could not
    /// find out" — see [`stub_verdict`] for why those fold together safely.
    reparse_tag: Option<u32>,
    /// True when the OS reports a reparse point, whatever its tag. Lets the
    /// fallback arm fire when the tag read itself failed.
    is_reparse_point: bool,
    /// Whether the path sits under a `WindowsApps` directory. Only ever used to
    /// NARROW the tagless fallback, never to condemn on its own.
    under_windows_apps: bool,
}

/// What [`stub_verdict`] concluded about one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StubVerdict {
    /// Positive evidence the tool is absent: this file cannot be a working
    /// executable.
    Stub,
    /// This file says nothing that would justify a negative verdict.
    NotStub,
    /// A link: it says nothing ITSELF. Follow it and judge the target.
    FollowLink,
}

/// Classify one file's shape. **Pure — unit-tested, including over shapes this
/// box cannot create.**
///
/// **A 0-byte file is NOT evidence of absence, and treating it as such was a
/// live regression.** On NTFS a symlink is a 0-byte reparse point, so
/// `symlink_metadata().len()` reports 0 for ANY symlinked exe — and every rustup
/// proxy is exactly that (`~/.cargo/bin/rustc.exe` → `rustup.exe`, measured 0
/// bytes on this fleet). Condemning 0-byte files therefore condemned a working,
/// current toolchain: the probe would time out under load, the stub check would
/// call it `Failed`, the key would diff as `Missing`, and `env apply` would
/// install over it. That is the exact harm this whole path exists to prevent,
/// re-entered from the other side.
///
/// The 0-byte-ness is also not what makes an App Execution Alias a stub — the
/// same directory holds ~34 WORKING 0-byte aliases on this box (`wsl.exe`,
/// `winget.exe`, `wt.exe`, `bash.exe`). What makes the placeholder a stub is its
/// reparse TAG plus the fact that it answered at neither budget, so that is what
/// is checked, in that order:
///
/// - a symlink or junction is FOLLOWED and its target judged instead;
/// - an `IO_REPARSE_TAG_APPEXECLINK` is a stub;
/// - a reparse point whose tag could not be read, 0 bytes, under `WindowsApps`,
///   is a stub too — the same file seen through a failed tag read, and narrow
///   enough that an ordinary file can never land there;
/// - everything else, 0 bytes or not, is NOT a stub. A plain 0-byte exe cannot
///   be `CreateProcess`d at all, so it fails to SPAWN and is reported `Failed`
///   long before this check is reached.
///
/// Fail-safe direction is NotStub: an unknown shape must fall through to
/// "unmeasured", which suppresses an apply, never to "absent", which triggers
/// one.
///
/// **Residual, stated rather than hidden:** an INSTALLED Store app wears the
/// same `IO_REPARSE_TAG_APPEXECLINK` as an uninstalled one — the difference
/// lives inside the reparse buffer, not in the tag or the file shape. What
/// separates them for us is the CLOCK, and only because this runs last: an
/// installed app's alias launches the app, which answers, so reaching here means
/// the alias produced nothing at 3s AND nothing at 10s. That is the whole of the
/// evidence, and it is why the check may not move earlier in the ladder.
fn stub_verdict(shape: &FileShape) -> StubVerdict {
    if shape.is_dir {
        return StubVerdict::NotStub;
    }
    match shape.reparse_tag {
        Some(IO_REPARSE_TAG_SYMLINK | IO_REPARSE_TAG_MOUNT_POINT) => {
            return StubVerdict::FollowLink
        }
        Some(IO_REPARSE_TAG_APPEXECLINK) => return StubVerdict::Stub,
        _ => {}
    }
    if shape.is_reparse_point
        && shape.reparse_tag.is_none()
        && shape.len == 0
        && shape.under_windows_apps
    {
        return StubVerdict::Stub;
    }
    StubVerdict::NotStub
}

/// Read one path's [`FileShape`]. `None` when the path cannot be stat'd at all.
///
/// `symlink_metadata` on purpose: an App Execution Alias is a reparse point
/// whose target does not resolve, so anything that FOLLOWS reports `false`/error
/// for the very file we need to look at.
fn file_shape(path: &Path) -> Option<FileShape> {
    let md = std::fs::symlink_metadata(path).ok()?;
    let mut shape = FileShape {
        is_dir: md.is_dir(),
        len: md.len(),
        reparse_tag: None,
        is_reparse_point: false,
        under_windows_apps: path
            .components()
            .any(|c| c.as_os_str().eq_ignore_ascii_case("WindowsApps")),
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        shape.is_reparse_point = md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
        if shape.is_reparse_point {
            // The tag is the whole discriminator, so read it from the OS rather
            // than inferring it. `FileType::is_symlink` is the std fallback: it
            // is true for exactly the two tags std knows how to follow, and
            // false for an App Execution Alias.
            shape.reparse_tag =
                reparse_tag_of(path).or_else(|| md.is_symlink().then_some(IO_REPARSE_TAG_SYMLINK));
        }
    }
    #[cfg(not(windows))]
    {
        shape.is_reparse_point = md.is_symlink();
        if md.is_symlink() {
            shape.reparse_tag = Some(IO_REPARSE_TAG_SYMLINK);
        }
    }
    Some(shape)
}

/// The reparse tag of `path`, via `FindFirstFileW`'s `dwReserved0` — the
/// documented way to read a tag without opening (and thus following) the file.
/// `None` when the path cannot be enumerated or is not a reparse point.
#[cfg(windows)]
fn reparse_tag_of(path: &Path) -> Option<u32> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{FindClose, FindFirstFileW, WIN32_FIND_DATAW};
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let mut data: WIN32_FIND_DATAW = std::mem::zeroed();
        let handle = FindFirstFileW(wide.as_ptr(), &mut data);
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return None;
        }
        FindClose(handle);
        (data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0).then_some(data.dwReserved0)
    }
}

/// How many links [`is_non_executable_stub`] will follow before giving up.
/// Bounded so a link cycle costs a handful of `stat`s rather than the capture.
const MAX_LINK_HOPS: usize = 8;

/// True when this file CANNOT be a working executable — positive evidence of an
/// absent tool, cheap enough to check after both probe attempts have failed.
///
/// The motivating case: on a box with no real Python, `python` resolves to
/// `%LOCALAPPDATA%\Microsoft\WindowsApps\python.exe`, an App Execution Alias
/// placeholder that spawns fine and then hangs. By the clock that is
/// indistinguishable from a slow tool, and classifying it unmeasured would leave
/// a box that genuinely needs Python installed permanently un-installable.
///
/// A LINK is followed to its target and the target judged, which is what keeps a
/// rustup proxy (`rustc.exe` → `rustup.exe`, a 0-byte reparse point on NTFS)
/// out of the stub arm. A dangling link resolves to nothing and returns `false`
/// — it cannot spawn either, but it would have failed at spawn time and been
/// reported `Failed` there, so there is nothing for this check to add.
///
/// See [`stub_verdict`] for the classification itself.
fn is_non_executable_stub(path: &Path) -> bool {
    let mut current = path.to_path_buf();
    for _ in 0..MAX_LINK_HOPS {
        let Some(shape) = file_shape(&current) else {
            return false;
        };
        match stub_verdict(&shape) {
            StubVerdict::Stub => return true,
            StubVerdict::NotStub => return false,
            StubVerdict::FollowLink => {
                let Ok(target) = std::fs::read_link(&current) else {
                    return false;
                };
                // A link target may be relative TO THE LINK, not to any cwd —
                // rustup writes exactly that (`rustc.exe` → `rustup.exe`).
                current = if target.is_absolute() {
                    target
                } else {
                    match current.parent() {
                        Some(dir) => dir.join(target),
                        None => return false,
                    }
                };
            }
        }
    }
    // A cycle, or a chain longer than anything real: we established nothing.
    false
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
) -> ProbeOutcome {
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
    // A spawn failure is a genuine "not usable here" — the shim does not exist,
    // or is not executable. Nothing about it is a measurement problem.
    let Ok(mut child) = command.spawn() else {
        return ProbeOutcome::Failed;
    };

    // Reap the whole TREE, not just the child. `Child::kill` is
    // `TerminateProcess` on ONE process, and a shim (a rustup proxy, a
    // volta/pyenv-win shim, `cmd /c …`) runs the real tool as a GRANDCHILD
    // holding inherited duplicates of both pipe write ends — so killing the
    // child alone orphans a process AND leaves the pipes without an EOF. The
    // guard drops on every return path below.
    let _tree = crate::process_helpers::ChildTreeGuard::attach(&child);

    // Drain BOTH pipes from the moment the child exists, not after it exits.
    //
    // This is load-bearing, not tidiness. Both streams are `piped()`, the pipe
    // buffer is small (~4 KB on Windows), and a child that fills it BLOCKS in
    // `write` until someone reads. Reading only after `try_wait` observes an
    // exit therefore deadlocks against any shim that is chatty before it exits —
    // an nvm-windows "no version selected" dump, a corepack/volta banner — and
    // the deadlock expires as a timeout, i.e. as "we could not measure this
    // box's toolchain" when in truth the tool answered at length. Draining
    // concurrently turns that entire class back into an ordinary exit.
    let stdout_reader = PipeDrain::spawn(child.stdout.take());
    let stderr_reader = PipeDrain::spawn(child.stderr.take());

    let deadline = started + budget;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Wait for the readers on a BOUNDED clock, and never by joining.
                // A join here has no timeout: if a grandchild outlived the
                // parent (a corepack/volta background update check is the
                // realistic shape) it still holds the write ends, the pipes
                // never EOF, and `collect_versions` hangs FOREVER — bypassing
                // every budget in this module and stalling the capture loop,
                // whose `MissedTickBehavior::Skip` never advances past a task
                // that does not return. The readers publish into a shared
                // buffer instead, so a partial answer is still readable.
                let drained = wait_for_drain(&stdout_reader, &stderr_reader, READER_DRAIN_GRACE);
                let stdout = stdout_reader.snapshot();
                let stderr = stderr_reader.snapshot();
                if !status.success() {
                    return ProbeOutcome::Failed;
                }
                let out = if stdout.trim().is_empty() {
                    stderr
                } else {
                    stdout
                };
                let first = out.lines().next().unwrap_or("").trim().to_string();
                return if !first.is_empty() {
                    ProbeOutcome::Measured(first)
                } else if drained {
                    // Both pipes reached EOF with nothing in them: the tool ran
                    // and said nothing. A definite negative.
                    ProbeOutcome::Failed
                } else {
                    // We gave up on a reader that never reached EOF. The empty
                    // output is OUR gap, not the box's — "we do not know" is the
                    // only honest verdict, and it is the one that suppresses an
                    // apply rather than triggering one.
                    ProbeOutcome::TimedOut
                };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    // The readers are left running rather than joined or
                    // awaited: we have given up on this child, and the caller
                    // must not wait on it. They own nothing of ours (the buffer
                    // is an `Arc`), and dropping `_tree` on the way out kills
                    // the grandchildren that would otherwise hold the pipes
                    // open, so they see EOF and exit instead of blocking
                    // forever.
                    //
                    // THE distinguished arm: we killed a live child, so we
                    // learned nothing about the installed version. It is a
                    // candidate verdict only — `version_of_confirmed` must
                    // spend the retry and rule out the stub before it stands.
                    return ProbeOutcome::TimedOut;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            // `try_wait` failing is an OS-level problem with the child handle,
            // not a slow tool — the box did answer, badly.
            Err(_) => return ProbeOutcome::Failed,
        }
    }
}

/// How much of each pipe is KEPT. Only the first line is ever used, so anything
/// past this is read-and-discarded: the deadlock a chatty shim causes is broken
/// by DRAINING, not by keeping, and a cap that stopped reading would re-create
/// it for any tool that prints more than the cap.
const PIPE_KEEP_BYTES: usize = 64 * 1024;

/// How long the EXIT path will wait for the readers to reach EOF before
/// reporting on what it already has.
///
/// A child that has exited has closed its own write ends, so this is normally
/// consumed in microseconds. It is only ever spent when something ELSE still
/// holds a duplicate — precisely the case that used to hang forever.
const READER_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// One child pipe, drained on its own thread into a bounded shared buffer.
///
/// Shared rather than returned-on-join for two reasons: the parent must be able
/// to give up on a reader that will never finish, and a reader blocked on a
/// grandchild's copy of the write end has usually ALREADY read the version line
/// — a partial answer here is a measured probe rather than an unknown one.
struct PipeDrain {
    /// The first [`PIPE_KEEP_BYTES`] read so far.
    kept: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    /// Set when the reader has seen EOF (or given up). The parent polls this
    /// instead of joining.
    finished: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl PipeDrain {
    fn spawn<R: std::io::Read + Send + 'static>(pipe: Option<R>) -> Self {
        use std::sync::atomic::Ordering;

        let kept = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let Some(mut p) = pipe else {
            // No pipe is the same as an immediately-empty one.
            finished.store(true, Ordering::Release);
            return Self { kept, finished };
        };
        let (kept_w, finished_w) = (kept.clone(), finished.clone());
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            let mut seen: usize = 0;
            loop {
                match std::io::Read::read(&mut p, &mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        if seen < PIPE_KEEP_BYTES {
                            let room = PIPE_KEEP_BYTES - seen;
                            if let Ok(mut buf) = kept_w.lock() {
                                buf.extend_from_slice(&chunk[..n.min(room)]);
                            }
                        }
                        seen = seen.saturating_add(n);
                        // Deliberately keep READING past the cap and throw the
                        // rest away: a child blocked in `write` never exits.
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            finished_w.store(true, Ordering::Release);
        });
        Self { kept, finished }
    }

    fn is_finished(&self) -> bool {
        self.finished.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Whatever has been read so far. Lossy on purpose: a shim emitting non-UTF-8
    /// must not be able to turn a successful probe into an empty one.
    fn snapshot(&self) -> String {
        match self.kept.lock() {
            Ok(buf) => String::from_utf8_lossy(&buf).into_owned(),
            // A panicking reader thread cannot be allowed to poison the probe
            // into a false negative either.
            Err(poisoned) => String::from_utf8_lossy(&poisoned.into_inner()).into_owned(),
        }
    }
}

/// Wait until both readers have reached EOF, or `grace` expires. Returns whether
/// they finished — the caller needs that to tell "the tool printed nothing" from
/// "we stopped reading".
fn wait_for_drain(a: &PipeDrain, b: &PipeDrain, grace: Duration) -> bool {
    let deadline = std::time::Instant::now() + grace;
    loop {
        if a.is_finished() && b.is_finished() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Re-run the EXACT probe [`collect_versions`] uses, with an explicit budget.
///
/// The `versions` apply (P2b slice 3) verifies its own work by re-probing after
/// driving a version manager. That verification is only worth anything if it
/// asks the same question the capture asks — same command, same args, same cwd
/// resolution — so it goes through this one function rather than a lookalike.
/// The budget is a parameter because an operator-invoked apply can afford to
/// wait longer than a periodic capture loop can (whose interval is itself
/// configurable — see [`super::capture_interval_secs`]).
///
/// Collapses [`ProbeOutcome`] to an `Option` on purpose: the apply's
/// verification treats "no answer" as "not verified" whichever arm produced it,
/// and it runs on a budget generous enough that the timeout arm carries no extra
/// information there.
pub(crate) fn probe_tool_version(
    cmd: &str,
    cwd: Option<&Path>,
    budget: Duration,
) -> Option<String> {
    version_of_within(cmd, &["--version"], cwd, budget).into_version()
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

// ============================================================================
// python dependency capture — TWO halves, and they are not interchangeable
// ============================================================================
//
// Plan `2026-08-08-ci-tool-registry-and-canonical-configuration-parity`, P3.
//
// ## The gap
//
// The captured `versions` vocabulary carried a `node_dep_` prefix (per-dep keys
// built as `format!("node_dep_{dep}")`, see [`put_sibling_tree_versions`]) and
// **no python equivalent** — `git grep python_dep_ origin/main` was empty in
// both qontinui-runner and qontinui-web. `python_constraint` was never a
// substitute: it is the single INTERPRETER constraint (`^3.12`) out of
// `tool.poetry.dependencies.python`, so the largest python surface in the fleet
// contributed nothing a drift oracle could compare.
//
// ## Why one half would have re-created the bug it fixes
//
// The obvious fix — mirror `node_dep_` for python — closes the *vocabulary*
// gap and NOT the drift gap, because of two facts that only bite together:
//
// 1. `node_dep_*` stores the **declared range** out of `package.json`, not an
//    installed version (`collectors.rs`, [`put_sibling_tree_versions`]). So does
//    `python_constraint`. Both are COMMITTED artifacts — as is a lockfile.
//    Two boxes on the same commit hold identical values no matter what is
//    actually installed in their environments.
// 2. Keys the server classifies as repo-**derived** are excluded from the
//    `in_sync` oracle (qontinui-web `devenv_drift.py`): a difference on a
//    derived key yields severity `info` and leaves `in_sync == True`.
//
// Compose them and a python capture built only from pyproject/poetry.lock,
// classified derived, reports `in_sync: True` for a box whose site-packages
// differ completely — the exact "reports success while measuring nothing" class
// this phase exists to remove, rebuilt by the fix for it.
//
// So this module ships BOTH halves, with deliberately DIFFERENT prefixes
// because the prefix is what selects the server-side classification:
//
// | half | prefix | reads | classification | answers |
// |------|--------|-------|----------------|---------|
// | A | `python_dep_` | the backend `pyproject.toml` | repo-derived (`info`) | "what does the project DECLARE" |
// | B | `python_installed_` | a live `pip list` | NOT derived (real drift) | "what is actually INSTALLED here" |
//
// Half A is vocabulary parity with `node_dep_*`: same source kind (a declared
// range), same severity, converging by updating the checkout. qontinui-web has
// already registered `python_dep_` in
// `devenv_section_policy._DERIVED_KEY_PREFIXES` (branch
// `p3/python-dep-drift-policy`), and that registration is why Half A must NOT
// carry installed-version semantics: the server would classify a real
// installed-package difference `info` and hide it.
//
// Half B is what closes the gap. It must NEVER be registered as derived, and
// its prefix is deliberately distinct so it cannot be swallowed by a
// `startswith("python_dep_")` rule.
//
// ## Cardinality — one key per DECLARED dep, a fixed FIVE for the environment
//
// Half A is bounded by the manifest: 37 direct runtime deps declared beside
// `python` in the backend pyproject as measured 2026-08-18. That is the same
// order as `node_dep_*` and the same thing a human edits, so per-key is worth
// its cost — the value of these keys is that a reader sees WHICH declaration
// moved.
//
// Half B is bounded by construction at FIVE keys regardless of environment
// size, because the environment is where the counts explode: the backend's own
// lock already resolves to 133 packages and a real site-packages is larger
// still, so one key per installed package would be hundreds of keys per machine
// per capture — the cost the plan warns about, on the half that runs on every
// box rather than only on boxes with a checkout. A sha256 over the sorted
// `name==version` set buys full-fidelity detection (any package, any version,
// added or removed, moves it) for O(1) keys. What a digest cannot do is say
// WHICH package moved; that is an accepted trade, and it is why Half A exists
// beside it — between them a reader gets "the declaration moved" per-key and
// "something in the environment moved" at a glance.
//
// ## Provenance, and why each half carries its own
//
// [`ProbeScopeKind`] is the precedent: a key that exists so a consumer can tell
// whether canonical's numbers are even COMPARABLE with its own before acting on
// a difference. Both halves follow it.
//
// Half B needs its own scope key rather than leaning on `probe_scope_kind`,
// even though it runs in the same resolved scope. `probe_scope_kind` is
// server-classified **derived**, so a scope difference between two boxes is
// `info` and does not break `in_sync` — exactly the swallowing this half exists
// to avoid. A digest difference caused by measuring two different scopes would
// then be reported as installed drift with nothing to say it was not
// comparable. [`PYTHON_INSTALLED_SCOPE_KEY`] is the non-derived twin.
//
// Half B carries TWO further comparability keys,
// [`PYTHON_INSTALLED_ENV_KIND_KEY`] and [`PYTHON_INSTALLED_INTERPRETER_KEY`],
// because the scope kind does not identify the interpreter. The interpreter
// comes off the inherited PATH, so a runner launched from an activated venv and
// the same runner launched from a plain shell inventory different environments
// on one box — the launch-dependence `resolve_probe_scope` exists to keep out
// of the toolchain keys, landing here on the one family that drives `in_sync`.
// The env kind is the gate that makes those two readings refusable instead of
// silently comparable, and it is sourced from the interpreter itself so it
// cannot disagree with the digest it certifies. Full reasoning, and what the
// gate does NOT fix, in the Half B section header.
//
// ## Silence is never success
//
// Both provenance keys are written UNCONDITIONALLY, including on every rung
// where nothing was found, so "measured, found nothing" is a different wire
// state from "could not measure":
//
// - measured-empty → `python_installed_probe = "measured"`, `..._count = "0"`,
//   a digest present (the digest of the empty set).
// - unmeasurable → `python_installed_probe` names WHY and the count, digest and
//   interpreter keys are ABSENT, so an unmeasured box can never be read as `0`
//   packages.
//
// The COMPLETE `python_installed_probe` vocabulary is exactly these six, and
// [`PythonInventoryProbe::wire`] is its only producer:
//
// | value | meaning |
// |-------|---------|
// | `measured` | the inventory was read; count + digest + interpreter present |
// | `scope_unusable` | the resolved probe scope is not a usable directory |
// | `python_absent` | no `python` on PATH in that scope |
// | `probe_failed` | the interpreter ran and exited non-zero |
// | `probe_timeout` | the probe outlived its budget and was killed |
// | `unparseable_output` | exited 0, output was not the document we can read |
//
// A consumer implementing "anything other than `measured` is not clean" should
// match on `measured` and treat every other value — including one added later —
// as not-clean, rather than enumerating the failures.
//
// **Known residuals, stated rather than hidden.** A capture reports; it cannot
// force a verdict. Two of them are open:
//
// 1. Two boxes BOTH unmeasurable for the same reason hold equal values and
//    would compare clean. Closing that needs the consuming oracle to treat a
//    `python_installed_probe` other than `measured` as not-clean — a
//    qontinui-web change, not a runner one.
// 2. The comparability gate NARROWS the incomparability; it does not close it.
//    Three known blind spots, in rough order of how often they will bite:
//
//    a. `not_venv` means only `sys.prefix == sys.base_prefix`. Conda
//       environments satisfy that (`base` and named envs alike), as do
//       pyenv-managed installs and any second system python. Adding
//       `python_installed_interpreter` catches the largest slice — 3.12 against
//       3.13 no longer passes — but two boxes on different conda envs of the
//       SAME minor version still compare.
//    b. The gate certifies the interpreter's PREFIX, not its `sys.path`.
//       `importlib.metadata.distributions()` walks all of `sys.path`, which
//       `PYTHONPATH` and `.pth` files extend — and `PYTHONPATH` is inherited
//       from the launching shell, i.e. the very launch-dependence this gate was
//       added to expose. Same box, same interpreter, two shells: different
//       inventory, identical `env_kind` and `interpreter`.
//    c. Two boxes that both resolve *a* venv both report `venv`, though the
//       venvs differ.
//
//    None of these makes the published markers WRONG; each is a case where
//    "these digests may be compared" is claimed more confidently than the
//    evidence supports. Closing them needs a declared interpreter resolved the
//    way `scope_root` is — deliberately out of scope here — and, for (b), a
//    `sys.path` fingerprint that would itself have to be path-free to be
//    publishable.

/// Provenance key for Half A: which source the `python_dep_*` declarations came
/// from, including every way there could be none.
///
/// ## Why the `__` infix, and why the prefix is still `python_dep_`
///
/// The prefix must stay `python_dep_` because qontinui-web classifies by
/// `str.startswith`: one prefix registration then covers this meta key too, and
/// a provenance marker can never be split off into "actionable drift" offering
/// to apply a source name.
///
/// Collision with a real package is structurally impossible rather than
/// unlikely: package-derived keys are built from a PEP 503 normalized name
/// ([`normalize_python_dep_name`]), which collapses every run of `-`, `_` and
/// `.` into a SINGLE separator, so no package name can produce a `__` infix.
const PYTHON_DEP_SOURCE_KEY: &str = "python_dep__source";

/// Half B provenance: HOW the installed inventory was obtained, and — when it
/// was not — why. See "Silence is never success" above.
const PYTHON_INSTALLED_PROBE_KEY: &str = "python_installed_probe";

/// Half B scope: WHICH kind of scope the inventory probe ran in. The
/// non-derived twin of `probe_scope_kind`; see the section header for why
/// leaning on that one would let an incomparability difference be swallowed.
const PYTHON_INSTALLED_SCOPE_KEY: &str = "python_installed_scope_kind";

/// Half B COMPARABILITY GATE: which kind of python environment the inventory
/// came from (`venv` | `system` | `unknown`).
///
/// This is not decoration beside the digest — it states whether two digests may
/// be compared AT ALL, the same job `probe_scope_kind` does for the toolchain
/// versions. A consumer that compares digests across differing values here is
/// reading incomparable numbers. Sourced from the interpreter itself
/// (`sys.prefix != sys.base_prefix`), never from `VIRTUAL_ENV`; see the Half B
/// section header for why a shell-set variable could disagree with the very
/// interpreter it would be certifying.
///
/// No path is emitted, only the kind — machine-local strings are dropped
/// structurally throughout this module.
const PYTHON_INSTALLED_ENV_KIND_KEY: &str = "python_installed_env_kind";

/// Half B: the interpreter's own `MAJOR.MINOR`, read in the SAME invocation
/// that produced the inventory.
///
/// The second half of the comparability gate, and the one that carries most of
/// its weight. [`PYTHON_INSTALLED_ENV_KIND_KEY`] only separates venv from
/// not-venv; two boxes on different conda envs, different pyenv installs, or
/// 3.12 against 3.13 all report `not_venv` and would otherwise pass the gate
/// with wholly incomparable digests. The interpreter version is free — the
/// script already has `sys.version_info` — and catches the largest slice of
/// that blind spot.
///
/// `MAJOR.MINOR`, not the full patch: a patch bump does not change which
/// packages are installed, so including it would manufacture incomparability
/// where none exists.
const PYTHON_INSTALLED_INTERPRETER_KEY: &str = "python_installed_interpreter";

/// Half B: how many packages the digest was taken over. Present ONLY when an
/// inventory was actually read, so `0` means "measured, environment is empty"
/// and absence means "not measured".
const PYTHON_INSTALLED_COUNT_KEY: &str = "python_installed_count";

/// Half B: sha256 over the sorted `name==version` set actually installed.
const PYTHON_INSTALLED_DIGEST_KEY: &str = "python_installed_digest";

/// Budget for the installed-inventory probe.
///
/// Five times [`CAPTURE_PROBE_BUDGET`] on purpose. The three `--version` shells
/// that budget was sized for print a string and exit; the inventory script
/// starts an interpreter and walks every distribution's metadata in
/// site-packages, which is I/O-bound and scales with the environment. Sizing it
/// at 3s would not make the capture faster — it would make this key read
/// `probe_timeout` on exactly the large environments it most needs to measure,
/// which is a false negative dressed as a budget.
///
/// A budget is NOT the mitigation for a stalled child: [`run_bounded_capture`]
/// drains stdout concurrently and never pipes stderr, so a child cannot block
/// on a full buffer. Raising this number would have hidden that bug rather than
/// fixed it.
const PYTHON_INVENTORY_BUDGET: Duration = Duration::from_secs(15);

/// Hard cap on stdout captured by [`run_bounded_capture`].
///
/// The poll-then-read shape this replaced was self-limiting at the 64 KiB pipe
/// capacity — but only BECAUSE it deadlocked there. Removing the deadlock
/// removed the accident that bounded memory, so the bound has to be stated
/// explicitly or a `python` on PATH that is not the interpreter we expect (a
/// shim printing a banner in a loop, a wrapper dumping a log) can allocate
/// hundreds of megabytes inside a GUI process, on a fleet already running at
/// memory pressure.
///
/// 8 MiB is far above any real measurement — a 5,000-package inventory
/// serializes well under 1 MiB — and far below a size that matters here. A read
/// truncated at the cap yields invalid JSON, which lands on
/// `unparseable_output`: the honest answer, since a truncated inventory is not
/// an inventory.
const STDOUT_CAPTURE_LIMIT: u64 = 8 * 1024 * 1024;

/// PEP 503 name normalization, rendered for use as a section key.
///
/// PEP 503 lowercases and collapses every run of `-`, `_` and `.` to a single
/// separator; we emit `_` as that separator so the key reads like the rest of
/// the section. `Pillow`, `python-multipart` and `opencv_python.headless`
/// therefore become `pillow`, `python_multipart`, `opencv_python_headless` —
/// stable across two boxes that spell the same dependency differently, which
/// would otherwise read as permanent drift.
///
/// Collapsing runs is also what makes the `__` meta-key infix unforgeable (see
/// [`PYTHON_DEP_SOURCE_KEY`]). Any character outside `[a-z0-9]` that is not a
/// separator is dropped rather than passed through, so a key is always a safe
/// identifier.
fn normalize_python_dep_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut pending_separator = false;
    for ch in raw.chars() {
        if matches!(ch, '-' | '_' | '.') {
            // Only remember the separator; emitting it is deferred so a
            // trailing run never lands in the key.
            pending_separator = !out.is_empty();
            continue;
        }
        if !ch.is_ascii_alphanumeric() {
            continue;
        }
        if pending_separator {
            out.push('_');
            pending_separator = false;
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

// ---------------------------------------------------------------------------
// Half A — python_dep_*: what the project DECLARES (repo-derived)
// ---------------------------------------------------------------------------

/// Which source the `python_dep_*` declarations were read from — including
/// every way there could be none.
///
/// Every non-[`Self::Pyproject`] variant means NO declaration keys were emitted,
/// and each names a DIFFERENT reason. That distinction is the point: without it
/// "this box declares nothing different" and "this box was never read" are the
/// same wire state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PythonDepSource {
    /// Read and parsed, declarations taken from `[tool.poetry.dependencies]`.
    Pyproject,
    /// Read and parsed, declarations taken from PEP 621 `[project]
    /// dependencies`.
    ///
    /// A DISTINCT value from [`Self::Pyproject`] because the two dialects spell
    /// the same declaration differently (`^0.136` vs `>=0.136`). Collapsing
    /// them would show a fleet mid-migration as every key drifted, with the
    /// provenance key insisting both sides read the same thing.
    PyprojectPep621,
    /// Read and parsed, both tables populated: Poetry 2.x's hybrid layout. The
    /// values come from `[project].dependencies`; the poetry table held
    /// overrides. Distinct so a version difference traceable to an override is
    /// not mistaken for a plain declaration difference.
    PyprojectHybrid,
    /// The pyproject exists but could not be read or parsed as TOML. A distinct
    /// state from absent: the checkout is there, the file is not usable.
    PyprojectUnreadable,
    /// No `qontinui-web/backend/pyproject.toml` under the workspace root — e.g.
    /// a production box without the source checkout.
    PyprojectAbsent,
    /// The workspace root itself did not resolve, so no sibling path could even
    /// be formed. Distinct from [`Self::PyprojectAbsent`]: there the layout was
    /// known and the file was missing; here the layout is unknown.
    WorkspaceUnresolved,
}

impl PythonDepSource {
    /// The stable wire string carried in `versions.python_dep__source`.
    pub fn wire(self) -> &'static str {
        match self {
            Self::Pyproject => "pyproject",
            Self::PyprojectPep621 => "pyproject_pep621",
            Self::PyprojectHybrid => "pyproject_hybrid",
            Self::PyprojectUnreadable => "pyproject_unreadable",
            Self::PyprojectAbsent => "pyproject_absent",
            Self::WorkspaceUnresolved => "workspace_unresolved",
        }
    }
}

/// Which spelling of the manifest a declaration set came from.
///
/// Kept apart because the two spell the SAME declaration differently — poetry's
/// `^0.136` against PEP 621's `>=0.136` — so a fleet mid-migration would show
/// every key drifted with nothing to say why. This is the same comparability
/// question `probe_scope_kind` answers for toolchain versions, one layer down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationDialect {
    /// `[tool.poetry.dependencies]` — caret/tilde constraints.
    Poetry,
    /// PEP 621 `[project] dependencies` — PEP 508 requirement strings.
    Pep621,
    /// BOTH tables carry entries: Poetry 2.x's documented hybrid layout, where
    /// `[project].dependencies` is the real list and `[tool.poetry.dependencies]`
    /// holds source/constraint OVERRIDES for a subset of it.
    ///
    /// Its own value because neither of the others is honest here. Reporting
    /// `poetry` would name a table that is not the list; reporting `pep621`
    /// would hide that overrides exist and may explain a version difference.
    Hybrid,
}

/// The DIRECT dependency declarations of a parsed backend pyproject, as
/// `(normalized name, declared constraint)` sorted by name, plus WHICH dialect
/// they were read from.
///
/// Mirrors `node_dep_*` exactly: the value is the DECLARED RANGE as written in
/// the manifest (`^0.136`, `>=0.0.9`), not a resolved or installed version.
/// Half B is where installed reality lives; conflating the two under this
/// prefix is the failure the section header describes.
///
/// Poetry spells a dependency two ways and both are read: a bare string
/// (`fastapi = "^0.136"`) and a table carrying extras
/// (`uvicorn = {extras = ["standard"], version = "^0.38"}`), whose `version`
/// field is the constraint. A table WITHOUT a `version` (a `path`/`git` dep) has
/// no comparable constraint to publish, so it contributes no key rather than a
/// fabricated one.
///
/// ## `[project].dependencies` wins wherever it exists
///
/// Poetry 2.x supports a documented hybrid layout that keeps
/// `[tool.poetry.dependencies]` for source and constraint OVERRIDES while the
/// real dependency list lives in `[project].dependencies`. Preferring the
/// poetry table whenever it is non-empty would publish the override SUBSET and
/// drop the real list, while `python_dep__source` still reported the manifest
/// read successfully — this module's own silence-reads-as-success failure one
/// layer down. Selection therefore keys on `[project].dependencies`, and the
/// hybrid gets its own dialect rather than being reported as either pure form.
///
/// `python` is excluded: it is the interpreter constraint, already published as
/// `python_constraint`.
///
/// Deliberately NOT the dev group — same call `node_dep_*` makes by listing
/// three runtime packages rather than every `devDependency`.
fn python_declared_deps(pyproject: &toml::Value) -> (DeclarationDialect, Vec<(String, String)>) {
    let poetry = pyproject
        .get("tool")
        .and_then(|v| v.get("poetry"))
        .and_then(|v| v.get("dependencies"))
        .and_then(|v| v.as_table())
        .map(poetry_declared_deps)
        .unwrap_or_default();
    let pep621 = pyproject
        .get("project")
        .and_then(|v| v.get("dependencies"))
        .and_then(|v| v.as_array())
        .map(|items| pep621_declared_deps(items.as_slice()))
        .unwrap_or_default();

    // Selection is driven by `[project].dependencies` being non-empty, NOT by
    // the poetry table being empty. An earlier form fell through only when the
    // poetry table yielded nothing, which closed just the override-WITHOUT-
    // version case: a Poetry 2.x override that carries a version
    // (`torch = {version = "^2", source = "pytorch-cpu"}` — documented and
    // common) yields a one-entry vector, so the capture would publish that
    // single key, silently drop the real `[project]` list, and still report
    // "read fine". The real list wins wherever it exists.
    match (pep621.is_empty(), poetry.is_empty()) {
        (false, false) => (DeclarationDialect::Hybrid, drop_name_collisions(pep621)),
        (false, true) => (DeclarationDialect::Pep621, drop_name_collisions(pep621)),
        (true, false) => (DeclarationDialect::Poetry, drop_name_collisions(poetry)),
        // Neither table declared anything. Report the poetry dialect (the one
        // this backend uses) with an empty set rather than inventing a
        // "declares nothing" dialect — `python_dep__source` already
        // distinguishes "the file was read" from "there was no file".
        (true, true) => (DeclarationDialect::Poetry, Vec::new()),
    }
}

/// Drop every declaration whose NORMALIZED name is claimed more than once.
///
/// PEP 503 normalization and marker-stripping both collapse distinct
/// requirements onto one key: `redis>=5; python_version<'3.12'` and
/// `redis>=6; python_version>='3.12'` become two `redis` entries with different
/// constraints, and `put` is a map insert, so last-write-wins would publish one
/// conditional declaration as if it were the unconditional truth. There is no
/// single honest value for such a key, so it gets none — the same call the
/// poetry branch makes for a path dep: no key rather than a fabricated one.
///
/// Input is assumed sorted by name (both producers sort), so collisions are
/// adjacent.
fn drop_name_collisions(mut deps: Vec<(String, String)>) -> Vec<(String, String)> {
    deps.sort();
    deps.dedup();
    let mut out: Vec<(String, String)> = Vec::with_capacity(deps.len());
    let mut i = 0;
    while i < deps.len() {
        let run_end = deps[i..]
            .iter()
            .position(|(n, _)| *n != deps[i].0)
            .map_or(deps.len(), |off| i + off);
        if run_end - i == 1 {
            out.push(deps[i].clone());
        }
        i = run_end;
    }
    out
}

/// `[tool.poetry.dependencies]` → `(normalized name, declared constraint)`.
fn poetry_declared_deps(table: &toml::map::Map<String, toml::Value>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (raw_name, spec) in table {
        if raw_name == "python" {
            continue;
        }
        let constraint = match spec {
            toml::Value::String(s) => Some(s.as_str()),
            toml::Value::Table(t) => t.get("version").and_then(|v| v.as_str()),
            _ => None,
        };
        let (Some(constraint), name) = (constraint, normalize_python_dep_name(raw_name)) else {
            continue;
        };
        if name.is_empty() || constraint.is_empty() {
            continue;
        }
        out.push((name, constraint.to_string()));
    }
    out.sort();
    out
}

/// PEP 621 `[project] dependencies` → `(normalized name, declared constraint)`.
///
/// `dependencies = ["fastapi>=0.136", "redis[hiredis] ~= 5.0"]`. The NAME is
/// everything before the first extras bracket, version specifier, environment
/// marker or whitespace; the rest, minus the extras, is the constraint. A bare
/// requirement has no specifier, and `*` — poetry's own spelling for "any
/// version" — is the honest rendering where an empty string would read as a
/// missing value.
///
/// ## Direct-reference (`@`) requirements are DROPPED
///
/// PEP 508 allows `name @ <url>` — `qontinui-schemas @ file:///D:/...`,
/// `pkg @ git+ssh://git@host/repo`. Three reasons none of them may be
/// published, any one of which is sufficient:
///
/// 1. It is not a version constraint, so it is the same case the poetry branch
///    already refuses: a `path`/`git` dep contributes no key rather than a
///    fabricated one. Publishing it here would make the two dialects disagree
///    about the same dependency.
/// 2. A `file://` URL is a machine-local ABSOLUTE PATH. This module drops those
///    structurally everywhere else; letting one onto the wire through a
///    requirement string is the same leak by a different door, and it would read
///    as permanent drift between any two boxes besides.
/// 3. A `git+ssh://user@host/...` URL can carry USERINFO, and it would reach the
///    section without passing [`sanitize_url`] — the function this file calls
///    the single choke point that guarantees a credential cannot leak. A second
///    path onto the wire that bypasses the choke point defeats the choke point.
fn pep621_declared_deps(items: &[toml::Value]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for req in items.iter().filter_map(|v| v.as_str()) {
        // Environment markers describe WHEN the dep applies, not which version,
        // so they are dropped before splitting.
        let req = req.split(';').next().unwrap_or("").trim();
        // Direct references are dropped whole — see the doc above. Checked
        // BEFORE splitting, because `@` can appear inside the URL too.
        if req.contains('@') {
            continue;
        }
        let split_at = req.find(|c: char| {
            c.is_whitespace() || matches!(c, '[' | '(' | '<' | '>' | '=' | '!' | '~')
        });
        let (raw_name, rest) = match split_at {
            Some(i) => (&req[..i], req[i..].trim()),
            None => (req, ""),
        };
        let name = normalize_python_dep_name(raw_name);
        if name.is_empty() || name == "python" {
            continue;
        }
        // Drop an extras group (`[hiredis]`) — it is not a version.
        let constraint = match rest.strip_prefix('[') {
            Some(after) => after.split(']').nth(1).unwrap_or("").trim(),
            None => rest,
        };
        let constraint = if constraint.is_empty() {
            "*"
        } else {
            constraint
        };
        out.push((name, constraint.to_string()));
    }
    out.sort();
    out
}

/// Resolve Half A from a workspace root. Pure over the filesystem layout (no
/// environment reads), so every rung — including each "read nothing, and here
/// is why" — is testable against a synthetic tree.
fn resolve_python_declared(
    workspace_root: Option<&Path>,
) -> (PythonDepSource, Vec<(String, String)>) {
    let Some(root) = workspace_root else {
        return (PythonDepSource::WorkspaceUnresolved, Vec::new());
    };
    let Some(path) = web_backend_pyproject(root) else {
        return (PythonDepSource::PyprojectAbsent, Vec::new());
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return (PythonDepSource::PyprojectUnreadable, Vec::new());
    };
    let Ok(parsed) = toml::from_str::<toml::Value>(&text) else {
        return (PythonDepSource::PyprojectUnreadable, Vec::new());
    };
    let (dialect, declared) = python_declared_deps(&parsed);
    let source = match dialect {
        DeclarationDialect::Poetry => PythonDepSource::Pyproject,
        DeclarationDialect::Pep621 => PythonDepSource::PyprojectPep621,
        DeclarationDialect::Hybrid => PythonDepSource::PyprojectHybrid,
    };
    (source, declared)
}

/// Write Half A into the section.
fn put_python_declared_deps(section: &mut Section, workspace_root: Option<&Path>) {
    let (source, declared) = resolve_python_declared(workspace_root);
    for (name, constraint) in declared {
        put(section, &format!("python_dep_{name}"), constraint);
    }
    // Written LAST, and unconditionally. Last so the no-forge invariant holds
    // STRUCTURALLY — this write cannot be overwritten by a package-derived key
    // even if normalization were ever weakened — rather than resting on the
    // proof that no package name can produce the `__` infix. Unconditionally
    // because it is what makes an empty result a stated observation rather than
    // an absent one, the same role `repos_scope_kind` plays in
    // [`collect_repos`].
    put(section, PYTHON_DEP_SOURCE_KEY, source.wire());
}

// ---------------------------------------------------------------------------
// Half B — python_installed_*: what is actually INSTALLED (NOT derived)
// ---------------------------------------------------------------------------
//
// ## Which environment this measures, and the guarantee it can give
//
// The interpreter is resolved from the INHERITED PATH. That is a real
// limitation and it is the same defect class [`resolve_probe_scope`] documents
// for the cwd: what gets measured becomes a function of how the runner process
// was launched. A runner started from an activated venv resolves that venv's
// `python`; the same runner restarted from a plain shell resolves the global
// one. The three `--version` shells have always had this sensitivity, but it
// stayed latent because a venv and a global interpreter usually report the same
// `3.12.x`. A sha256 over a whole package set does not stay latent — it differs
// every time — and it lands on the one key family deliberately excluded from
// the derived classification, i.e. the family that drives `in_sync`.
//
// The runner cannot make PATH deterministic without pinning an interpreter
// path, which is exactly the machine-local configuration this module refuses to
// put on the wire. So the guarantee is the [`ProbeScopeKind`] one instead:
// **publish the comparability marker beside the measurement, sourced from the
// same observation, and require consumers to gate on it.**
//
// [`PYTHON_INSTALLED_ENV_KIND_KEY`] is that gate. It is NOT decoration beside
// the digest — it is the statement of whether two digests may be compared at
// all. A consumer that compares digests across differing `env_kind` values is
// reading incomparable numbers, exactly as `apply_versions`'s `scope_mismatch`
// refuses a `versions` apply across differing `probe_scope_kind`.
//
// Crucially the marker comes from the INTERPRETER ITSELF (`sys.prefix !=
// sys.base_prefix`), not from the `VIRTUAL_ENV` environment variable. That
// variable is set by shell ACTIVATION, so it is both false-negative (a runner
// launched with an absolute venv interpreter and no activation) and
// false-positive (an activated shell whose PATH resolution then lands
// elsewhere). Reading it would have described the launching shell while the
// digest described some other interpreter — a marker that can disagree with the
// thing it certifies is worse than none. One probe, one interpreter, both
// facts.
//
// ### Residual, stated because it is not closed
//
// The gate makes the incomparability VISIBLE and refusable; it does not make
// the measurement launch-independent. Two boxes that both resolve a venv report
// `env_kind = venv` and their digests will still be compared, even though they
// are different venvs. Closing that needs a declared interpreter (an operator
// setting, resolved like `scope_root`) — deliberately out of scope here, and
// noted so the next change does not mistake the gate for a full fix.

/// The outcome of a bounded subprocess whose FULL stdout we need.
///
/// [`version_of_within`] cannot serve here: it returns all of stdout's first
/// line only, and Half B needs the whole document. Both need the failure modes
/// kept apart, because each is a different provenance answer and "could not
/// measure" must never render as "measured, found nothing".
///
/// Named `CaptureOutcome`, not `ProbeOutcome`: this module already has a
/// [`ProbeOutcome`], the `--version` probe's three-way verdict, and the two are
/// NOT the same quantity — that one distinguishes "unmeasured" from "absent"
/// for a single version string, this one reports how a full-stdout capture
/// ended. Sharing a name would make the pair unrepresentable in one module and,
/// worse, invite a future reader to treat a `TimedOut` here as the same
/// established fact the version probe earns with a confirming retry.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CaptureOutcome {
    /// The process exited 0. Carries all of stdout.
    Ok(String),
    /// The process could not be started — the command is not on PATH.
    SpawnFailed,
    /// The process ran and exited non-zero.
    NonZeroExit,
    /// The process outlived its budget and was killed.
    TimedOut,
}

/// Run a bounded subprocess and capture its FULL stdout, keeping the failure
/// modes apart.
///
/// ## Why this does NOT reuse [`version_of_within`]'s poll-then-read shape
///
/// That shape is safe for a `--version` probe and unsafe here. It leaves both
/// pipes attached and reads them only AFTER `try_wait` reports exit, so the
/// child must exit before a single byte is drained. A pipe holds 64 KiB
/// (`PIPE_BUFFER_CAPACITY` in std's `child_pipe.rs`; the same order on Linux):
/// once the child fills it, the child blocks in `write`, never exits,
/// `try_wait` never returns `Some`, and the budget expires. The probe then
/// reports [`CaptureOutcome::TimedOut`] — a stall misreported as a slow
/// environment, and worst on the LARGE environments Half B most needs to
/// measure. `version_of_within` escapes this only because a version string is
/// bytes, not kilobytes.
///
/// Two fixes, both load-bearing:
///
/// 1. **stderr is `Stdio::null()`.** Nothing in this function consumes stderr,
///    and an undrained pipe is a deadlock waiting for a noisy child. A python
///    with leftover partial installs emits one `Ignoring invalid distribution`
///    warning per bad dist and can fill 64 KiB on its own.
/// 2. **stdout is drained by a reader thread** that runs concurrently with the
///    wait loop, so the child can never block on a full buffer.
///
/// ## The reader is NEVER joined — it reports over a channel with a deadline
///
/// Joining it would have been a worse bug than the deadlock it replaced.
/// `child.kill()` terminates only the DIRECT child; a grandchild that inherited
/// the stdout handle keeps the write end open, so `read_to_string` never
/// returns and a `join()` blocks with NO bound. On Windows that is the ordinary
/// case rather than an exotic one: pyenv-win installs `python.bat`/`python.exe`
/// shims, the Microsoft Store ships a `python.exe` app-execution alias that
/// re-launches, and conda and other wrapper launchers all interpose a process.
///
/// The blast radius is what makes it unacceptable: [`collect_versions`] is
/// synchronous inside the async `build_envelope`, awaited from the periodic
/// task in `env_agent::mod`. An unbounded block there permanently consumes a
/// tokio worker and the env-capture loop never ticks again — silently, with no
/// log and no `probe_timeout` — recoverable only by restarting the runner,
/// which fleet policy forbids.
///
/// So the reader hands its buffer back over an `mpsc` channel and every read of
/// that channel is a `recv_timeout` against the SAME deadline as the wait loop.
/// On expiry the [`std::thread::JoinHandle`] is simply dropped: the thread
/// detaches and exits on its own when the pipe finally closes.
///
/// Residual, stated: a detached reader can outlive the call while a grandchild
/// holds the pipe. It costs one thread and at most [`STDOUT_CAPTURE_LIMIT`]
/// bytes, and it does NOT block the capture loop, the tokio worker, or the next
/// capture. Reaping it would need a process-tree kill, which fleet policy
/// forbids.
fn run_bounded_capture(
    cmd: &str,
    args: &[&str],
    cwd: Option<&Path>,
    budget: Duration,
) -> CaptureOutcome {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::Instant;

    let started = Instant::now();
    let mut command = crate::process_helpers::no_window(cmd);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // Never piped: see the doc above. An unconsumed pipe is a deadlock.
        .stderr(Stdio::null());
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    let Ok(mut child) = command.spawn() else {
        return CaptureOutcome::SpawnFailed;
    };
    let Some(mut stdout) = child.stdout.take() else {
        // Cannot happen with `Stdio::piped()`, but reporting a failed
        // measurement is the only honest answer if it ever does.
        let _ = child.kill();
        let _ = child.wait();
        return CaptureOutcome::NonZeroExit;
    };

    // Drains CONCURRENTLY with the wait loop below — this is what keeps the
    // child from blocking on a full pipe. The handle is deliberately DROPPED
    // rather than bound to a name: there is no join anywhere in this function,
    // by design (see the doc above).
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut buf = String::new();
        // Capped: an uncapped read is an unbounded allocation inside a GUI
        // process. See `STDOUT_CAPTURE_LIMIT`.
        let _ = stdout.take(STDOUT_CAPTURE_LIMIT).read_to_string(&mut buf);
        // The receiver is gone whenever we already gave up; that is expected.
        let _ = tx.send(buf);
    });

    let deadline = started + budget;
    let remaining = |deadline: Instant| deadline.saturating_duration_since(Instant::now());
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return CaptureOutcome::NonZeroExit;
                }
                // Bounded even on the success path: a fast-exiting shim whose
                // grandchild still holds the pipe is the classic shape, and it
                // reaches HERE, not the timeout arm.
                return match rx.recv_timeout(remaining(deadline)) {
                    Ok(out) => CaptureOutcome::Ok(out),
                    // Exited cleanly but its output never arrived in budget.
                    // No measurement was obtained, so say so.
                    Err(_) => CaptureOutcome::TimedOut,
                };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return CaptureOutcome::TimedOut;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            // A `try_wait` error means we lost track of the child; treat it as
            // a failed measurement rather than an empty one.
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return CaptureOutcome::NonZeroExit;
            }
        }
    }
}

/// The inventory probe: a stdlib-only python script printing ONE compact JSON
/// document carrying both facts Half B needs.
///
/// ## Why not `pip list --format=json`
///
/// Three reasons, in order of weight:
///
/// 1. **It cannot certify its own environment.** The venv marker has to come
///    from the same interpreter that produced the inventory, or the two can
///    disagree — see the section header. `sys.prefix`/`sys.base_prefix` are
///    available only from inside the interpreter.
/// 2. **pip need not be installed.** `importlib.metadata` is stdlib since 3.8,
///    and the backend requires >=3.12, so this reads the same install database
///    pip would with strictly fewer preconditions.
/// 3. **pip is chatty on stderr**, which this probe discards; a quiet probe is
///    easier to reason about than one whose diagnostics vanish.
///
/// Names are emitted raw and normalized on the Rust side, so ONE normalizer
/// ([`normalize_python_dep_name`]) governs both halves and they cannot drift.
///
/// `n not in p` keeps the FIRST distribution seen for a name. `distributions()`
/// walks `sys.path` in order and an `import` resolves to the first match, so
/// first-wins is the version that would actually be imported; last-wins would
/// have recorded the shadowed loser.
///
/// `py` carries `sys.version_info[:2]` from the same invocation, so the
/// interpreter identity cannot disagree with the inventory it labels — the same
/// reason the venv flag is read here rather than from the environment.
const PYTHON_INVENTORY_SCRIPT: &str = "import json,sys\n\
     from importlib.metadata import distributions\n\
     p={}\n\
     for d in distributions():\n\
     \x20   try:\n\
     \x20       n=d.metadata['Name']; v=d.version\n\
     \x20   except Exception:\n\
     \x20       continue\n\
     \x20   if n and v and n not in p: p[n]=v\n\
     json.dump({'venv':sys.prefix!=sys.base_prefix,\
     'py':'%d.%d'%sys.version_info[:2],'packages':p},sys.stdout)\n";

/// HOW the installed inventory was obtained — and, when it was not, why.
///
/// Only [`Self::Measured`] carries a measurement. Every other variant means the
/// count and digest keys are ABSENT, so an unmeasured box can never be read as
/// an empty one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PythonInventoryProbe {
    /// The interpreter ran the inventory script and its output parsed.
    Measured,
    /// The resolved probe scope is not a usable directory, so no probe could be
    /// started there. Distinct from [`Self::PythonAbsent`]: nothing was learned
    /// about python at all, and labelling it "python absent" would assert
    /// something this rung never tested. (`resolve_probe_scope` normally
    /// guarantees an existing directory, so this is a defence-in-depth rung.)
    ScopeUnusable,
    /// No `python` on PATH in the resolved scope — nothing to inventory.
    PythonAbsent,
    /// `python` ran and exited non-zero: no `importlib.metadata`, a broken
    /// install, or a sandbox refusing the read.
    ProbeFailed,
    /// The probe outlived [`PYTHON_INVENTORY_BUDGET`] and was killed. NOT an
    /// empty environment. Since [`run_bounded_capture`] drains stdout
    /// concurrently and discards stderr, this can no longer mean "the child
    /// filled a pipe and stalled" — it means genuinely slow, so a box reporting
    /// it is worth looking at rather than explaining away.
    ProbeTimeout,
    /// The interpreter exited 0 but its output was not the document we can
    /// read. A changed contract, not an empty environment.
    UnparseableOutput,
}

impl PythonInventoryProbe {
    /// The stable wire string carried in `versions.python_installed_probe`.
    pub fn wire(self) -> &'static str {
        match self {
            Self::Measured => "measured",
            Self::ScopeUnusable => "scope_unusable",
            Self::PythonAbsent => "python_absent",
            Self::ProbeFailed => "probe_failed",
            Self::ProbeTimeout => "probe_timeout",
            Self::UnparseableOutput => "unparseable_output",
        }
    }
}

/// WHICH kind of python environment the inventory was taken from — the
/// comparability gate. See the section header: two digests may only be compared
/// when these agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PythonEnvKind {
    /// `sys.prefix != sys.base_prefix` — a virtual environment.
    Venv,
    /// `sys.prefix == sys.base_prefix`. Named `NotVenv`, and emitted as
    /// `not_venv`, because that is ALL it means.
    ///
    /// It was called `system`, which read as "the box's one system python" and
    /// invited exactly the over-reading this gate exists to prevent. A conda
    /// environment is not a venv (`base` and named envs alike satisfy
    /// `sys.prefix == sys.base_prefix`); neither is a pyenv-managed install,
    /// nor a second system python. All of them land here, so this value on its
    /// own certifies far less than a reader would assume — which is why
    /// [`PYTHON_INSTALLED_INTERPRETER_KEY`] is published beside it.
    NotVenv,
    /// Nothing was measured, so the environment was never identified. NOT a
    /// synonym for `not_venv`: claiming a kind we did not observe is the
    /// failure this whole module is built against.
    Unknown,
}

impl PythonEnvKind {
    /// The stable wire string carried in `versions.python_installed_env_kind`.
    pub fn wire(self) -> &'static str {
        match self {
            Self::Venv => "venv",
            Self::NotVenv => "not_venv",
            Self::Unknown => "unknown",
        }
    }
}

/// A resolved installed-package inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PythonInventory {
    probe: PythonInventoryProbe,
    /// The comparability gate. [`PythonEnvKind::Unknown`] whenever nothing was
    /// measured.
    env_kind: PythonEnvKind,
    /// The interpreter's `MAJOR.MINOR`, or `None` when nothing was measured —
    /// never a guess.
    interpreter: Option<String>,
    /// `(digest, package_count)`. `Some` iff an inventory was actually read —
    /// so `Some(_, 0)` is "measured, environment is empty" and `None` is "not
    /// measured".
    measured: Option<(String, usize)>,
}

impl PythonInventory {
    /// An inventory that measured nothing, for the stated `probe` reason.
    fn nothing(probe: PythonInventoryProbe) -> Self {
        Self {
            probe,
            env_kind: PythonEnvKind::Unknown,
            interpreter: None,
            measured: None,
        }
    }
}

/// Parse the inventory script's JSON into `(env kind, normalized sorted set)`.
///
/// Returns `None` when the document is not the object we emit — a changed
/// contract, which must not render as an empty environment. `venv` is required
/// rather than defaulted: defaulting it would invent a comparability verdict.
///
/// Names go through [`normalize_python_dep_name`] so two boxes spelling the
/// same distribution differently (`Pillow` vs `pillow`) do not read as drift.
///
/// ## A normalized-name collision here PICKS; Half A DROPS
///
/// The script already keeps the first distribution per raw name (the one an
/// `import` would resolve to). What can still collide is two DIFFERENT raw
/// names normalizing together — `Foo-Bar` and `foo_bar` both installed — and
/// this keeps the first in sorted order via `or_insert`.
///
/// That is deliberately the opposite of `drop_name_collisions` in Half A, and
/// the asymmetry is not an oversight. Half A publishes one key per declaration,
/// so an ambiguous key would state a falsehood about a specific dependency and
/// is better absent. Half B publishes a DIGEST: dropping an entry would silently
/// change the measured set for every consumer, whereas picking deterministically
/// keeps the digest a stable function of what is installed. Determinism comes
/// from the `BTreeMap` ordering, not from the input order.
fn parse_inventory_json(text: &str) -> Option<PythonInventory> {
    let parsed: Value = serde_json::from_str(text).ok()?;
    let env_kind = match parsed.get("venv")?.as_bool()? {
        true => PythonEnvKind::Venv,
        false => PythonEnvKind::NotVenv,
    };
    // Required, not defaulted: a missing field means the contract changed, and
    // inventing an interpreter identity would forge half the gate.
    let interpreter = parsed.get("py")?.as_str()?.to_string();
    let packages = parsed.get("packages")?.as_object()?;
    let mut seen: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for (name, version) in packages {
        let Some(version) = version.as_str() else {
            continue;
        };
        let normalized = normalize_python_dep_name(name);
        if normalized.is_empty() || version.is_empty() {
            continue;
        }
        // `or_insert`, not `insert`: first-in-sorted-order wins. See the
        // collision note above — this must not depend on iteration order.
        seen.entry(normalized)
            .or_insert_with(|| version.to_string());
    }
    let packages: Vec<(String, String)> = seen.into_iter().collect();
    Some(PythonInventory {
        probe: PythonInventoryProbe::Measured,
        env_kind,
        interpreter: Some(interpreter),
        measured: Some((python_package_set_digest(&packages), packages.len())),
    })
}

/// sha256 over a package set, rendered as lowercase hex.
///
/// Fields are LENGTH-PREFIXED rather than joined with separators. A `name==ver`
/// rendering is ambiguous the moment a version contains the framing bytes: two
/// different sets could hash the same, which on a key whose entire job is "are
/// these identical" is the one bug that cannot be noticed by reading the
/// output. Length prefixes make the encoding injective for any byte content.
///
/// Input order is the `BTreeMap`'s in [`parse_inventory_json`], so the digest is
/// a pure function of the SET and cannot move because the interpreter enumerated
/// site-packages in a different order. Full 64-hex, not truncated.
fn python_package_set_digest(packages: &[(String, String)]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for (name, version) in packages {
        hasher.update((name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update((version.len() as u64).to_le_bytes());
        hasher.update(version.as_bytes());
    }
    hex::encode(hasher.finalize())
}

/// Turn a probe outcome into an inventory. Pure, so every rung — including all
/// the unmeasurable ones — is testable without spawning anything.
fn inventory_from_outcome(outcome: CaptureOutcome) -> PythonInventory {
    match outcome {
        CaptureOutcome::SpawnFailed => PythonInventory::nothing(PythonInventoryProbe::PythonAbsent),
        CaptureOutcome::NonZeroExit => PythonInventory::nothing(PythonInventoryProbe::ProbeFailed),
        CaptureOutcome::TimedOut => PythonInventory::nothing(PythonInventoryProbe::ProbeTimeout),
        CaptureOutcome::Ok(stdout) => parse_inventory_json(&stdout)
            .unwrap_or_else(|| PythonInventory::nothing(PythonInventoryProbe::UnparseableOutput)),
    }
}

/// Write Half B into the section.
///
/// The probe runs in the SAME resolved scope as the `python` version key, so
/// the interpreter inventoried here is the one that key reports. No interpreter
/// path is emitted: it is machine-local and incomparable, the same reason
/// [`ProbeScopeKind`] publishes a kind rather than a path.
fn put_python_installed(section: &mut Section, scope: &ProbeScope) {
    // Guarded rather than left to the spawn: a bad cwd and a missing
    // interpreter both surface as a spawn error, and reporting "python absent"
    // for an unusable directory would assert something never tested.
    let inventory = match scope.root.as_deref() {
        Some(root) if !root.is_dir() => {
            PythonInventory::nothing(PythonInventoryProbe::ScopeUnusable)
        }
        root => inventory_from_outcome(run_bounded_capture(
            "python",
            &["-c", PYTHON_INVENTORY_SCRIPT],
            root,
            PYTHON_INVENTORY_BUDGET,
        )),
    };

    // Written UNCONDITIONALLY: these three are what make an unmeasurable box a
    // stated observation instead of a silent one.
    put(section, PYTHON_INSTALLED_PROBE_KEY, inventory.probe.wire());
    put(section, PYTHON_INSTALLED_SCOPE_KEY, scope.kind.wire());
    put(
        section,
        PYTHON_INSTALLED_ENV_KIND_KEY,
        inventory.env_kind.wire(),
    );

    // Absent rather than guessed when nothing was measured — the same rule the
    // count and digest follow.
    if let Some(interpreter) = inventory.interpreter {
        put(section, PYTHON_INSTALLED_INTERPRETER_KEY, interpreter);
    }
    if let Some((digest, count)) = inventory.measured {
        put(section, PYTHON_INSTALLED_DIGEST_KEY, digest);
        put(section, PYTHON_INSTALLED_COUNT_KEY, count.to_string());
    }
}

/// [`collect_versions`]'s result: the section, plus the keys in it this capture
/// could not MEASURE.
///
/// The unknown set is a PARALLEL per-key set, deliberately not a value-shape
/// change. Every section value is a `Value::String` by envelope contract (see
/// this module's header), and the pull's diff drops any non-string value
/// silently — so promoting a value to `{value, status}` would make the key
/// vanish from the diff entirely, turning a loud bug into a quiet one. A
/// sentinel string is no better: canonical holds `1.82.0`, this box would hold
/// `"<unknown>"`, and the diff yields `Differs`, which is also actionable. It
/// mirrors the server's `derived_keys` set instead, which the plan already knows
/// how to report-but-never-act-on.
#[derive(Debug)]
pub struct VersionsCapture {
    /// The section, exactly as before: string values only, absent keys allowed.
    pub section: Section,
    /// Keys whose probe timed out (`ProbeOutcome::TimedOut`). NOT the keys that
    /// failed — a spawn error or a non-zero exit is a genuine "not installed
    /// here" and must keep classifying as missing, so an apply can still fix it.
    pub unknown_keys: BTreeSet<String>,
}

impl VersionsCapture {
    /// A capture that measured NOTHING — the degrade for a collector that did
    /// not complete at all.
    ///
    /// Deliberately empty on BOTH halves: the empty section is dropped by the
    /// envelope's isolation driver, so the pull sees "no local data for this
    /// section" (un-comparable) rather than a section full of absent keys, and
    /// an empty `unknown_keys` cannot claim we established anything either.
    pub fn empty() -> Self {
        Self {
            section: Section::new(),
            unknown_keys: BTreeSet::new(),
        }
    }
}

/// Collect the `versions` section: this binary's own crate + tauri versions,
/// node deps from this repo's own `package.json` under the workspace root,
/// python from the web-backend pyproject.toml, the `python_dep_*` DECLARED
/// constraints from that same manifest (no lockfile is read, and these are
/// constraints — not pins, and not installed versions; the whole reason Half B
/// exists is that a declaration cannot tell you what is installed), the
/// `python_installed_*` inventory of the live interpreter, plus bounded
/// `--version` shells where the tools resolve. Missing inputs → key absent,
/// never an error — with the stated exception of the two provenance keys
/// `python_dep__source` and `python_installed_probe`, written on every rung
/// precisely so "measured nothing" cannot look like "found no difference".
///
/// A toolchain probe that survives the whole confirmation ladder in
/// [`version_of_confirmed`] without answering still omits its key (the section's
/// value contract has no room to say otherwise) but also RECORDS it in
/// [`VersionsCapture::unknown_keys`], so the consumer can tell "we could not
/// measure this" from "this box does not have it". Only a probe that ran to a
/// definite answer — or a resolved binary that proved itself a stub — puts a key
/// on the missing side of that line.
pub fn collect_versions() -> VersionsCapture {
    let mut section = Section::new();
    let mut unknown_keys = BTreeSet::new();

    put_binary_versions(&mut section);

    // An unresolved workspace root omits the sibling-tree keys rather than
    // guessing at a layout — see the workspace-root bridge above. This section
    // reads only the PATH; the resolution's kind is `repos`' provenance, not
    // this section's (which carries `probe_scope_kind` for the toolchain scope
    // instead — a different quantity, see `collect_repos`).
    let root = workspace_root().into_root();
    if let Some(root) = &root {
        put_sibling_tree_versions(&mut section, root);
    }

    // Half A. NOT inside the `if let` above, deliberately: the `python_dep_*`
    // family carries its own provenance key and that key must be written on
    // EVERY rung — including "the workspace root did not resolve", which is
    // exactly the case where staying silent would let an unread box look like
    // one that agrees with canonical. See the section header above.
    put_python_declared_deps(&mut section, root.as_deref());

    // ---- bounded `--version` shells (optional; see the budget constants) ----
    //
    // These are the ONLY keys in this section that describe the box rather than
    // the source tree the binary was built from, and (post web #808 / runner
    // #818 derived-key filtering) the only ones a P2b apply can move. They all
    // run in the DECLARED scope root so the answer does not depend on how the
    // runner process was launched.
    //
    // The tool list is NOT re-spelled here. `apply_versions::APPLIABLE_TOOLS` is
    // the existing owner of "which keys does this runner treat as observed
    // toolchain versions", and it already supplies both halves this loop needs
    // (`key()` and `command()`). A fourth hardcoded copy would drift, and drift
    // here means a probed tool whose timeout is never recorded as unmeasured —
    // i.e. a silent regression to the very defect this path fixes.
    let scope = probe_scope();
    // Capture PROVENANCE. The three keys below are only comparable across boxes
    // that measured the same KIND of scope; without this the drift oracle
    // silently compares a project tree's toolchain against another box's
    // default one and calls the difference drift. The runner's `versions` apply
    // refuses on a mismatch rather than installing a version that was observed
    // somewhere else. The PATH is deliberately NOT emitted: it differs between
    // any two boxes while meaning the same thing, and it is operator-local.
    put(&mut section, "probe_scope_kind", scope.kind.wire());

    // Half B — the installed-package inventory. Runs in the SAME resolved scope
    // as the three shells below, so the interpreter it inventories is the one
    // the `python` key reports. It publishes its OWN scope key rather than
    // relying on `probe_scope_kind` above, because that one is server-classified
    // derived and an incomparability difference must not be swallowed at `info`
    // — see the section header.
    put_python_installed(&mut section, &scope);

    let scope = scope.root;
    let scope_ref = scope.as_deref();
    for tool in super::apply_versions::APPLIABLE_TOOLS {
        let key = tool.key();
        let outcome = version_of(tool.command(), &["--version"], scope_ref);
        if record_probe_outcome(&mut section, &mut unknown_keys, key, outcome) {
            // WARN rather than debug: a box that routinely lands here is a box
            // whose `versions` drift report is partly blind, and the only other
            // trace of it is the (additive, easily-ignored) envelope field.
            warn!(
                "{}",
                unknown_streak_message(
                    key,
                    record_unknown_streak(key),
                    super::capture_interval_secs()
                )
            );
        } else {
            clear_unknown_streak(key);
        }
    }

    VersionsCapture {
        section,
        unknown_keys,
    }
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

/// Route one probe outcome into the capture, and report whether it landed in the
/// unmeasured set. **Pure — unit-tested.**
///
/// The single writer of both halves, so they can never contradict each other: a
/// key lands in the section (Measured), in `unknown_keys` (TimedOut), or in
/// NEITHER (Failed — genuinely absent, and therefore still installable), and
/// never in both. Extracted so that invariant is testable synthetically, without
/// a box that happens to be slow enough to produce a timeout.
fn record_probe_outcome(
    section: &mut Section,
    unknown_keys: &mut BTreeSet<String>,
    key: &str,
    outcome: ProbeOutcome,
) -> bool {
    match outcome {
        ProbeOutcome::Measured(v) => {
            put(section, key, v);
            false
        }
        // Key still absent from the section — but recorded as UNMEASURED, not
        // as missing.
        ProbeOutcome::TimedOut => {
            unknown_keys.insert(key.to_string());
            true
        }
        // Genuinely not usable here: leave the key absent and say nothing — this
        // is the case the pull is SUPPOSED to classify as missing.
        ProbeOutcome::Failed => false,
    }
}

/// Consecutive captures in which a key came back unmeasured, keyed by section
/// key.
///
/// PROCESS-LOCAL and in-memory: no file, no schema, nothing to migrate or clean
/// up, and a runner restart simply starts counting again. That is the right
/// trade for its only job — turning a per-capture warn that reads identically on
/// its 1st and its 40th occurrence into a signal an operator can act on.
static UNKNOWN_STREAKS: std::sync::Mutex<BTreeMap<String, u32>> =
    std::sync::Mutex::new(BTreeMap::new());

/// Count one more consecutive unmeasured capture for `key` and return the new
/// streak. A warn counter must never be able to take the capture loop down, so
/// it never panics.
///
/// **Poisoning is recovered from, not degraded to a constant.** A `Mutex` stays
/// poisoned for the PROCESS LIFETIME, so an `Err` arm that returned `1` did not
/// degrade one capture — it silently disabled the escalation permanently, and a
/// genuinely wedged probe would then warn in its "the box was busy" wording
/// forever. `into_inner` keeps the counter: the data behind it is a
/// `BTreeMap<String, u32>` that cannot be left in a broken invariant by a panic
/// between two of its own statements.
fn record_unknown_streak(key: &str) -> u32 {
    let mut streaks = UNKNOWN_STREAKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let entry = streaks.entry(key.to_string()).or_insert(0);
    *entry = entry.saturating_add(1);
    *entry
}

/// Reset `key`'s streak — any DEFINITE outcome ends it, measured or failed.
/// Recovers from poisoning for the same reason [`record_unknown_streak`] does:
/// a streak that can never be CLEARED would eventually escalate every key.
fn clear_unknown_streak(key: &str) {
    UNKNOWN_STREAKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(key);
}

/// The warn text for one unmeasured key. **Pure — unit-tested.**
///
/// Past [`UNKNOWN_STREAK_ALARM`] it stops describing a busy box and starts
/// naming a wedged probe, because those are different operator actions: the
/// first resolves itself, the second never will, and a key that is unmeasured on
/// every pass of the capture loop is otherwise reported-and-never-acted-on
/// indefinitely with nothing to distinguish it from the transient case.
///
/// `interval_secs` is the loop's REAL interval (see
/// [`super::capture_interval_secs`]), passed in rather than assumed. This
/// message's entire job is telling an operator "this has been broken long enough
/// to act on", and it used to compute that from a hardcoded 15 minutes — wrong
/// twice over: the interval is configurable and floored at 60s (where it claimed
/// "~60 minutes" for ~3 minutes of elapsed time), and even at the default, N
/// captures span N-1 intervals, so four of them are 45 minutes and not 60.
fn unknown_streak_message(key: &str, streak: u32, interval_secs: u64) -> String {
    let head = format!(
        "env_agent: `{key} --version` exceeded both the {}s capture budget and the {}s confirming \
         retry — reporting it as UNMEASURED rather than missing, so `env apply` will not install \
         over a version it never read",
        CAPTURE_PROBE_BUDGET.as_secs(),
        CAPTURE_PROBE_RETRY_BUDGET.as_secs(),
    );
    if streak < UNKNOWN_STREAK_ALARM {
        format!("{head} (consecutive unmeasured captures: {streak})")
    } else {
        // N captures span N-1 intervals, at THIS runner's interval — not at a
        // hardcoded one.
        let elapsed_minutes = u64::from(streak.saturating_sub(1)) * interval_secs / 60;
        format!(
            "{head}. THIS KEY HAS NOW BEEN UNMEASURED FOR {streak} CONSECUTIVE CAPTURES \
             (~{elapsed_minutes} minutes, at this runner's {interval_secs}s capture interval): it \
             is a WEDGED probe, not a slow one, and it will not resolve on its own. `versions` \
             drift for `{key}` on this box is BLIND until it does — run `{key} --version` by hand \
             in the capture scope and fix or remove the shim. The runner will keep refusing to act \
             on the key in the meantime; it will NOT start treating it as absent, because \
             installing over a version nobody read is the failure this refusal exists to prevent."
        )
    }
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
            let tree = r
                .find_tree(r.index().unwrap().write_tree().unwrap())
                .unwrap();
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

        /// Overwrite the web backend's `pyproject.toml`. Requires
        /// [`Self::with_web_tree`] to have created the directory.
        fn with_backend_pyproject(self, contents: &str) -> Self {
            let backend = self.root.join(WEB_REPO_DIR).join("backend");
            std::fs::write(backend.join("pyproject.toml"), contents).unwrap();
            self
        }
    }

    // ---- python dependency capture: Half A (declared) + Half B (installed) ----

    /// A pyproject in the shape the web backend actually uses: PEP 621 metadata
    /// with `dynamic = ["dependencies"]`, and poetry's own dependency table
    /// carrying the interpreter constraint plus the direct deps — including
    /// both spellings poetry allows (a bare string and an extras table) and a
    /// path dep, which has no version constraint to publish.
    const SYNTHETIC_BACKEND_PYPROJECT: &str = r#"
[project]
name = "backend"
version = "0.1.0"
dynamic = ["dependencies"]

[tool.poetry.dependencies]
python = ">=3.12,<3.15"
fastapi = "^0.136"
python-multipart = ">=0.0.9"
Pillow = "^12"
uvicorn = {extras = ["standard"], version = "^0.38"}
qontinui-schemas = {path = "../../qontinui-schemas", develop = true}

[tool.poetry.group.dev.dependencies]
pytest = "^8.0"
"#;

    fn python_dep_keys(section: &Section) -> Vec<String> {
        section
            .keys()
            .filter(|k| k.starts_with("python_dep_") && !k.starts_with("python_dep__"))
            .cloned()
            .collect()
    }

    /// **Half A is vocabulary parity with `node_dep_*`, which stores the
    /// DECLARED RANGE.** The value must be the constraint as written in the
    /// manifest, never a resolved or installed version: qontinui-web registers
    /// `python_dep_` as a repo-derived prefix, and a derived key is excluded
    /// from the `in_sync` oracle — so shipping installed-version semantics under
    /// this prefix would publish the one thing that matters at a severity that
    /// hides it.
    #[test]
    fn python_dep_keys_carry_the_declared_constraint_like_node_dep() {
        let f = workspace_fixture()
            .with_web_tree()
            .with_backend_pyproject(SYNTHETIC_BACKEND_PYPROJECT);
        let mut section = Section::new();
        put_python_declared_deps(&mut section, Some(&f.root));

        assert_eq!(
            section.get(PYTHON_DEP_SOURCE_KEY).and_then(|v| v.as_str()),
            Some("pyproject"),
        );

        // Declared ranges, verbatim.
        assert_eq!(
            section.get("python_dep_fastapi").and_then(|v| v.as_str()),
            Some("^0.136"),
        );
        // Name normalization: PEP 503 lowercases and collapses separators, so
        // `python-multipart` and `Pillow` land on stable keys.
        assert_eq!(
            section
                .get("python_dep_python_multipart")
                .and_then(|v| v.as_str()),
            Some(">=0.0.9"),
        );
        assert_eq!(
            section.get("python_dep_pillow").and_then(|v| v.as_str()),
            Some("^12"),
        );
        // Poetry's table spelling: the constraint is the `version` field, and
        // the extras list is not a version.
        assert_eq!(
            section.get("python_dep_uvicorn").and_then(|v| v.as_str()),
            Some("^0.38"),
        );

        // A path dep has no comparable constraint — no key rather than a
        // fabricated one.
        assert!(
            !section.contains_key("python_dep_qontinui_schemas"),
            "a path dep has no version constraint to publish"
        );
        // The interpreter constraint is `python_constraint`'s job, not a dep.
        assert!(!section.contains_key("python_dep_python"));
        // Dev group is out of scope, mirroring `node_dep_*`.
        assert!(!section.contains_key("python_dep_pytest"));

        assert_eq!(python_dep_keys(&section).len(), 4);
    }

    /// Half A provenance names the source ACTUALLY read on every rung, and each
    /// "read nothing" rung is a DIFFERENT value — so an unread box is
    /// distinguishable from one whose manifest declares nothing.
    #[test]
    fn python_dep_source_reports_the_source_actually_read() {
        assert_eq!(
            resolve_python_declared(None).0,
            PythonDepSource::WorkspaceUnresolved
        );

        let bare = workspace_fixture();
        assert_eq!(
            resolve_python_declared(Some(&bare.root)).0,
            PythonDepSource::PyprojectAbsent
        );

        let broken = workspace_fixture()
            .with_web_tree()
            .with_backend_pyproject("not { toml [[");
        assert_eq!(
            resolve_python_declared(Some(&broken.root)).0,
            PythonDepSource::PyprojectUnreadable
        );

        let good = workspace_fixture()
            .with_web_tree()
            .with_backend_pyproject(SYNTHETIC_BACKEND_PYPROJECT);
        assert_eq!(
            resolve_python_declared(Some(&good.root)).0,
            PythonDepSource::Pyproject
        );

        // A manifest that resolved but declares nothing beside `python`: the
        // source says `pyproject` and there are simply no dep keys — a
        // different observation from every rung above.
        let empty = workspace_fixture()
            .with_web_tree()
            .with_backend_pyproject("[tool.poetry.dependencies]\npython = \"^3.12\"\n");
        let (source, declared) = resolve_python_declared(Some(&empty.root));
        assert_eq!(source, PythonDepSource::Pyproject);
        assert!(declared.is_empty());

        let wires = [
            PythonDepSource::Pyproject,
            PythonDepSource::PyprojectPep621,
            PythonDepSource::PyprojectHybrid,
            PythonDepSource::PyprojectUnreadable,
            PythonDepSource::PyprojectAbsent,
            PythonDepSource::WorkspaceUnresolved,
        ]
        .map(|s| s.wire());
        let unique: std::collections::BTreeSet<&str> = wires.iter().copied().collect();
        assert_eq!(unique.len(), wires.len(), "wire strings must be distinct");
    }

    /// The scope hazard `workspace_package_json` documents, applied to python:
    /// `python_dep_<name>` carries no repo qualifier, so it must describe
    /// exactly ONE tree — the same tree `python_constraint` reads. A pyproject
    /// anywhere else is never a substitute, and the resolver never climbs out
    /// of the workspace root.
    #[test]
    fn python_dep_scope_is_the_web_backend_and_never_walks() {
        let f = workspace_fixture().with_runner_tree();
        std::fs::write(f.root.join("pyproject.toml"), SYNTHETIC_BACKEND_PYPROJECT).unwrap();
        std::fs::write(
            f.root.join(RUNNER_REPO_DIR).join("pyproject.toml"),
            SYNTHETIC_BACKEND_PYPROJECT,
        )
        .unwrap();

        assert_eq!(
            web_backend_pyproject(&f.root),
            None,
            "only the web backend's own pyproject counts"
        );
        assert_eq!(
            resolve_python_declared(Some(&f.root)).0,
            PythonDepSource::PyprojectAbsent,
            "a decoy pyproject elsewhere must not be adopted"
        );
    }

    /// A backend that migrates off poetry's dependency table to PEP 621 must
    /// degrade to a DIFFERENT key set, never to a silently empty capture that
    /// reports clean. The value stays a declared constraint on that path too,
    /// and the dialect is reported so the two spellings are never conflated.
    #[test]
    fn declared_deps_fall_back_to_pep_621_when_poetry_table_is_absent() {
        let pep621: toml::Value = toml::from_str(
            r#"
[project]
name = "backend"
dependencies = [
  "fastapi>=0.136",
  "redis[hiredis] ~= 5.0",
  "pillow ; python_version >= '3.12'",
  "sqlalchemy",
]
"#,
        )
        .unwrap();
        assert_eq!(
            python_declared_deps(&pep621),
            (
                DeclarationDialect::Pep621,
                vec![
                    ("fastapi".to_string(), ">=0.136".to_string()),
                    // An environment marker says WHEN the dep applies, not
                    // which version, so it is dropped rather than published.
                    ("pillow".to_string(), "*".to_string()),
                    // The extras group is stripped; the specifier survives.
                    ("redis".to_string(), "~= 5.0".to_string()),
                    // No specifier at all — `*` is poetry's own spelling for
                    // "any", honest where an empty string would look like a
                    // missing value.
                    ("sqlalchemy".to_string(), "*".to_string()),
                ]
            )
        );

        // The poetry table is the declaration list only when
        // `[project].dependencies` does not exist — the real backend's shape,
        // where `[project]` says `dynamic = ["dependencies"]` and delegates.
        let pure_poetry: toml::Value = toml::from_str(
            "[project]\ndynamic = [\"dependencies\"]\n\
             [tool.poetry.dependencies]\npython = \"^3.12\"\nfastapi = \"^0.136\"\n",
        )
        .unwrap();
        assert_eq!(
            python_declared_deps(&pure_poetry),
            (
                DeclarationDialect::Poetry,
                vec![("fastapi".to_string(), "^0.136".to_string())]
            )
        );

        // When BOTH tables carry entries, `[project]` is the list and the
        // poetry table is overrides — so the poetry-only entry must NOT be the
        // published set, and the dialect must say a hybrid was read.
        let both: toml::Value = toml::from_str(
            "[project]\ndependencies = [\"wrong-one\"]\n\
             [tool.poetry.dependencies]\npython = \"^3.12\"\nfastapi = \"^0.136\"\n",
        )
        .unwrap();
        assert_eq!(
            python_declared_deps(&both),
            (
                DeclarationDialect::Hybrid,
                vec![("wrong_one".to_string(), "*".to_string())]
            ),
            "`[project].dependencies` is the list wherever it exists"
        );

        // …but an override-only poetry table must FALL THROUGH to PEP 621, not
        // report "read fine, declares nothing". Poetry 2.x's hybrid layout
        // keeps the poetry table for source/constraint overrides while the real
        // list lives in `[project]`; returning the empty subset there would be
        // silence reading as success one layer down.
        let hybrid: toml::Value = toml::from_str(
            "[project]\ndependencies = [\"fastapi>=0.136\", \"redis>=5\"]\n\
             [tool.poetry.dependencies]\npython = \"^3.12\"\n",
        )
        .unwrap();
        assert_eq!(
            python_declared_deps(&hybrid),
            (
                DeclarationDialect::Pep621,
                vec![
                    ("fastapi".to_string(), ">=0.136".to_string()),
                    ("redis".to_string(), ">=5".to_string()),
                ]
            ),
            "an override-only poetry table must not hide the real declaration list"
        );

        // A Poetry 2.x override that CARRIES a version is the case the
        // empty-only fall-through missed: it yields a non-empty poetry vector,
        // so preferring the poetry table would publish that one key and
        // silently drop the real `[project]` list while still reporting "read
        // fine". `[project].dependencies` wins wherever it exists, and the
        // hybrid is reported as its own dialect.
        let hybrid_with_version: toml::Value = toml::from_str(
            "[project]\ndependencies = [\"fastapi>=0.136\", \"torch>=2\"]\n\
             [tool.poetry.dependencies]\ntorch = {version = \"^2\", source = \"cpu\"}\n",
        )
        .unwrap();
        assert_eq!(
            python_declared_deps(&hybrid_with_version),
            (
                DeclarationDialect::Hybrid,
                vec![
                    ("fastapi".to_string(), ">=0.136".to_string()),
                    ("torch".to_string(), ">=2".to_string()),
                ]
            ),
            "a versioned override must not shrink the capture to itself"
        );

        // PEP 508 direct references are DROPPED whole. They are not version
        // constraints (the poetry branch already refuses the equivalent), a
        // `file://` URL is a machine-local absolute path, and a `git+ssh://`
        // URL would put userinfo on the wire without ever passing
        // `sanitize_url` — the file's single credential choke point.
        let direct_refs: toml::Value = toml::from_str(
            "[project]\ndependencies = [\
             \"qontinui-schemas @ file:///D:/qontinui-root/qontinui-schemas\", \
             \"secretpkg @ git+ssh://git:hunter2@example.com/o/r.git\", \
             \"fastapi>=0.136\"]\n",
        )
        .unwrap();
        let (dialect, deps) = python_declared_deps(&direct_refs);
        assert_eq!(dialect, DeclarationDialect::Pep621);
        assert_eq!(
            deps,
            vec![("fastapi".to_string(), ">=0.136".to_string())],
            "direct-reference requirements must contribute no key at all"
        );
        let rendered = format!("{deps:?}");
        assert!(
            !rendered.contains("qontinui-root"),
            "machine-local path leaked: {rendered}"
        );
        assert!(
            !rendered.contains("hunter2"),
            "credential leaked: {rendered}"
        );

        // Two requirements collapsing onto ONE normalized key (markers are
        // stripped) publish NOTHING for that key: there is no honest single
        // value, and `put` is a map insert, so last-write-wins would present a
        // conditional declaration as the unconditional truth.
        let collide: toml::Value = toml::from_str(
            "[project]\ndependencies = [\
             \"redis>=5 ; python_version<'3.12'\", \
             \"redis>=6 ; python_version>='3.12'\", \
             \"fastapi>=0.136\"]\n",
        )
        .unwrap();
        assert_eq!(
            python_declared_deps(&collide).1,
            vec![("fastapi".to_string(), ">=0.136".to_string())],
            "a normalized-name collision must drop the key, not pick a winner"
        );
    }

    /// **Half B is the half that closes the gap, and it must not be swallowed.**
    /// qontinui-web classifies derived keys by `str.startswith("python_dep_")`;
    /// any installed-inventory key under that prefix would be scored `info` and
    /// excluded from `in_sync`, which is the exact failure this half exists to
    /// prevent.
    ///
    /// Asserted BEHAVIOURALLY, on the keys actually emitted into a section
    /// rather than on the constants. Checking the constants would pass for
    /// someone who wrote `put(section, "python_dep_installed_count", …)` with a
    /// literal — breaking the highest-stakes invariant in this change while the
    /// test named after it stayed green.
    #[test]
    fn installed_keys_use_a_prefix_the_derived_rule_cannot_swallow() {
        let f = workspace_fixture()
            .with_web_tree()
            .with_backend_pyproject(SYNTHETIC_BACKEND_PYPROJECT);
        let mut section = Section::new();
        put_python_declared_deps(&mut section, Some(&f.root));
        put_python_installed(&mut section, &unusable_scope(ProbeScopeKind::Default));

        // Everything Half B emitted must be OUTSIDE the derived prefix.
        let installed: Vec<&String> = section
            .keys()
            .filter(|k| k.starts_with("python_installed_"))
            .collect();
        assert!(
            !installed.is_empty(),
            "Half B must emit keys even when it cannot measure"
        );
        for key in &installed {
            assert!(
                !key.starts_with("python_dep_"),
                "{key} would be classified repo-derived and hidden from in_sync"
            );
        }

        // And the converse: EVERY key under the derived prefix is either Half
        // A's provenance marker or one of the declarations Half A resolved.
        // Nothing else may hide there — an installed-inventory key smuggled
        // under this prefix is the failure this test exists to catch.
        let declared = resolve_python_declared(Some(&f.root)).1;
        for key in section.keys().filter(|k| k.starts_with("python_dep_")) {
            if key == PYTHON_DEP_SOURCE_KEY {
                continue;
            }
            let name = key.strip_prefix("python_dep_").unwrap();
            assert!(
                declared.iter().any(|(n, _)| n == name),
                "{key} is under the derived prefix but is not a resolved declaration"
            );
        }

        // Half A's meta key is the opposite case: it MUST share the derived
        // prefix so ONE server registration covers it, and no package name can
        // forge it.
        assert!(PYTHON_DEP_SOURCE_KEY.starts_with("python_dep__"));
        assert!(section.contains_key(PYTHON_DEP_SOURCE_KEY));
        for raw in [
            "source",
            "lock-digest",
            "lock__digest",
            "lock..digest",
            "LOCK_-_DIGEST",
            "_source",
            "source_",
        ] {
            let key = format!("python_dep_{}", normalize_python_dep_name(raw));
            assert!(
                !key.starts_with("python_dep__"),
                "package name {raw:?} forged a meta key: {key}"
            );
        }
        assert_eq!(normalize_python_dep_name("Pillow"), "pillow");
        assert_eq!(
            normalize_python_dep_name("opencv-python.headless"),
            "opencv_python_headless"
        );
        assert_eq!(normalize_python_dep_name("--"), "");
    }

    /// A scope whose root cannot be used, so Half B exercises an unmeasurable
    /// rung deterministically instead of whatever python this box happens to
    /// have.
    fn unusable_scope(kind: ProbeScopeKind) -> ProbeScope {
        ProbeScope {
            root: Some(
                std::env::temp_dir()
                    .join("qontinui_no_such_scope_root")
                    .join(std::process::id().to_string()),
            ),
            rejected: None,
            kind,
        }
    }

    /// Half B reads a real installed inventory: the script's document produces a
    /// digest over the whole set at a FIXED key cost, and the digest moves for
    /// any package difference — which is what makes a box whose environment
    /// differs report drifted rather than clean.
    #[test]
    fn installed_inventory_digests_the_whole_environment_at_fixed_cost() {
        let doc = r#"{"venv": false, "py": "3.12", "packages":
            {"requests": "2.32.3", "Pillow": "12.1.0", "fastapi": "0.136.2"}}"#;
        let inventory = inventory_from_outcome(CaptureOutcome::Ok(doc.to_string()));
        assert_eq!(inventory.probe, PythonInventoryProbe::Measured);
        assert_eq!(inventory.env_kind, PythonEnvKind::NotVenv);
        let (digest, count) = inventory.measured.clone().expect("a parsed doc measures");
        assert_eq!(count, 3);
        assert_eq!(digest.len(), 64, "full sha256 hex, got {digest}");
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));

        // Order- and case-independent: the digest is a function of the SET.
        let reordered = r#"{"venv": false, "py": "3.12", "packages":
            {"fastapi": "0.136.2", "pillow": "12.1.0", "requests": "2.32.3"}}"#;
        assert_eq!(
            inventory_from_outcome(CaptureOutcome::Ok(reordered.to_string())).measured,
            inventory.measured,
            "case + order differences are not environment differences"
        );

        // ONE package at a different version moves it — the exit condition.
        let bumped = doc.replace("2.32.3", "2.32.4");
        assert_ne!(
            inventory_from_outcome(CaptureOutcome::Ok(bumped)).measured,
            inventory.measured,
            "a differing installed package must not read as clean"
        );
        // …and so does one extra package.
        let extra = r#"{"venv": false, "py": "3.12", "packages":
            {"requests": "2.32.3", "Pillow": "12.1.0", "fastapi": "0.136.2",
             "numpy": "2.3.0"}}"#;
        assert_ne!(
            inventory_from_outcome(CaptureOutcome::Ok(extra.to_string())).measured,
            inventory.measured,
        );

        // The digest framing is INJECTIVE. With `name==version` joining, a
        // version carrying the framing bytes could make two different sets hash
        // identically; length-prefixing makes that impossible.
        let a = [("a".to_string(), "1\nb==2".to_string())];
        let b = [
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
        ];
        assert_ne!(
            python_package_set_digest(&a),
            python_package_set_digest(&b),
            "digest framing must be unambiguous"
        );
    }

    /// The comparability GATE. The interpreter comes off the inherited PATH, so
    /// a venv and a system interpreter on ONE box produce different digests;
    /// `python_installed_env_kind` is what makes that refusable instead of
    /// silently comparable. It must come from the interpreter itself, so it can
    /// never disagree with the digest it certifies, and it must be `unknown` —
    /// never `system` — when nothing was measured.
    #[test]
    fn env_kind_gates_digest_comparability_and_is_never_assumed() {
        let venv = inventory_from_outcome(CaptureOutcome::Ok(
            r#"{"venv": true, "py": "3.12", "packages": {"fastapi": "0.136.2"}}"#.to_string(),
        ));
        let system = inventory_from_outcome(CaptureOutcome::Ok(
            r#"{"venv": false, "py": "3.12", "packages": {"fastapi": "0.136.2"}}"#.to_string(),
        ));
        assert_eq!(venv.env_kind, PythonEnvKind::Venv);
        assert_eq!(system.env_kind, PythonEnvKind::NotVenv);
        // Same packages: the digest CANNOT distinguish them, which is exactly
        // why the gate has to be a separate published key.
        assert_eq!(venv.measured, system.measured);
        assert_ne!(venv.env_kind.wire(), system.env_kind.wire());

        // A document missing EITHER half of the gate is UNPARSEABLE, not
        // "assume not_venv" / "assume some version". Defaulting either would
        // invent a comparability verdict.
        for missing in [
            r#"{"py": "3.12", "packages": {"fastapi": "0.136.2"}}"#,
            r#"{"venv": false, "packages": {"fastapi": "0.136.2"}}"#,
        ] {
            assert_eq!(
                inventory_from_outcome(CaptureOutcome::Ok(missing.to_string())).probe,
                PythonInventoryProbe::UnparseableOutput,
                "for {missing}",
            );
        }

        // `not_venv` is a WEAK statement — it means only "not a venv", and
        // conda/pyenv/second-system pythons all land there. The interpreter
        // version is the second half of the gate, and it comes from the same
        // invocation so it cannot disagree with the digest it labels.
        assert_eq!(venv.interpreter.as_deref(), Some("3.12"));
        let other_minor = inventory_from_outcome(CaptureOutcome::Ok(
            r#"{"venv": false, "py": "3.13", "packages": {"fastapi": "0.136.2"}}"#.to_string(),
        ));
        assert_eq!(other_minor.env_kind, system.env_kind);
        assert_eq!(other_minor.measured, system.measured);
        assert_ne!(
            other_minor.interpreter, system.interpreter,
            "two minors with identical packages must not pass the gate as comparable"
        );

        // Nothing measured => no interpreter claim at all.
        assert!(inventory_from_outcome(CaptureOutcome::SpawnFailed)
            .interpreter
            .is_none());

        // Nothing measured => `unknown`, on every unmeasurable rung.
        for outcome in [
            CaptureOutcome::SpawnFailed,
            CaptureOutcome::NonZeroExit,
            CaptureOutcome::TimedOut,
            CaptureOutcome::Ok("not json".to_string()),
        ] {
            assert_eq!(
                inventory_from_outcome(outcome.clone()).env_kind,
                PythonEnvKind::Unknown,
                "for {outcome:?}: an unmeasured environment has no kind"
            );
        }
        let wires = [
            PythonEnvKind::Venv,
            PythonEnvKind::NotVenv,
            PythonEnvKind::Unknown,
        ]
        .map(|k| k.wire());
        let unique: std::collections::BTreeSet<&str> = wires.iter().copied().collect();
        assert_eq!(unique.len(), wires.len());
    }

    /// **The negative case — the one that matters more.** "Measured, and the
    /// environment is empty" must be distinguishable on the wire from "could
    /// not measure", or a box that was never measured reads exactly like a box
    /// that agrees with canonical.
    #[test]
    fn unmeasurable_environment_is_distinguishable_from_a_measured_empty_one() {
        // Measured, empty: the probe names the method, the count STATES zero,
        // and a digest (of the empty set) exists.
        let measured = inventory_from_outcome(CaptureOutcome::Ok(
            r#"{"venv":false,"py":"3.12","packages":{}}"#.to_string(),
        ));
        assert_eq!(measured.probe, PythonInventoryProbe::Measured);
        let (empty_digest, count) = measured.measured.clone().expect("an empty set is measured");
        assert_eq!(count, 0, "a parsed-but-empty environment must STATE zero");
        assert_eq!(empty_digest, python_package_set_digest(&[]));

        // Every unmeasurable rung: a DIFFERENT reason, and NO count and NO
        // digest — so it can never be misread as zero packages.
        for (outcome, expected) in [
            (
                CaptureOutcome::SpawnFailed,
                PythonInventoryProbe::PythonAbsent,
            ),
            (
                CaptureOutcome::NonZeroExit,
                PythonInventoryProbe::ProbeFailed,
            ),
            (CaptureOutcome::TimedOut, PythonInventoryProbe::ProbeTimeout),
            // Exited 0, but the output is not the contract we can read. A
            // changed interpreter script, NOT an empty environment.
            (
                CaptureOutcome::Ok("Package  Version\n-------  -------".to_string()),
                PythonInventoryProbe::UnparseableOutput,
            ),
            (
                CaptureOutcome::Ok("[\"not\", \"an object\"]".to_string()),
                PythonInventoryProbe::UnparseableOutput,
            ),
        ] {
            let inventory = inventory_from_outcome(outcome.clone());
            assert_eq!(inventory.probe, expected, "for {outcome:?}");
            assert!(
                inventory.measured.is_none(),
                "{expected:?} must publish neither a count nor a digest"
            );
            assert_ne!(
                inventory.probe.wire(),
                measured.probe.wire(),
                "an unmeasurable box must not share the measured box's provenance"
            );
        }

        let wires = [
            PythonInventoryProbe::Measured,
            PythonInventoryProbe::ScopeUnusable,
            PythonInventoryProbe::PythonAbsent,
            PythonInventoryProbe::ProbeFailed,
            PythonInventoryProbe::ProbeTimeout,
            PythonInventoryProbe::UnparseableOutput,
        ]
        .map(|p| p.wire());
        let unique: std::collections::BTreeSet<&str> = wires.iter().copied().collect();
        assert_eq!(unique.len(), wires.len(), "wire strings must be distinct");
    }

    /// **A child that outruns the pipe buffer must not stall the probe.**
    ///
    /// `run_bounded_capture` used to leave both pipes attached and read stdout
    /// only after `try_wait` reported exit. A pipe holds 64 KiB, so a child
    /// writing past that blocked in `write`, never exited, and the probe
    /// reported `probe_timeout` — degrading on exactly the large environments
    /// Half B most needs to measure. Nothing caught it because no test ever
    /// produced more than a version string.
    ///
    /// This drives a REAL child well past the buffer, so a regression to
    /// poll-then-read fails here rather than in production.
    #[test]
    fn bounded_capture_survives_a_child_that_outruns_the_pipe_buffer() {
        let dir = std::env::temp_dir().join(format!(
            "qontinui_probe_flood_{}_{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let payload = dir.join("payload.txt");
        // 256 KiB — four times the 64 KiB pipe capacity, so the old shape
        // deadlocks rather than merely coming close.
        let line = "x".repeat(255);
        let body: String = std::iter::repeat(line.as_str())
            .take(1024)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&payload, &body).unwrap();
        let payload_str = payload.to_string_lossy().to_string();

        #[cfg(windows)]
        let (cmd, args) = ("cmd", vec!["/c", "type", payload_str.as_str()]);
        #[cfg(not(windows))]
        let (cmd, args) = ("cat", vec![payload_str.as_str()]);

        // A budget short enough that a genuine deadlock cannot masquerade as a
        // slow box: copying 256 KiB is milliseconds' work.
        let outcome = run_bounded_capture(cmd, &args, None, Duration::from_secs(20));
        let _ = std::fs::remove_dir_all(&dir);

        match outcome {
            CaptureOutcome::Ok(out) => assert!(
                out.len() >= 200_000,
                "stdout was truncated: got {} bytes, expected the whole payload",
                out.len()
            ),
            other => panic!(
                "a child writing 256 KiB must complete, not {other:?} — \
                 stdout is not being drained concurrently"
            ),
        }
    }

    /// Half B publishes its own scope kind rather than leaning on
    /// `probe_scope_kind`, which the server classifies derived — a difference
    /// there is scored `info` and would let two boxes that measured
    /// incomparable scopes still read as agreeing. The three unconditional keys
    /// are written on every rung, including the unmeasurable ones.
    #[test]
    fn installed_capture_always_states_provenance_scope_and_env_kind() {
        for kind in [
            ProbeScopeKind::Declared,
            ProbeScopeKind::Default,
            ProbeScopeKind::Inherited,
        ] {
            let scope = unusable_scope(kind);
            let mut section = Section::new();
            put_python_installed(&mut section, &scope);

            assert_eq!(
                section
                    .get(PYTHON_INSTALLED_SCOPE_KEY)
                    .and_then(|v| v.as_str()),
                Some(kind.wire()),
                "the scope kind must be stated, not inherited from probe_scope_kind"
            );
            // An unusable scope is reported AS SUCH — not as "python absent",
            // which would assert something this rung never tested.
            assert_eq!(
                section
                    .get(PYTHON_INSTALLED_PROBE_KEY)
                    .and_then(|v| v.as_str()),
                Some("scope_unusable"),
            );
            assert_eq!(
                section
                    .get(PYTHON_INSTALLED_ENV_KIND_KEY)
                    .and_then(|v| v.as_str()),
                Some("unknown"),
                "an unmeasured environment must not be labelled `not_venv`"
            );
            assert!(
                !section.contains_key(PYTHON_INSTALLED_INTERPRETER_KEY),
                "an unmeasured environment must not claim an interpreter"
            );
            // Nothing measured, so neither measurement key may appear.
            assert!(!section.contains_key(PYTHON_INSTALLED_COUNT_KEY));
            assert!(!section.contains_key(PYTHON_INSTALLED_DIGEST_KEY));
            // Nothing here may leak a machine-local path.
            let json = serde_json::to_string(&section).unwrap();
            assert!(
                !json.contains("qontinui_no_such_scope_root"),
                "a probe path must never reach the wire: {json}"
            );
        }
    }

    /// Both provenance keys are present on a REAL capture no matter how this
    /// box resolves — the whole point of hoisting Half A out of the `if let`
    /// and writing Half B's three keys unconditionally.
    #[test]
    fn collect_versions_always_publishes_both_provenance_keys() {
        // `collect_versions` now returns a `VersionsCapture`; this test asserts
        // over the section half only — the unknown set is covered separately.
        let section = collect_versions().section;

        let source = section
            .get(PYTHON_DEP_SOURCE_KEY)
            .and_then(|v| v.as_str())
            .expect("python_dep__source must be present on every capture");
        assert!(
            [
                "pyproject",
                "pyproject_pep621",
                "pyproject_hybrid",
                "pyproject_unreadable",
                "pyproject_absent",
                "workspace_unresolved",
            ]
            .contains(&source),
            "unknown Half A provenance {source}"
        );

        let probe = section
            .get(PYTHON_INSTALLED_PROBE_KEY)
            .and_then(|v| v.as_str())
            .expect("python_installed_probe must be present on every capture");
        assert!(
            [
                "measured",
                "scope_unusable",
                "python_absent",
                "probe_failed",
                "probe_timeout",
                "unparseable_output",
            ]
            .contains(&probe),
            "unknown Half B provenance {probe}"
        );

        // Measurement keys track the probe verdict exactly: present iff
        // measured. This is the invariant that keeps "not measured" from ever
        // rendering as an agreeing box.
        let measured = probe == "measured";
        assert_eq!(section.contains_key(PYTHON_INSTALLED_COUNT_KEY), measured);
        assert_eq!(section.contains_key(PYTHON_INSTALLED_DIGEST_KEY), measured);
        assert!(section.contains_key(PYTHON_INSTALLED_SCOPE_KEY));

        // The comparability gate is always stated, and is only a real kind when
        // something was actually measured.
        let env_kind = section
            .get(PYTHON_INSTALLED_ENV_KIND_KEY)
            .and_then(|v| v.as_str())
            .expect("the comparability gate must be present on every capture");
        assert!(["venv", "not_venv", "unknown"].contains(&env_kind));
        assert_eq!(env_kind != "unknown", measured);
        assert_eq!(
            section.contains_key(PYTHON_INSTALLED_INTERPRETER_KEY),
            measured,
            "the interpreter half of the gate tracks the probe verdict too"
        );
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
        version_of_within(cmd, &args, cwd, TEST_PROBE_BUDGET).into_version()
    }

    /// The whole distinction, at the source: a probe that CANNOT run is
    /// [`ProbeOutcome::Failed`], a probe that runs too long is
    /// [`ProbeOutcome::TimedOut`]. Collapsing them (as the old `Option` return
    /// did) is what let a busy box read as a box missing its toolchain.
    #[test]
    fn probe_outcome_separates_a_missing_tool_from_a_slow_one() {
        // Spawn failure — no such executable anywhere on PATH.
        assert_eq!(
            version_of_within(
                "qontinui_no_such_binary_probe_test",
                &["--version"],
                None,
                TEST_PROBE_BUDGET,
            ),
            ProbeOutcome::Failed,
            "a tool that cannot be spawned must stay FAILED (= genuinely absent)"
        );

        // Non-zero exit — the tool ran and said no.
        #[cfg(windows)]
        let (cmd, args) = ("cmd", ["/c", "exit 3"]);
        #[cfg(not(windows))]
        let (cmd, args) = ("sh", ["-c", "exit 3"]);
        assert_eq!(
            version_of_within(cmd, &args, None, TEST_PROBE_BUDGET),
            ProbeOutcome::Failed,
            "a non-zero exit must stay FAILED, not become UNKNOWN"
        );

        // Over budget — the child was alive at the deadline and got killed.
        #[cfg(windows)]
        let (slow_cmd, slow_args) = ("cmd", ["/c", "ping -n 6 127.0.0.1 >nul"]);
        #[cfg(not(windows))]
        let (slow_cmd, slow_args) = ("sh", ["-c", "sleep 5"]);
        assert_eq!(
            version_of_within(slow_cmd, &slow_args, None, Duration::from_millis(300)),
            ProbeOutcome::TimedOut,
            "a probe killed at its deadline must be TIMED OUT, never FAILED"
        );

        // And a healthy probe still measures.
        #[cfg(windows)]
        let (ok_cmd, ok_args) = ("cmd", ["/c", "echo 1.2.3"]);
        #[cfg(not(windows))]
        let (ok_cmd, ok_args) = ("sh", ["-c", "echo 1.2.3"]);
        assert_eq!(
            version_of_within(ok_cmd, &ok_args, None, TEST_PROBE_BUDGET),
            ProbeOutcome::Measured("1.2.3".to_string()),
        );
    }

    /// `into_version` is the collapse the APPLY's re-probe uses — it must lose
    /// the distinction on purpose, and only there.
    #[test]
    fn probe_outcome_into_version_collapses_both_failure_arms() {
        assert_eq!(
            ProbeOutcome::Measured("1.82.0".to_string()).into_version(),
            Some("1.82.0".to_string())
        );
        assert_eq!(ProbeOutcome::TimedOut.into_version(), None);
        assert_eq!(ProbeOutcome::Failed.into_version(), None);
    }

    /// The capture budget is a PRODUCTION latency choice and this branch does
    /// not move it — only the representation of exceeding it changed. Pinned so
    /// a future "just make it bigger" cannot land unnoticed: capture runs on a
    /// periodic loop against three tools — an interval an operator can set as
    /// low as 60s — so a wedged shim must cost seconds.
    #[test]
    fn capture_probe_budget_is_unchanged_at_three_seconds() {
        assert_eq!(CAPTURE_PROBE_BUDGET, Duration::from_secs(3));
        assert!(
            TEST_PROBE_BUDGET > CAPTURE_PROBE_BUDGET,
            "the generous TEST budget must never be the one the capture path uses"
        );
        // The retry does not relax the first budget — it composes with it, and
        // the COMPOSITION is what each tick actually pays. Pinned as a ceiling
        // on the whole section: at most one retry per key, over the keys
        // `APPLIABLE_TOOLS` owns. 39s is 65% of the 60s interval floor, which is
        // why this ceiling is pinned at all and why the caller runs it on the
        // blocking pool rather than a runtime worker.
        let worst_case = (CAPTURE_PROBE_BUDGET + CAPTURE_PROBE_RETRY_BUDGET)
            * u32::try_from(super::super::apply_versions::APPLIABLE_TOOLS.len()).unwrap();
        assert!(
            worst_case <= Duration::from_secs(60),
            "a whole `versions` capture must stay seconds, not minutes; got {worst_case:?}"
        );
    }

    /// Every key reported unmeasured is genuinely ABSENT from the section, and
    /// the section keeps its string-only value contract — the two halves can
    /// never contradict each other.
    ///
    /// Constructed SYNTHETICALLY, from the one function that writes both halves.
    /// The previous form called `collect_versions()` and looped over
    /// `capture.unknown_keys`, which is empty on any healthy box: the loop body
    /// never ran, so the test asserted nothing about the feature while still
    /// spawning three real subprocesses at the PRODUCTION budget — exactly what
    /// `version_of_within`'s own doc says tests must not do. A test that passes
    /// green without exercising its feature is worse than no test.
    #[test]
    fn a_probe_outcome_lands_in_the_section_or_the_unknown_set_but_never_both() {
        let mut section = Section::new();
        let mut unknown = BTreeSet::new();

        assert!(
            !record_probe_outcome(
                &mut section,
                &mut unknown,
                "node",
                ProbeOutcome::Measured("v22.1.0".to_string())
            ),
            "a measured probe is not unmeasured"
        );
        assert!(
            record_probe_outcome(&mut section, &mut unknown, "rustc", ProbeOutcome::TimedOut),
            "a timed-out probe IS the unmeasured case"
        );
        assert!(
            !record_probe_outcome(&mut section, &mut unknown, "python", ProbeOutcome::Failed),
            "a failed probe is genuinely absent, not unmeasured"
        );

        // Measured → section only.
        assert_eq!(section.get("node").and_then(Value::as_str), Some("v22.1.0"));
        assert!(!unknown.contains("node"));
        // Timed out → unknown set only. THE invariant every consumer leans on:
        // an unmeasured key diffs as `Missing`, and `compute_plan` re-derives
        // that from the section rather than trusting the set.
        assert!(unknown.contains("rustc"));
        assert!(!section.contains_key("rustc"));
        // Failed → neither, so it still classifies as missing and stays
        // installable.
        assert!(!unknown.contains("python"));
        assert!(!section.contains_key("python"));

        assert!(
            section.values().all(Value::is_string),
            "envelope contract: every section value is a string"
        );
        assert!(
            unknown.iter().all(|k| !section.contains_key(k)),
            "no key may be reported unmeasured while carrying a value"
        );
    }

    /// A shim that writes more than a pipe buffer before exiting must classify
    /// on its EXIT, not on the clock. Reading the pipes only after `try_wait`
    /// saw an exit deadlocked against exactly this: the child blocks in `write`,
    /// never exits, and the probe reports "we could not measure this box's
    /// toolchain" about a tool that answered at length.
    #[test]
    fn a_chatty_probe_is_measured_not_timed_out() {
        // ~40 KB of banner on stdout — an order of magnitude past the ~4 KB
        // Windows pipe buffer — with the version line FIRST, the way a real
        // shim's `--version` output starts.
        #[cfg(windows)]
        let (cmd, args) = (
            "cmd",
            [
                "/c",
                "echo 1.2.3 & for /L %i in (1,1,900) do @echo \
                 xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
            ],
        );
        #[cfg(not(windows))]
        let (cmd, args) = (
            "sh",
            [
                "-c",
                "echo 1.2.3; i=0; while [ $i -lt 900 ]; do echo \
                 xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; i=$((i+1)); done",
            ],
        );
        assert_eq!(
            version_of_within(cmd, &args, None, TEST_PROBE_BUDGET),
            ProbeOutcome::Measured("1.2.3".to_string()),
            "a chatty-but-healthy shim must be MEASURED — a blocked pipe is a deadlock, not a \
             slow tool"
        );
    }

    /// Build a [`FileShape`] for the classifier tests. Named fields at the call
    /// site would drown the one bit each case is about.
    fn shape(
        len: u64,
        reparse_tag: Option<u32>,
        is_reparse_point: bool,
        under_windows_apps: bool,
    ) -> FileShape {
        FileShape {
            is_dir: false,
            len,
            reparse_tag,
            is_reparse_point,
            under_windows_apps,
        }
    }

    /// THE regression: **a 0-byte file is not evidence of absence.** On NTFS a
    /// symlink is a 0-byte reparse point, so the old unconditional
    /// `md.len() == 0 → stub` arm condemned every symlinked exe — including
    /// every rustup proxy on this fleet (`~/.cargo/bin/rustc.exe` → `rustup.exe`,
    /// measured at 0 bytes). A timed-out probe on a busy box then read as
    /// "rustc is absent" and `env apply` would install over a working toolchain.
    ///
    /// Runs over synthetic shapes on purpose: an App Execution Alias cannot be
    /// forged in a test at all, and a symlink needs privileges this box may
    /// lack, so the SHAPES are what the classifier is pinned against.
    #[test]
    fn stub_verdict_condemns_the_alias_and_never_the_symlink() {
        // A symlink says nothing itself — follow it and judge the target.
        assert_eq!(
            stub_verdict(&shape(0, Some(IO_REPARSE_TAG_SYMLINK), true, false)),
            StubVerdict::FollowLink,
            "a 0-byte symlink is the rustup-proxy shape; condemning it installs over a live \
             toolchain"
        );
        assert_eq!(
            stub_verdict(&shape(0, Some(IO_REPARSE_TAG_MOUNT_POINT), true, false)),
            StubVerdict::FollowLink,
        );
        // …even under WindowsApps, where the old belt-and-braces arm fired on
        // any reparse point at all.
        assert_eq!(
            stub_verdict(&shape(0, Some(IO_REPARSE_TAG_SYMLINK), true, true)),
            StubVerdict::FollowLink,
        );

        // The App Execution Alias placeholder — the tag IS the positive fact.
        assert_eq!(
            stub_verdict(&shape(0, Some(IO_REPARSE_TAG_APPEXECLINK), true, true)),
            StubVerdict::Stub,
        );

        // A plain 0-byte file is NOT a stub. It cannot be `CreateProcess`d
        // either, so it fails at SPAWN and is reported `Failed` there — this
        // check has nothing to add, and guessing here is what broke rustc.
        assert_eq!(
            stub_verdict(&shape(0, None, false, false)),
            StubVerdict::NotStub
        );
        assert_eq!(
            stub_verdict(&shape(0, None, false, true)),
            StubVerdict::NotStub,
            "0 bytes under WindowsApps is not enough either: this box holds 34 WORKING 0-byte \
             aliases there (wsl.exe, winget.exe, wt.exe, bash.exe)"
        );

        // An ordinary binary, and a directory.
        assert_eq!(
            stub_verdict(&shape(4096, None, false, false)),
            StubVerdict::NotStub
        );
        assert_eq!(
            stub_verdict(&FileShape {
                is_dir: true,
                ..shape(0, Some(IO_REPARSE_TAG_APPEXECLINK), true, true)
            }),
            StubVerdict::NotStub,
        );
    }

    /// The tagless fallback exists for a failed tag read, and must stay narrow
    /// enough that only the alias shape can land in it.
    #[test]
    fn stub_verdict_fallback_needs_reparse_and_zero_bytes_and_windowsapps() {
        // All three: the alias, seen through a tag read that failed.
        assert_eq!(stub_verdict(&shape(0, None, true, true)), StubVerdict::Stub);
        // Drop any one of them and the verdict is NOT a negative.
        assert_eq!(
            stub_verdict(&shape(0, None, true, false)),
            StubVerdict::NotStub
        );
        assert_eq!(
            stub_verdict(&shape(12, None, true, true)),
            StubVerdict::NotStub
        );
        assert_eq!(
            stub_verdict(&shape(0, None, false, true)),
            StubVerdict::NotStub
        );
    }

    /// The end-to-end shape the regression was found on: a 0-byte SYMLINK whose
    /// target is a real file must never be classified a stub.
    ///
    /// Creating a symlink on Windows needs Developer Mode or admin. If that
    /// fails the test says so LOUDLY and explains what went uncovered rather
    /// than printing the same `ok` as a real pass — the classifier itself is
    /// covered unconditionally by the two tests above, which is why a skip here
    /// is tolerable at all.
    #[test]
    fn a_symlink_to_a_real_binary_is_never_a_stub() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("rustup.exe");
        std::fs::write(&real, b"MZ not really a binary but not empty either").unwrap();
        let link = dir.path().join("rustc.exe");

        // Relative target, on purpose: that is what rustup writes.
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file("rustup.exe", &link);
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink("rustup.exe", &link);

        if let Err(e) = made {
            eprintln!(
                "\n!! SKIPPED a_symlink_to_a_real_binary_is_never_a_stub: could not create a \
                 symlink here ({e}).\n!! On Windows this needs Developer Mode or an elevated \
                 shell. UNCOVERED by this run: that `is_non_executable_stub` FOLLOWS a real \
                 on-disk 0-byte symlink to its target.\n!! The classifier itself IS covered — see \
                 stub_verdict_condemns_the_alias_and_never_the_symlink.\n"
            );
            return;
        }

        // The precondition that makes this the real shape: on NTFS the LINK
        // itself reports 0 bytes, which is exactly what the old check condemned.
        //
        // It is a WINDOWS fact, not a universal one: a POSIX symlink's
        // `symlink_metadata().len()` is the length of the stored target string
        // (10 here, for `rustup.exe`), so asserting 0 unconditionally fails on
        // Linux and says nothing about the classifier. What both platforms DO
        // share is that the link is a link — `file_shape` maps that to
        // `FollowLink` on either OS — so that is what is asserted off Windows,
        // and the behavioural assertion below stays common to both.
        let link_md = std::fs::symlink_metadata(&link).unwrap();
        #[cfg(windows)]
        assert_eq!(
            link_md.len(),
            0,
            "precondition: on NTFS a symlink is a 0-byte reparse point"
        );
        #[cfg(not(windows))]
        assert!(
            link_md.file_type().is_symlink(),
            "precondition: the link must be a symlink"
        );
        assert!(
            !is_non_executable_stub(&link),
            "a rustup-style 0-byte symlink to a real binary must NOT be a stub — classifying it \
             one is what made `env apply` reinstall a working toolchain"
        );
    }

    /// A directory and a path that does not exist at all must both fall through
    /// to the retry's verdict, not be declared absent by the stub check.
    #[test]
    fn the_stub_check_declares_nothing_about_what_it_cannot_read() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_non_executable_stub(dir.path()));
        assert!(!is_non_executable_stub(&dir.path().join("nope.exe")));

        // A real binary is NOT a stub, so a slow one keeps its unknown verdict.
        let real = dir.path().join("rustc.exe");
        std::fs::write(&real, b"MZ not really a binary but not empty either").unwrap();
        assert!(!is_non_executable_stub(&real));

        // And a plain 0-byte file — the shape the old check condemned — is no
        // longer a negative verdict on its own.
        let empty = dir.path().join("python.exe");
        std::fs::write(&empty, b"").unwrap();
        assert!(!is_non_executable_stub(&empty));
    }

    /// The stub check is only reachable if the binary can be FOUND, so
    /// resolution has to work on the two shapes a probe command actually takes.
    #[test]
    fn resolve_executable_finds_a_path_and_refuses_a_name_that_is_not_there() {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("python.exe");
        std::fs::write(&stub, b"").unwrap();

        // Already a path: taken as given, existence still checked.
        assert_eq!(resolve_executable(&stub.to_string_lossy()), Some(stub));
        assert_eq!(
            resolve_executable(&dir.path().join("absent.exe").to_string_lossy()),
            None
        );

        // A bare name nothing on PATH provides resolves to nothing — which is
        // the fall-through that must leave the verdict to the retry rather than
        // declaring the tool absent.
        assert_eq!(
            resolve_executable("qontinui_no_such_binary_probe_test"),
            None
        );

        // …and the shell every platform ships DOES resolve, so a `None` above
        // means "not there", not "resolution is broken".
        let shell = if cfg!(windows) { "cmd" } else { "sh" };
        assert!(
            resolve_executable(shell).is_some(),
            "PATH resolution itself must work, or the stub check is dead code"
        );
    }

    /// Resolution must answer the question `Command::spawn` answers, because the
    /// file it returns is the one the stub check condemns. Every divergence here
    /// means inspecting a file that never ran.
    ///
    /// Windows-only: all three divergences are Windows loader rules (std appends
    /// `.exe` and consults no `PATHEXT`; the running exe's directory is searched
    /// first; an extensionless file is never executed).
    #[cfg(windows)]
    #[test]
    fn resolve_executable_mirrors_what_spawn_would_run() {
        let _env_lock = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let early = dir.path().join("early");
        let late = dir.path().join("late");
        std::fs::create_dir_all(&early).unwrap();
        std::fs::create_dir_all(&late).unwrap();

        // The Git-for-Windows shape: an EXTENSIONLESS `python` ahead of the real
        // `python.exe` on PATH. std would never run the former.
        let name = "qontinui_resolve_probe_tool";
        std::fs::write(early.join(name), b"#!/bin/sh\n").unwrap();
        let real = late.join(format!("{name}.exe"));
        std::fs::write(&real, b"MZ").unwrap();

        let prior = std::env::var_os("PATH");
        // Leading, doubled and trailing separators — `split_paths` yields an
        // EMPTY PathBuf for each, and `dir.join(name)` on one produces a
        // RELATIVE path resolved against the process cwd.
        std::env::set_var("PATH", format!(";{};;{};", early.display(), late.display()));
        let resolved = resolve_executable(name);
        if let Some(p) = prior {
            std::env::set_var("PATH", p);
        }

        let resolved = resolved.expect("the .exe on PATH must resolve");
        assert_eq!(
            resolved, real,
            "the extensionless file was returned though `spawn` would never run it"
        );
        assert!(
            resolved.is_absolute(),
            "an empty PATH entry must never yield a cwd-relative path: {}",
            resolved.display()
        );
    }

    /// `PATHEXT` is broader than what std will execute: a `.cmd` / `.bat` /
    /// `.com` on PATH is NOT something `Command::spawn` can run, so returning
    /// one pointed the stub check at a file the probe never touched.
    #[cfg(windows)]
    #[test]
    fn resolve_executable_refuses_extensions_spawn_cannot_run() {
        let _env_lock = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let name = "qontinui_resolve_probe_batch";
        std::fs::write(dir.path().join(format!("{name}.cmd")), b"@echo off\n").unwrap();
        std::fs::write(dir.path().join(format!("{name}.bat")), b"@echo off\n").unwrap();

        let prior = std::env::var_os("PATH");
        std::env::set_var("PATH", format!("{}", dir.path().display()));
        let resolved = resolve_executable(name);
        if let Some(p) = prior {
            std::env::set_var("PATH", p);
        }

        assert_eq!(
            resolved, None,
            "a .cmd/.bat is not runnable by `Command::spawn`, so it must not be what the stub \
             check inspects"
        );
    }

    /// The confirmation ladder: a first timeout is a CANDIDATE, and a
    /// genuinely-slow-but-present tool that answers inside the longer budget is
    /// MEASURED, not unmeasured. Without the retry, every key on a loaded box
    /// that lost one race stayed unknown until some future capture got lucky.
    #[test]
    fn a_slow_tool_that_answers_on_the_retry_is_measured() {
        // Answers in ~1s: past a 200ms first budget, inside a 30s retry.
        #[cfg(windows)]
        let (cmd, args) = ("cmd", ["/c", "ping -n 2 127.0.0.1 >nul & echo 1.2.3"]);
        #[cfg(not(windows))]
        let (cmd, args) = ("sh", ["-c", "sleep 1; echo 1.2.3"]);

        assert_eq!(
            version_of_confirmed(
                cmd,
                &args,
                None,
                Duration::from_millis(200),
                TEST_PROBE_BUDGET,
            ),
            ProbeOutcome::Measured("1.2.3".to_string()),
            "the retry exists so a present-but-slow tool is not reported unmeasured"
        );
    }

    /// …and a tool that answers at NO budget still ends up unmeasured. The
    /// ladder narrows the unknown arm; it must not close it, because refusing to
    /// install over a version nobody read is the safer failure.
    ///
    /// The command is the SLEEPER ITSELF, not `cmd /c ping …`: the wrapped form
    /// made this test reproduce the very leak it sits beside — `Child::kill` is
    /// one process, so each run orphaned a `ping.exe` grandchild that kept both
    /// pipe write ends open and left the reader threads blocked forever.
    /// `ChildTreeGuard` reaps a tree now, but a test should not depend on the
    /// mechanism it is not testing.
    #[test]
    fn a_wedged_probe_survives_the_ladder_as_unmeasured() {
        #[cfg(windows)]
        let (cmd, args) = ("ping", ["-n", "30", "127.0.0.1"]);
        #[cfg(not(windows))]
        let (cmd, args) = ("sleep", ["30"]);

        assert_eq!(
            version_of_confirmed(
                cmd,
                &args,
                None,
                Duration::from_millis(200),
                Duration::from_millis(400),
            ),
            ProbeOutcome::TimedOut,
        );
    }

    /// The stub check runs LAST — after the confirming retry, never instead of
    /// it. A check that pre-empts the retry spends the one mechanism that
    /// rescues a slow-but-present tool nowhere, on exactly the boxes where the
    /// check is most likely to misjudge; that ordering is what let a rustup
    /// symlink be reported absent while the toolchain was current.
    ///
    /// Injected rather than real, because the shape that misjudges (an App
    /// Execution Alias) cannot be created by a test.
    #[test]
    fn the_stub_check_never_pre_empts_the_confirming_retry() {
        // Answers in ~1s: past the 200ms first budget, inside the retry.
        #[cfg(windows)]
        let (cmd, args) = ("cmd", ["/c", "ping -n 2 127.0.0.1 >nul & echo 1.2.3"]);
        #[cfg(not(windows))]
        let (cmd, args) = ("sh", ["-c", "sleep 1; echo 1.2.3"]);

        // A stub check that condemns EVERYTHING. If it ran before the retry the
        // outcome would be `Failed` — i.e. "you do not have this tool".
        assert_eq!(
            version_of_confirmed_with(
                cmd,
                &args,
                None,
                Duration::from_millis(200),
                TEST_PROBE_BUDGET,
                &|_| true,
            ),
            ProbeOutcome::Measured("1.2.3".to_string()),
            "the confirming retry must be spent BEFORE any negative verdict"
        );

        // …and the check is still live: a tool that answers at no budget, whose
        // binary proves itself a stub, is a definite negative.
        #[cfg(windows)]
        let (wedged, wedged_args) = ("cmd", ["/c", "ping -n 30 127.0.0.1 >nul"]);
        #[cfg(not(windows))]
        let (wedged, wedged_args) = ("sh", ["-c", "sleep 30"]);
        assert_eq!(
            version_of_confirmed_with(
                wedged,
                &wedged_args,
                None,
                Duration::from_millis(200),
                Duration::from_millis(400),
                &|_| true,
            ),
            ProbeOutcome::Failed,
            "the stub check must still be able to condemn — it is only later, not gone"
        );
    }

    /// A child that EXITS while a grandchild still holds the pipe write ends
    /// must not hang the capture.
    ///
    /// This is the shape a shim leaves behind — `rustc.exe` is a rustup proxy
    /// that spawns the real toolchain, and a corepack/volta background update
    /// check outlives its parent by design. The exit path used to `join()` both
    /// reader threads with NO timeout: the pipes never EOF, `collect_versions`
    /// blocks forever, and the capture loop — `MissedTickBehavior::Skip` on a
    /// task that never returns — stops advancing at all. Every budget in this
    /// module is bypassed by that one unbounded join.
    #[test]
    fn a_grandchild_holding_the_pipe_cannot_hang_the_exit_path() {
        // The parent prints its version and exits IMMEDIATELY; the background
        // child inherits both pipes and sits on them.
        #[cfg(windows)]
        let (cmd, args) = ("cmd", ["/c", "echo 1.2.3 & start /b ping -n 30 127.0.0.1"]);
        #[cfg(not(windows))]
        let (cmd, args) = ("sh", ["-c", "echo 1.2.3; sleep 30 &"]);

        let started = std::time::Instant::now();
        let outcome = version_of_within(cmd, &args, None, TEST_PROBE_BUDGET);
        let elapsed = started.elapsed();

        assert_eq!(
            outcome,
            ProbeOutcome::Measured("1.2.3".to_string()),
            "the tool ANSWERED — a grandchild sitting on the pipe must not lose that"
        );
        assert!(
            elapsed < READER_DRAIN_GRACE + Duration::from_secs(20),
            "the exit path must be BOUNDED; took {elapsed:?}"
        );
        // …and it must have BEEN the blocked case. If the background child did
        // not actually inherit the pipes, the readers hit EOF immediately, this
        // returns in milliseconds, and the test passes while testing nothing —
        // which is worse than not having it. Measured at 2.3s here, i.e. the
        // full grace, which is only spent when a reader never sees EOF.
        assert!(
            elapsed >= READER_DRAIN_GRACE,
            "the grandchild did not hold the pipes open, so the bounded wait was never \
             exercised (took {elapsed:?}); this test is not testing what it claims"
        );
    }

    /// Only the first line is ever used, so each pipe keeps a bounded prefix —
    /// but it must keep READING past that prefix. A cap that stopped reading
    /// would re-create the deadlock the concurrent drain exists to break, for
    /// any tool that prints more than the cap.
    #[test]
    fn output_past_the_keep_cap_is_drained_not_deadlocked() {
        // ~200 KB, three times the keep cap, with the version line FIRST.
        #[cfg(windows)]
        let (cmd, args) = (
            "cmd",
            [
                "/c",
                "echo 1.2.3 & for /L %i in (1,1,4000) do @echo \
                 xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
            ],
        );
        #[cfg(not(windows))]
        let (cmd, args) = (
            "sh",
            [
                "-c",
                "echo 1.2.3; i=0; while [ $i -lt 4000 ]; do echo \
                 xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; i=$((i+1)); done",
            ],
        );
        assert_eq!(
            version_of_within(cmd, &args, None, TEST_PROBE_BUDGET),
            ProbeOutcome::Measured("1.2.3".to_string()),
            "a tool that prints more than the keep cap must still be MEASURED"
        );
    }

    /// A key that is unmeasured on EVERY pass of the capture loop is
    /// otherwise reported-and-never-acted-on with nothing to distinguish it from
    /// a one-off. The streak escalates the warn — and deliberately never decays
    /// to `Failed`, which would install over a version nobody read on a timer.
    #[test]
    fn a_persistent_unknown_escalates_its_warning() {
        let transient = unknown_streak_message("rustc", 1, 900);
        assert!(transient.contains("consecutive unmeasured captures: 1"));
        assert!(!transient.contains("WEDGED"));

        let wedged = unknown_streak_message("rustc", UNKNOWN_STREAK_ALARM, 900);
        assert!(
            wedged.contains("CONSECUTIVE CAPTURES"),
            "a perpetually-unknown key must read differently from a one-off: {wedged}"
        );
        assert!(wedged.contains("WEDGED"));
        // It names an operator action, not just a fact.
        assert!(wedged.contains("rustc --version"));
        // And it never promises to start treating the key as absent.
        assert!(wedged.contains("will NOT start treating it as absent"));
    }

    /// The elapsed time in that warn must be the runner's REAL one. It was
    /// `streak * 15` minutes: wrong at the default (four captures span three
    /// intervals = 45 minutes, not 60) and wildly wrong at the configurable 60s
    /// floor, where it told an operator "~60 minutes" about ~3 minutes. This is
    /// a WARN whose entire purpose is "this has been broken long enough to act
    /// on"; a fabricated duration is the one thing it must not carry.
    #[test]
    fn the_wedged_warning_carries_the_real_capture_interval() {
        let default_interval = unknown_streak_message("rustc", 4, 900);
        assert!(
            default_interval.contains("~45 minutes"),
            "4 captures span 3 intervals of 900s: {default_interval}"
        );
        assert!(
            !default_interval.contains("~60 minutes"),
            "the old hardcoded `streak * 15` reading must be gone: {default_interval}"
        );

        let at_the_floor = unknown_streak_message("rustc", 4, 60);
        assert!(
            at_the_floor.contains("~3 minutes") && at_the_floor.contains("60s capture interval"),
            "at the interval floor the same streak is minutes, not an hour: {at_the_floor}"
        );
    }

    /// A poisoned counter must not disable the escalation FOREVER. A `Mutex`
    /// stays poisoned for the process lifetime, so the old `Err` arm returning
    /// `1` meant a genuinely wedged probe could never reach its alarm wording
    /// again — the counter is recovered instead.
    #[test]
    fn a_poisoned_streak_counter_still_counts() {
        let key = "qontinui_streak_poison_test_key";
        clear_unknown_streak(key);

        // Poison the lock from a panicking thread.
        let _ = std::thread::spawn(|| {
            let _guard = UNKNOWN_STREAKS.lock().unwrap();
            panic!("poison the streak counter");
        })
        .join();
        assert!(
            UNKNOWN_STREAKS.is_poisoned(),
            "precondition: lock is poisoned"
        );

        assert_eq!(record_unknown_streak(key), 1);
        assert_eq!(
            record_unknown_streak(key),
            2,
            "a poisoned counter that degrades to 1 can never escalate again"
        );
        clear_unknown_streak(key);
        assert_eq!(
            record_unknown_streak(key),
            1,
            "clear must work through poison too"
        );
        clear_unknown_streak(key);
    }

    /// The streak counts CONSECUTIVE captures: any definite outcome — measured
    /// or failed — resets it, so a key that flaps never accumulates its way to
    /// the wedged wording.
    #[test]
    fn a_definite_outcome_resets_the_unknown_streak() {
        // A key of its own, so parallel tests cannot collide on the process
        // -global counter.
        let key = "qontinui_streak_probe_test_key";
        clear_unknown_streak(key);
        assert_eq!(record_unknown_streak(key), 1);
        assert_eq!(record_unknown_streak(key), 2);
        clear_unknown_streak(key);
        assert_eq!(
            record_unknown_streak(key),
            1,
            "a measured or failed capture must end the streak, not extend it"
        );
        clear_unknown_streak(key);
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
