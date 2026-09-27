//! Repo → owning-tenant resolution, with the caches that make it affordable in
//! a periodic loop.
//!
//! Phase 6 of `2026-08-29-runner-work-scoped-writes-default-tenant-credential`.
//!
//! # Why this lives in the LIB crate
//!
//! The lookup started life inside the binary's `repo_detection` module as a
//! `#[tauri::command]` feeding a spawn-picker default. Its real consumer is now
//! the plan → work-unit adapter, which is a **lib** module
//! ([`crate::plan_workunit_adapter`]) and cannot reach the binary's tree at
//! all. So the resolution, the caches and the wire parsing live here, and the
//! binary keeps only the Tauri command surface over them.
//!
//! # Why it returns a [`TenantScope`] and not an `Option<Uuid>`
//!
//! `Option<Uuid>` collapses "coord says this repo has no owner" into "coord did
//! not answer". Feeding that collapse into credential selection is the
//! absence-is-not-zero trap (served policy `verification-and-evidence`
//! `silent-empty-is-unknown`): the caller would present the DEFAULT binding's
//! JWT on a row it could not attribute, which on a multi-bound device is a
//! cross-tenant write. [`TenantScope`] keeps the two apart, and D2's degrade
//! rule then does the right thing with each.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
use tracing::debug;
use uuid::Uuid;

use crate::auth::TenantScope;

/// `repo slug → owning tenants` as coord reports it on
/// `GET /coord/canonical-repos`.
///
/// A registered repo with NO owner (`canonical_repos.tenant_id IS NULL`, the
/// unscoped-pilot default) maps to an empty [`RepoOwners`], which is distinct
/// from "repo absent" — the KEY set is what [`is_repo_registered`] answers from.
///
/// **Several owners are representable** (plan
/// `2026-09-20-a-sessions-tenant-follows-its-repo-and-every-coord-answer-names-its-tenant`
/// Phase 2). This map used to be `HashMap<String, Option<String>>`, so a repo
/// served twice collapsed to whichever row came last — a silent pick among
/// owners. `coord.tenant_repos`'s primary key is `(tenant_id, repo)`, so a
/// repo legitimately belonging to several tenants is a real shape, and every
/// reader below must see all of them rather than a guess.
pub type CanonicalRepos = HashMap<String, RepoOwners>;

/// The owners coord served for one repo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoOwners {
    /// Every distinct, parseable tenant id served for the repo, in first-seen
    /// order. Empty for a registered-but-unowned repo.
    pub tenants: Vec<Uuid>,
    /// Served `tenant_id` values that do not parse as a uuid. A shape we do
    /// not understand is not an absence, so its presence makes every reader
    /// answer UNKNOWN rather than silently dropping it.
    pub malformed: Vec<String>,
}

impl RepoOwners {
    fn add(&mut self, raw: &str) {
        match Uuid::parse_str(raw.trim()) {
            Ok(t) => {
                if !self.tenants.contains(&t) {
                    self.tenants.push(t);
                }
            }
            Err(_) => self.malformed.push(raw.to_string()),
        }
    }
}

/// How long a cached coord snapshot or `git remote` answer stands.
///
/// Sized against the plan adapter's ~68s reconcile cycle: short enough that a
/// repo which GAINS a tenant is picked up on the next cycle without restarting
/// the runner, long enough that one cycle costs one lookup rather than one per
/// artifact.
pub const CACHE_TTL: Duration = Duration::from_secs(60);

// ===========================================================================
// slug parsing
// ===========================================================================

/// `owner/name` for the checkout at `working_dir`, via
/// `git remote get-url origin`. `None` when the directory is not a checkout,
/// has no `origin`, or `git` failed.
///
/// A lossy projection of [`probe_repo`] kept for the callers whose only
/// question is "which repo, if any" — the spawn picker's registration nudge and
/// the credential-scope path, both of which treat every miss alike. A caller
/// that must tell "not a repo" from "could not look" reads [`probe_repo`].
///
/// Blocking: shells out. Callers on an async runtime go through
/// [`tenant_scope_for_path`], which dispatches it to `spawn_blocking` and
/// caches the answer.
pub fn detect_repo_slug(working_dir: &str) -> Option<String> {
    match probe_repo(working_dir, REMOTE_PROBE_BUDGET) {
        RepoProbe::Slug(s) => Some(s),
        _ => None,
    }
}

/// Budget for one `git remote get-url origin`. This is not periodic, but it IS
/// burst-prone: it fires once per `terminal_create`, and ~130 concurrent
/// session spawns were observed during the 2026-08-30 wedge. 130 unbounded
/// `.output()` calls behind one wedged git is 130 blocking-pool threads.
const REMOTE_PROBE_BUDGET: Duration = Duration::from_secs(20);

/// What `git remote get-url origin` established about a directory — with
/// "could not look" kept apart from "there is nothing to look at".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoProbe {
    /// No `.git` in the directory or any ancestor: not inside a checkout.
    NotACheckout,
    /// A GitHub `owner/name` slug.
    Slug(String),
    /// The checkout's origin is not a GitHub remote, so coord's registry (which
    /// holds GitHub slugs) cannot have registered it. Carries the remote URL.
    NotGithub(String),
    /// `git` did not answer: it exited non-zero (no `origin` remote, or a
    /// `.git` that is not a readable checkout), could not be spawned, timed
    /// out, or its output was truncated. UNKNOWN — never "not a repo".
    Failed(String),
}

/// Run `git remote get-url origin` in `working_dir` under `budget` and classify
/// the answer. Blocking: shells out.
pub fn probe_repo(working_dir: &str, budget: Duration) -> RepoProbe {
    if !has_git_ancestor(Path::new(working_dir)) {
        return RepoProbe::NotACheckout;
    }
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.args(["-C", working_dir, "remote", "get-url", "origin"]);
    match crate::process_helpers::run_probe(cmd, budget, "repo_tenant: git remote get-url origin") {
        crate::process_helpers::ProbeOutcome::Captured(stdout) => {
            let url = String::from_utf8_lossy(&stdout).trim().to_string();
            match parse_repo_slug(&url) {
                Some(slug) => RepoProbe::Slug(slug),
                None => RepoProbe::NotGithub(url),
            }
        }
        crate::process_helpers::ProbeOutcome::Degraded(reason) => RepoProbe::Failed(format!(
            "git remote get-url origin did not answer ({reason:?})"
        )),
    }
}

/// Does `dir` or any ancestor contain a `.git`? Mirrors git's own repository
/// discovery walk, and exists purely as a NEGATIVE fast path: forking `git` for
/// a directory that provably has no repository above it is a process spawn to
/// learn nothing.
///
/// Both shapes count — `.git` is a directory in a clone and a FILE in a linked
/// worktree, and the runner is full of the latter. A `GIT_DIR` environment
/// override is deliberately not honoured: it would make this answer disagree
/// with the `git` invocation below, and nothing in the runner sets one for
/// these probes.
fn has_git_ancestor(dir: &Path) -> bool {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if d.join(".git").exists() {
            return true;
        }
        cur = d.parent();
    }
    false
}

/// The hosts whose `owner/name` path is a slug in coord's canonical-repo
/// registry. Anything else must not be mapped onto a GitHub slug.
fn is_github_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("github.com") || host.eq_ignore_ascii_case("www.github.com")
}

/// `owner/name` from a git remote URL (SSH or HTTP(S)). Split from the process
/// spawn so the shapes are unit-testable.
pub fn parse_repo_slug(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }

    // SSH: git@github.com:owner/name.git
    if let Some(rest) = url.strip_prefix("git@") {
        let (host, after_colon) = rest.split_once(':')?;
        // Coord's canonical-repo registry holds GitHub `owner/name` slugs. A
        // remote on any other host is a different repo that merely shares the
        // two path segments, so it has no slug here at all.
        if !is_github_host(host) {
            return None;
        }
        let slug = after_colon.trim_end_matches(".git");
        if slug.contains('/') && !slug.is_empty() {
            return Some(slug.to_string());
        }
    }

    // HTTPS: https://github.com/owner/name.git (or http)
    if url.starts_with("https://") || url.starts_with("http://") || url.starts_with("ssh://") {
        if let Ok(parsed) = url::Url::parse(url) {
            if !parsed.host_str().is_some_and(is_github_host) {
                return None;
            }
            let path = parsed
                .path()
                .trim_start_matches('/')
                .trim_end_matches(".git");
            let parts: Vec<&str> = path.splitn(3, '/').collect();
            if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
                return Some(format!("{}/{}", parts[0], parts[1]));
            }
        }
    }

    None
}

// ===========================================================================
// coord read
// ===========================================================================

async fn fetch_registered_repos() -> Result<CanonicalRepos, String> {
    let (base, _coord_base_source) = crate::profiles::coord_base_with_source();
    let base = base.trim_end_matches('/');
    let url = format!("{base}/coord/canonical-repos");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;

    // coord-tenant-scope(device): coord's own `get_list` says it — "gate-only with FleetPrincipal ... The registry is a fleet-wide repo set (not tenant-partitioned at the read surface ...), so gate-only is correct; no tenant scoping applies" (`data/canonical_repos.rs:2198-2203`). The default binding is correct by construction, and this read could not be tenant-scoped anyway: it is what RESOLVES every other repo's tenant, so scoping it on one would be circular.
    let resp = crate::auth::attach_device_auth(client.get(&url))
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!(
            "GET /coord/canonical-repos returned {}",
            resp.status().as_u16()
        ));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("parse canonical-repos body: {e}"))?;

    parse_canonical_repos(&body)
}

/// Project coord's `GET /coord/canonical-repos` body into `repo → tenant_id`.
/// Split out from the transport so the shape contract is unit-testable.
///
/// A repo served on several rows ACCUMULATES its owners instead of the last row
/// winning — see [`CanonicalRepos`].
///
/// A body without a `canonical_repos` ARRAY is an `Err`, not an empty
/// registry: a 2xx in a shape we do not understand established nothing, and
/// an empty map would read on as "every repo is unregistered". A `tenant_id`
/// that is neither a string nor null is recorded as malformed for the same
/// reason.
fn parse_canonical_repos(body: &serde_json::Value) -> Result<CanonicalRepos, String> {
    let arr = body
        .get("canonical_repos")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "canonical-repos body carries no `canonical_repos` array".to_string())?;
    let mut out = CanonicalRepos::new();
    for item in arr {
        let Some(repo) = item.get("repo").and_then(|r| r.as_str()) else {
            continue;
        };
        let owners = out.entry(repo.to_string()).or_default();
        match item.get("tenant_id") {
            None | Some(serde_json::Value::Null) => {}
            Some(serde_json::Value::String(raw)) => owners.add(raw),
            Some(other) => owners.malformed.push(other.to_string()),
        }
    }
    Ok(out)
}

// ===========================================================================
// caches
// ===========================================================================

/// TTL'd SNAPSHOT of coord's canonical-repo map.
///
/// One snapshot rather than per-repo entries, because
/// `GET /coord/canonical-repos` returns the whole map in a single call: a
/// per-repo cache would issue N round-trips for N repos and still know nothing
/// about the repos it did not ask about. The snapshot is what bounds the plan
/// adapter — it scans every plan on a ~68s cycle, and this holds that to **at
/// most one coord lookup per cycle** however many plans (or repos) it walks.
///
/// **Negative answers are cached by construction**, which is the property the
/// common case needs today: every `canonical_repos` row had a NULL `tenant_id`
/// when Phase 1 measured them (2026-08-30), so a repo that resolves to no
/// tenant is the norm and must not be re-queried per plan. A repo ABSENT from
/// the snapshot is likewise answered from the snapshot, not by a lookup.
///
/// **Failures are cached too**, for the same bound and no other reason:
/// without it a coord outage turns one cycle into one 10s-timeout HTTP attempt
/// *per plan*. The cost is that a blip is remembered for [`CACHE_TTL`] —
/// acceptable because a *successful* read is already served up to that stale,
/// and because the stored `Err` is exactly what keeps
/// [`tenant_scope_for_repo_slug`] answering `Unresolved` rather than "this
/// repo has no tenant".
struct CanonicalRepoCache {
    ttl: Duration,
    /// `(fetched_at, outcome)`. `None` = never fetched.
    state: RwLock<Option<(Instant, Result<CanonicalRepos, String>)>>,
}

impl CanonicalRepoCache {
    fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            state: RwLock::new(None),
        }
    }

    /// Read the snapshot, refreshing through `fetch` when it is cold or older
    /// than the TTL.
    ///
    /// `now` and `fetch` are INJECTED so both the hit and the expiry are
    /// exercised without a `sleep`: a test advances `now` by adding a
    /// `Duration` to a single `Instant` and counts how often `fetch` ran.
    async fn snapshot<F, Fut>(&self, now: Instant, fetch: F) -> Result<CanonicalRepos, String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<CanonicalRepos, String>>,
    {
        {
            let state = self.state.read().await;
            if let Some((at, outcome)) = state.as_ref() {
                if now.duration_since(*at) < self.ttl {
                    return outcome.clone();
                }
            }
        }
        let fresh = fetch().await;
        let mut state = self.state.write().await;
        *state = Some((now, fresh.clone()));
        fresh
    }

    /// Drop the snapshot so the next read refetches — used after a write that
    /// changes what coord would answer.
    async fn invalidate(&self) {
        *self.state.write().await = None;
    }
}

static CANONICAL_REPOS: once_cell::sync::Lazy<CanonicalRepoCache> =
    once_cell::sync::Lazy::new(|| CanonicalRepoCache::new(CACHE_TTL));

/// TTL'd `directory → owner/name slug` cache over `git remote get-url origin`.
///
/// The coord snapshot alone does not bound the adapter's cost: resolving an
/// artifact's owning repo starts from a filesystem path, and without this every
/// plan in a cycle would fork a `git` process. A repo's origin remote is about
/// as stable as a fact gets, so a TTL'd answer is honest; `None` (not a git
/// checkout, or `git` failed) is cached for the same reason coord's negative
/// answers are — it is the steady state for any non-repo directory.
struct RepoSlugCache {
    ttl: Duration,
    entries: RwLock<HashMap<PathBuf, (Instant, Option<String>)>>,
}

impl RepoSlugCache {
    fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: RwLock::new(HashMap::new()),
        }
    }

    /// As [`CanonicalRepoCache::snapshot`]: `now` and `detect` are injected so
    /// the TTL is testable without sleeping or shelling out to `git`.
    async fn slug_for<F, Fut>(&self, dir: &Path, now: Instant, detect: F) -> Option<String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Option<String>>,
    {
        {
            let entries = self.entries.read().await;
            if let Some((at, slug)) = entries.get(dir) {
                if now.duration_since(*at) < self.ttl {
                    return slug.clone();
                }
            }
        }
        let fresh = detect().await;
        let mut entries = self.entries.write().await;
        entries.insert(dir.to_path_buf(), (now, fresh.clone()));
        fresh
    }
}

static REPO_SLUGS: once_cell::sync::Lazy<RepoSlugCache> =
    once_cell::sync::Lazy::new(|| RepoSlugCache::new(CACHE_TTL));

/// Read the canonical-repo map, refreshing through coord when the cache is
/// cold. `Err` is reserved for a genuine coord failure so callers can tell
/// "coord said no tenant" from "coord didn't answer".
pub async fn canonical_repos() -> Result<CanonicalRepos, String> {
    CANONICAL_REPOS
        .snapshot(Instant::now(), fetch_registered_repos)
        .await
}

/// Forget the cached canonical-repo snapshot, so the next read asks coord.
/// Called after registering a repo — the write changes the answer, and waiting
/// out the TTL would report the repo unregistered for up to a minute.
pub async fn invalidate_canonical_repos() {
    CANONICAL_REPOS.invalidate().await;
}

/// Is `slug` a repo coord has registered at all? Distinct from having a
/// tenant: the KEY set answers this, the value answers ownership.
///
/// `false` on a coord failure, matching the caller's degrade posture (an
/// unreachable coord must not raise a "repo not registered" alarm — it just
/// keeps quiet).
pub async fn is_repo_registered(slug: &str) -> bool {
    match canonical_repos().await {
        Ok(repos) => repos.contains_key(slug),
        Err(e) => {
            debug!("repo_tenant: failed to fetch registered repos: {e}");
            false
        }
    }
}

// ===========================================================================
// repo → tenant, as a CREDENTIAL DECISION
// ===========================================================================

/// Project one canonical-repo lookup into a [`TenantScope`]. Pure, so every
/// arm below is a unit test rather than a claim in a comment.
///
/// [`TenantScope::Device`] is unreachable from here **by construction**, and
/// that is a decision, not an omission: a work unit, a drift alarm and a commit
/// observation all HAVE an owning tenant, so "this route carries no tenancy"
/// is never a true statement about them. Only `Owned` and `Unresolved` are
/// honest answers, and `Unresolved` is safe — on a single-bound device D2 still
/// presents the default (nothing regresses today), while on a multi-bound one
/// it degrades to unauthenticated, which is the point.
fn scope_from_lookup(
    repos: Result<&CanonicalRepos, &str>,
    slug: &str,
    device_is_bound_to: &dyn Fn(&Uuid) -> bool,
) -> TenantScope {
    let repos = match repos {
        Ok(r) => r,
        // Coord did not answer. UNKNOWN, never "no tenant".
        Err(_) => return TenantScope::Unresolved,
    };
    let Some(owners) = repos.get(slug) else {
        // Repo absent from coord's registry entirely.
        return TenantScope::Unresolved;
    };
    // A tenant_id coord served that will not parse is a shape we do not
    // understand, not an absence — and not something to skip past to the
    // parseable rows either.
    if !owners.malformed.is_empty() {
        return TenantScope::Unresolved;
    }
    match owners.tenants.as_slice() {
        // `Owned` ONLY for a tenant this device can present a credential
        // for. The registry is cross-tenant: a repo registered to a tenant
        // this device is not bound to must never be declared as that
        // tenant, because the bearer lookup would then find no slot and
        // the write would go out unauthenticated under a foreign tenant id.
        // UNKNOWN is the honest answer from where this device stands.
        [t] if device_is_bound_to(t) => TenantScope::Owned(*t),
        [_] => TenantScope::Unresolved,
        // Registered, `tenant_id IS NULL` — the state ALL FIVE live rows were
        // in when Phase 1 measured them. Coord answered honestly, and the
        // answer is "nobody has claimed this repo yet".
        [] => TenantScope::Unresolved,
        // Several owners. The map used to collapse these to the last row
        // served, which picked an owner nobody chose; a credential decision
        // must not guess between tenants, so this is the same ambiguity
        // answer as an unattributable row.
        _ => TenantScope::Unresolved,
    }
}

/// Resolve the tenant that owns `slug` (an `owner/name` canonical repo) as a
/// credential decision.
pub async fn tenant_scope_for_repo_slug(slug: &str) -> TenantScope {
    let snapshot = canonical_repos().await;
    if let Err(e) = snapshot.as_ref() {
        debug!("repo_tenant: tenant_scope_for_repo_slug({slug}) — coord lookup failed: {e}");
    }
    scope_from_lookup(
        snapshot.as_ref().map_err(|e| e.as_str()),
        slug,
        &crate::auth::device_holds_usable_binding,
    )
}

/// The directory `git remote get-url origin` should run in for `path`.
///
/// A directory is its own answer; anything else (a plan's `source_path`, a
/// file that does not exist) resolves to its parent. `git -C` works from any
/// depth inside a checkout, so a subdirectory is fine.
fn repo_dir_for(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        Some(path.to_path_buf())
    } else {
        path.parent().map(Path::to_path_buf)
    }
}

/// Resolve the tenant that owns whatever repo `path` lives in — the shape
/// every work-scoped caller actually holds (a plan's `source_path`, a scanned
/// canonical checkout, an install's `repo_path`, a watched repo).
///
/// Both hops are cached ([`RepoSlugCache`] for the `git` probe,
/// [`CanonicalRepoCache`] for the coord read), so a caller in a periodic loop
/// pays at most one of each per [`CACHE_TTL`] regardless of how many artifacts
/// it walks.
pub async fn tenant_scope_for_path(path: &Path) -> TenantScope {
    let Some(dir) = repo_dir_for(path) else {
        return TenantScope::Unresolved;
    };
    let probe_dir = dir.clone();
    let slug = REPO_SLUGS
        .slug_for(&dir, Instant::now(), move || async move {
            // `git remote get-url` shells out — keep it off the async runtime.
            crate::wedge_diagnostics::spawn_blocking_tracked(move || {
                detect_repo_slug(&probe_dir.to_string_lossy())
            })
            .await
            .ok()
            .flatten()
        })
        .await;
    match slug {
        Some(s) => tenant_scope_for_repo_slug(&s).await,
        // Not a git checkout, or `git` failed: we cannot even name the repo,
        // so we certainly cannot name its tenant.
        None => TenantScope::Unresolved,
    }
}

// ===========================================================================
// cwd → EXPECTED tenant, as an OBSERVATION (never a credential decision)
// ===========================================================================

/// The tenant the repo in a session's working directory belongs to — what a
/// coord answer is compared WITH (plan
/// `2026-09-20-a-sessions-tenant-follows-its-repo-and-every-coord-answer-names-its-tenant`
/// Phase 2).
///
/// Distinct from [`TenantScope`] on purpose. `TenantScope` answers "which
/// credential may this write present", so it is `Owned` only for a tenant this
/// device holds a binding for. This answers "whose project is this", so a repo
/// owned by a tenant the device is NOT bound to is still [`CwdTenant::Resolved`]
/// — an answer from any other tenant is another project's data either way,
/// and saying so is the point.
///
/// **"Empty" and "could not look" never share an arm.** [`Self::NoRepo`] and
/// [`Self::RepoUnregistered`] are coord / git ANSWERING that there is nothing
/// to expect; [`Self::Unknown`] is every path on which nothing was established
/// (git failed or timed out, coord unreachable, a non-2xx, an unparseable body,
/// an unparseable tenant id). The old spawn-picker read collapsed all of them
/// into `Ok(None)`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(
    tag = "state",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CwdTenant {
    /// The repo has exactly one owning tenant.
    Resolved {
        tenant_id: Uuid,
        /// The `owner/name` slug that was looked up.
        repo: String,
        /// Where the ownership fact came from (`canonical_repos` today).
        source: String,
        /// RFC 3339 time this resolution was made.
        observed_at: String,
    },
    /// The directory is not inside a git checkout: there is no repo to expect
    /// anything of.
    NoRepo,
    /// A checkout whose repo has no owning tenant in coord's registry — absent
    /// from it, registered without an owner, or not a GitHub remote coord could
    /// ever have registered. Coord ANSWERED; the answer is "nobody".
    RepoUnregistered { repo: String },
    /// The repo is registered to more than one tenant.
    Several { repo: String, tenant_ids: Vec<Uuid> },
    /// Nothing could be established. UNKNOWN, never "no tenant".
    Unknown { reason: String },
}

impl CwdTenant {
    /// The `state` tag, for logs and the UI.
    pub fn state(&self) -> &'static str {
        match self {
            CwdTenant::Resolved { .. } => "resolved",
            CwdTenant::NoRepo => "no_repo",
            CwdTenant::RepoUnregistered { .. } => "repo_unregistered",
            CwdTenant::Several { .. } => "several",
            CwdTenant::Unknown { .. } => "unknown",
        }
    }
}

/// Overall budget for one [`cwd_tenant_for_path`]: the `git` probe plus the
/// (cached) coord read. It runs once per nonce mint and is awaited by at most
/// the session's first coord call, so it must be short; past it the answer is
/// [`CwdTenant::Unknown`], which is true.
pub const CWD_TENANT_BUDGET: Duration = Duration::from_secs(10);

/// The `git remote get-url origin` share of [`CWD_TENANT_BUDGET`].
const CWD_REMOTE_PROBE_BUDGET: Duration = Duration::from_secs(5);

/// A remote URL safe to show: any `user:password@` is dropped, so a token
/// embedded in an HTTPS remote never reaches a log line or a tool result.
fn display_remote(url: &str) -> String {
    // scp-style `user@host:path` FIRST: it has no `://`, and `url` would
    // otherwise happily parse `user:secret@host:path` as scheme `user` with an
    // opaque path, keeping the secret. Whatever precedes the `@` is dropped —
    // normally a bare user, but `user:secret@` is not unheard of.
    if !url.contains("://") {
        return match url.rsplit_once('@') {
            Some((_, host_path)) => host_path.to_string(),
            None => url.to_string(),
        };
    }
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.to_string()
        }
        // Anything else that does not parse is shown as a class, never
        // verbatim.
        Err(_) => "an unparseable origin remote".to_string(),
    }
}

/// Project one repo probe and one canonical-repo lookup into a [`CwdTenant`].
/// Pure, so every arm is a unit test.
pub fn classify_cwd_tenant(
    probe: &RepoProbe,
    repos: Result<&CanonicalRepos, &str>,
    observed_at: &str,
) -> CwdTenant {
    let slug = match probe {
        RepoProbe::NotACheckout => return CwdTenant::NoRepo,
        RepoProbe::Failed(reason) => {
            return CwdTenant::Unknown {
                reason: reason.clone(),
            }
        }
        RepoProbe::NotGithub(url) => {
            return CwdTenant::RepoUnregistered {
                repo: format!("{} (not a GitHub remote)", display_remote(url)),
            }
        }
        RepoProbe::Slug(slug) => slug,
    };
    let repos = match repos {
        Ok(r) => r,
        // Coord did not answer: UNKNOWN, never "unregistered".
        Err(e) => {
            return CwdTenant::Unknown {
                reason: format!("coord repo registry unreadable: {e}"),
            }
        }
    };
    let Some(owners) = repos.get(slug) else {
        return CwdTenant::RepoUnregistered { repo: slug.clone() };
    };
    if !owners.malformed.is_empty() {
        return CwdTenant::Unknown {
            reason: format!(
                "coord served an unparseable tenant id for {slug}: {}",
                owners.malformed.join(", ")
            ),
        };
    }
    match owners.tenants.as_slice() {
        [] => CwdTenant::RepoUnregistered { repo: slug.clone() },
        [t] => CwdTenant::Resolved {
            tenant_id: *t,
            repo: slug.clone(),
            source: "canonical_repos".to_string(),
            observed_at: observed_at.to_string(),
        },
        several => CwdTenant::Several {
            repo: slug.clone(),
            tenant_ids: several.to_vec(),
        },
    }
}

/// Resolve the tenant the repo at `path` belongs to — bounded by
/// [`CWD_TENANT_BUDGET`], never blocking the async runtime (the `git` probe
/// runs on the blocking pool), and never throwing: every failure is an
/// [`CwdTenant::Unknown`] naming itself.
pub async fn cwd_tenant_for_path(path: &Path) -> CwdTenant {
    cwd_tenant_with(path, CWD_TENANT_BUDGET, canonical_repos).await
}

/// [`cwd_tenant_for_path`] with the budget and the coord read injected, so the
/// timeout and the coord-failure arm are testable without a network.
async fn cwd_tenant_with<F, Fut>(path: &Path, budget: Duration, fetch: F) -> CwdTenant
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<CanonicalRepos, String>>,
{
    let resolve = async {
        let Some(dir) = repo_dir_for(path) else {
            return CwdTenant::NoRepo;
        };
        let probe = match crate::wedge_diagnostics::spawn_blocking_tracked(move || {
            probe_repo(&dir.to_string_lossy(), CWD_REMOTE_PROBE_BUDGET)
        })
        .await
        {
            Ok(p) => p,
            Err(e) => RepoProbe::Failed(format!("git probe did not complete: {e}")),
        };
        let observed_at = chrono::Utc::now().to_rfc3339();
        match &probe {
            // Only a slug needs coord at all.
            RepoProbe::Slug(_) => {
                let repos = fetch().await;
                classify_cwd_tenant(&probe, repos.as_ref().map_err(|e| e.as_str()), &observed_at)
            }
            _ => classify_cwd_tenant(&probe, Ok(&CanonicalRepos::new()), &observed_at),
        }
    };
    match tokio::time::timeout(budget, resolve).await {
        Ok(t) => t,
        Err(_) => CwdTenant::Unknown {
            reason: format!(
                "repo→tenant resolution did not finish within {}s",
                budget.as_secs()
            ),
        },
    }
}

/// Resolve a caller-named `owner/name` slug the same way — for the spawn
/// picker, which may be handed a slug instead of a directory.
pub async fn cwd_tenant_for_slug(slug: &str) -> CwdTenant {
    let observed_at = chrono::Utc::now().to_rfc3339();
    let repos = canonical_repos().await;
    classify_cwd_tenant(
        &RepoProbe::Slug(slug.to_string()),
        repos.as_ref().map_err(|e| e.as_str()),
        &observed_at,
    )
}

#[cfg(test)]
mod tests {
    /// The pre-existing tests exercise the registry arms, not the binding gate:
    /// run them as a device bound to every tenant.
    fn scope_from_lookup_all_bound(
        repos: Result<&CanonicalRepos, &str>,
        slug: &str,
    ) -> TenantScope {
        super::scope_from_lookup(repos, slug, &|_| true)
    }

    #[test]
    fn a_repo_registered_to_a_tenant_this_device_is_not_bound_to_is_unresolved() {
        let mine = uuid::Uuid::from_bytes([0xA1; 16]);
        let theirs = uuid::Uuid::from_bytes([0xB2; 16]);
        let (mine_s, theirs_s) = (mine.to_string(), theirs.to_string());
        let m = map(&[
            ("acme/ours", Some(mine_s.as_str())),
            ("other/theirs", Some(theirs_s.as_str())),
        ]);
        let bound_to_mine_only = |t: &uuid::Uuid| *t == mine;
        assert_eq!(
            super::scope_from_lookup(Ok(&m), "acme/ours", &bound_to_mine_only),
            TenantScope::Owned(mine),
        );
        assert_eq!(
            super::scope_from_lookup(Ok(&m), "other/theirs", &bound_to_mine_only),
            TenantScope::Unresolved,
            "a registry answer naming a tenant this device cannot present must not \
             become a declared tenant: the body would claim it while the bearer \
             found no slot"
        );
        // And therefore nothing is ever declared for it on the wire.
        assert_eq!(
            super::scope_from_lookup(Ok(&m), "other/theirs", &bound_to_mine_only).declared_tenant(),
            None
        );
    }

    #[test]
    fn a_non_github_remote_has_no_slug() {
        assert_eq!(parse_repo_slug("https://gitlab.com/acme/x.git"), None);
        assert_eq!(parse_repo_slug("git@gitlab.com:acme/x.git"), None);
        assert_eq!(parse_repo_slug("ssh://git@bitbucket.org/acme/x.git"), None);
        assert_eq!(
            parse_repo_slug("ssh://git@github.com/acme/x.git"),
            Some("acme/x".to_string())
        );
        assert_eq!(
            parse_repo_slug("https://GitHub.com/acme/x"),
            Some("acme/x".to_string())
        );
    }

    use super::*;

    #[test]
    fn canonical_repos_projects_repo_to_tenant() {
        let body = serde_json::json!({
            "canonical_repos": [
                { "repo": "acme/pizzeria", "tenant_id": "6b1f4b0e-0000-4000-8000-000000000001" },
                { "repo": "acme/unscoped", "tenant_id": serde_json::Value::Null },
                { "tenant_id": "6b1f4b0e-0000-4000-8000-000000000002" },
            ]
        });
        let map = parse_canonical_repos(&body).unwrap();
        // Tenant-scoped repo resolves.
        assert_eq!(
            map["acme/pizzeria"].tenants,
            vec![Uuid::parse_str("6b1f4b0e-0000-4000-8000-000000000001").unwrap()]
        );
        // Registered but unscoped: PRESENT as a key (so `is_repo_registered`
        // still says yes) with no tenant to infer.
        assert!(map.contains_key("acme/unscoped"));
        assert!(map["acme/unscoped"].tenants.is_empty());
        // An item with no `repo` is skipped entirely rather than keyed on "".
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn a_misshapen_canonical_repos_body_is_an_error_not_an_empty_registry() {
        // A 2xx in a shape we do not understand established nothing: it must
        // not read on as "every repo is unregistered".
        assert!(parse_canonical_repos(&serde_json::json!({})).is_err());
        assert!(parse_canonical_repos(&serde_json::json!({ "canonical_repos": "nope" })).is_err());
        // A genuinely empty registry is still an answer.
        assert!(
            parse_canonical_repos(&serde_json::json!({ "canonical_repos": [] }))
                .unwrap()
                .is_empty()
        );
        // A non-string tenant id is malformed, never "no owner".
        let m = parse_canonical_repos(&serde_json::json!({
            "canonical_repos": [{ "repo": "a/b", "tenant_id": 7 }]
        }))
        .unwrap();
        assert_eq!(m["a/b"].malformed, vec!["7".to_string()]);
        assert!(matches!(
            classify_cwd_tenant(&RepoProbe::Slug("a/b".into()), Ok(&m), "t0"),
            CwdTenant::Unknown { .. }
        ));
    }

    #[test]
    fn an_scp_remote_never_shows_what_precedes_the_at() {
        let t = classify_cwd_tenant(
            &RepoProbe::NotGithub("me:s3cret@gitlab.example:acme/x.git".into()),
            Ok(&CanonicalRepos::new()),
            "t0",
        );
        match t {
            CwdTenant::RepoUnregistered { repo } => {
                assert!(!repo.contains("s3cret"), "{repo}");
                assert!(repo.starts_with("gitlab.example:acme/x.git"), "{repo}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_https_url() {
        assert_eq!(
            parse_repo_slug("https://github.com/acme/widget.git"),
            Some("acme/widget".to_string())
        );
    }

    #[test]
    fn parse_https_url_no_git_suffix() {
        assert_eq!(
            parse_repo_slug("https://github.com/acme/widget"),
            Some("acme/widget".to_string())
        );
    }

    #[test]
    fn parse_ssh_url() {
        assert_eq!(
            parse_repo_slug("git@github.com:acme/widget.git"),
            Some("acme/widget".to_string())
        );
    }

    #[test]
    fn parse_ssh_url_no_git_suffix() {
        assert_eq!(
            parse_repo_slug("git@github.com:acme/widget"),
            Some("acme/widget".to_string())
        );
    }

    #[test]
    fn parse_empty_url() {
        assert_eq!(parse_repo_slug(""), None);
    }

    #[test]
    fn parse_garbage() {
        assert_eq!(parse_repo_slug("not-a-url"), None);
    }

    // ---- repo → TenantScope -------------------------------------------

    /// Build a registry the way [`parse_canonical_repos`] would from these
    /// rows — so a repo named twice accumulates, exactly as on the wire.
    fn map(pairs: &[(&str, Option<&str>)]) -> CanonicalRepos {
        let rows: Vec<serde_json::Value> = pairs
            .iter()
            .map(|(r, t)| serde_json::json!({ "repo": r, "tenant_id": t }))
            .collect();
        parse_canonical_repos(&serde_json::json!({ "canonical_repos": rows })).unwrap()
    }

    const T1: &str = "6b1f4b0e-0000-4000-8000-000000000001";

    /// The only arm that may name a tenant.
    #[test]
    fn a_tenant_scoped_repo_resolves_to_owned() {
        let m = map(&[("acme/pizzeria", Some(T1))]);
        assert_eq!(
            scope_from_lookup_all_bound(Ok(&m), "acme/pizzeria"),
            TenantScope::Owned(Uuid::parse_str(T1).unwrap())
        );
    }

    /// `tenant_id IS NULL` — the state ALL FIVE live rows were in on
    /// 2026-08-30. Coord answered; nobody owns the repo.
    #[test]
    fn a_null_tenant_row_is_unresolved_not_device() {
        let m = map(&[("acme/unscoped", None)]);
        assert_eq!(
            scope_from_lookup_all_bound(Ok(&m), "acme/unscoped"),
            TenantScope::Unresolved
        );
    }

    /// A repo coord has never heard of.
    #[test]
    fn an_absent_repo_is_unresolved() {
        let m = map(&[("acme/other", Some(T1))]);
        assert_eq!(
            scope_from_lookup_all_bound(Ok(&m), "acme/missing"),
            TenantScope::Unresolved
        );
    }

    /// The absence-is-not-zero arm, and the reason this returns a
    /// `TenantScope` instead of an `Option<Uuid>`: a coord failure must NEVER
    /// read as "this repo has no tenant", because that reads on to "present
    /// the default binding's credential".
    #[test]
    fn a_coord_failure_is_unresolved_and_not_a_missing_tenant() {
        assert_eq!(
            scope_from_lookup_all_bound(Err("GET /coord/canonical-repos returned 503"), "acme/x"),
            TenantScope::Unresolved
        );
    }

    /// A served `tenant_id` that will not parse is a shape we do not
    /// understand — still not an absence, and still never a guessed owner.
    #[test]
    fn an_unparseable_tenant_id_is_unresolved() {
        let m = map(&[("acme/bad", Some("not-a-uuid"))]);
        assert_eq!(
            scope_from_lookup_all_bound(Ok(&m), "acme/bad"),
            TenantScope::Unresolved
        );
    }

    /// `Device` is unreachable from a repo lookup BY CONSTRUCTION. A
    /// work-scoped row always has an owning tenant, so `Device` — "this route
    /// carries no tenancy" — would be a false statement about it, and it is
    /// the one variant that suppresses D2's degrade on a multi-bound device.
    #[test]
    fn a_repo_lookup_never_yields_device() {
        let m = map(&[
            ("a/owned", Some(T1)),
            ("a/null", None),
            ("a/bad", Some("nope")),
        ]);
        for slug in ["a/owned", "a/null", "a/bad", "a/absent"] {
            assert_ne!(
                scope_from_lookup_all_bound(Ok(&m), slug),
                TenantScope::Device,
                "{slug} must not classify as Device"
            );
            assert_ne!(
                scope_from_lookup_all_bound(Err("boom"), slug),
                TenantScope::Device,
                "{slug} must not classify as Device on a coord failure"
            );
        }
    }

    // ---- the caches, driven by an INJECTED clock ---------------------------
    //
    // No test here sleeps. `Instant` is `Add<Duration>`, so a single `t0` plus
    // an offset is a complete, deterministic clock.

    /// The adapter's requirement, stated as a test: many reads inside one TTL
    /// window cost ONE coord lookup — including the reads that miss, which is
    /// what makes today's all-NULL corpus survivable at hundreds of plans a
    /// cycle.
    #[tokio::test]
    async fn the_snapshot_serves_hits_and_misses_from_one_lookup() {
        let cache = CanonicalRepoCache::new(Duration::from_secs(60));
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let fetch = || {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { Ok(map(&[("a/owned", Some(T1)), ("a/null", None)])) }
        };

        let t0 = Instant::now();
        for offset in [0u64, 1, 30, 59] {
            let snap = cache
                .snapshot(t0 + Duration::from_secs(offset), fetch)
                .await
                .unwrap();
            assert!(snap.contains_key("a/owned"));
            // The NEGATIVE answers come from the same snapshot: a NULL-tenant
            // repo and an absent repo both resolve with no extra lookup.
            assert!(snap["a/null"].tenants.is_empty());
            assert!(!snap.contains_key("a/absent"));
        }
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "four reads inside the TTL must cost exactly one coord lookup"
        );
    }

    /// The TTL is what lets a repo that GAINS a tenant be picked up without
    /// restarting the runner — the whole reason the cache is not permanent.
    #[tokio::test]
    async fn the_snapshot_expires_so_a_new_owner_is_picked_up() {
        let cache = CanonicalRepoCache::new(Duration::from_secs(60));
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let t0 = Instant::now();

        let first = cache
            .snapshot(t0, || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Ok(map(&[("a/repo", None)])) }
            })
            .await
            .unwrap();
        assert_eq!(
            scope_from_lookup_all_bound(Ok(&first), "a/repo"),
            TenantScope::Unresolved
        );

        // Still inside the window: the stale NULL answer stands.
        let stale = cache
            .snapshot(t0 + Duration::from_secs(59), || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Ok(map(&[("a/repo", Some(T1))])) }
            })
            .await
            .unwrap();
        assert_eq!(
            scope_from_lookup_all_bound(Ok(&stale), "a/repo"),
            TenantScope::Unresolved
        );

        // Past it: coord is asked again and the repo now resolves.
        let fresh = cache
            .snapshot(t0 + Duration::from_secs(60), || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Ok(map(&[("a/repo", Some(T1))])) }
            })
            .await
            .unwrap();
        assert_eq!(
            scope_from_lookup_all_bound(Ok(&fresh), "a/repo"),
            TenantScope::Owned(Uuid::parse_str(T1).unwrap())
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    /// A coord outage must also cost one lookup per window, not one per plan —
    /// otherwise a single cycle becomes hundreds of ten-second timeouts. The
    /// cached failure still reads as `Unresolved`, never as "no tenant".
    #[tokio::test]
    async fn a_failed_lookup_is_cached_and_still_reads_as_unresolved() {
        let cache = CanonicalRepoCache::new(Duration::from_secs(60));
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let fetch = || {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { Err("GET /coord/canonical-repos returned 503".to_string()) }
        };
        let t0 = Instant::now();
        for offset in [0u64, 5, 59] {
            let snap = cache
                .snapshot(t0 + Duration::from_secs(offset), fetch)
                .await;
            assert_eq!(
                scope_from_lookup_all_bound(snap.as_ref().map_err(|e| e.as_str()), "a/repo"),
                TenantScope::Unresolved
            );
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// A registration write must be visible immediately, not up to a TTL later
    /// — `register_repo_with_coord` invalidates for exactly this.
    #[tokio::test]
    async fn invalidate_forces_the_next_read_to_refetch() {
        let cache = CanonicalRepoCache::new(Duration::from_secs(60));
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let fetch = || {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { Ok(map(&[("a/repo", None)])) }
        };
        let t0 = Instant::now();
        let _ = cache.snapshot(t0, fetch).await;
        cache.invalidate().await;
        let _ = cache.snapshot(t0, fetch).await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    /// The slug cache bounds the OTHER hop: one `git remote` probe per
    /// directory per window, with the "not a checkout" answer cached too.
    #[tokio::test]
    async fn the_slug_cache_probes_once_per_dir_per_window() {
        let cache = RepoSlugCache::new(Duration::from_secs(60));
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let probe = |answer: Option<&'static str>| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move { answer.map(String::from) }
        };
        let t0 = Instant::now();
        let plans = Path::new("/w/qontinui-dev-notes/plans");
        let other = Path::new("/w/not-a-repo");

        for offset in [0u64, 30, 59] {
            assert_eq!(
                cache
                    .slug_for(plans, t0 + Duration::from_secs(offset), || probe(Some(
                        "qontinui/qontinui-dev-notes"
                    )))
                    .await
                    .as_deref(),
                Some("qontinui/qontinui-dev-notes")
            );
            // A negative answer is cached as well — a non-checkout directory
            // must not re-fork `git` on every artifact under it.
            assert_eq!(
                cache
                    .slug_for(other, t0 + Duration::from_secs(offset), || probe(None))
                    .await,
                None
            );
        }
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "one probe per directory, positive and negative alike"
        );

        // Past the TTL both directories are probed again.
        let _ = cache
            .slug_for(plans, t0 + Duration::from_secs(60), || probe(Some("a/b")))
            .await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    /// The negative fast path: a directory with no `.git` anywhere above it
    /// cannot be a checkout, so `detect_repo_slug` must answer without forking
    /// `git`. Asserted through the public fn — a tempdir under the system temp
    /// root has no repository ancestor.
    #[test]
    fn a_directory_with_no_git_ancestor_needs_no_git_process() {
        let d = tempfile::tempdir().unwrap();
        assert!(!has_git_ancestor(d.path()));
        assert_eq!(detect_repo_slug(&d.path().display().to_string()), None);
        // A `.git` FILE counts, not just a directory: that is the shape every
        // linked worktree has, and the runner is full of those.
        std::fs::write(d.path().join(".git"), "gitdir: /elsewhere").unwrap();
        assert!(has_git_ancestor(d.path()));
        // And it is inherited downward, the way git's own discovery walk is.
        let nested = d.path().join("plans");
        std::fs::create_dir(&nested).unwrap();
        assert!(has_git_ancestor(&nested));
    }

    /// A file path resolves through its parent directory; a directory is its
    /// own answer. This is what lets a plan's `source_path` be handed in raw.
    #[test]
    fn repo_dir_for_walks_up_from_a_file() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        let file = dir.join("2026-01-01-plan.md");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(repo_dir_for(dir).as_deref(), Some(dir));
        assert_eq!(repo_dir_for(&file).as_deref(), Some(dir));
        // A path that does not exist is treated as a file: its parent is the
        // best guess, and a wrong guess costs an `Unresolved`, not a bad slot.
        assert_eq!(
            repo_dir_for(&dir.join("nope/plan.md")).as_deref(),
            Some(dir.join("nope").as_path())
        );
        // An empty path is neither a directory nor has a parent: nothing to
        // probe, so the caller gets `Unresolved` rather than a probe of `.`.
        assert_eq!(repo_dir_for(Path::new("")), None);
    }

    // ---- several owners, and cwd → CwdTenant ------------------------------

    const T2: &str = "6b1f4b0e-0000-4000-8000-000000000002";

    /// A repo served on two rows keeps BOTH owners — it used to collapse to
    /// the last row, a silent pick among tenants.
    #[test]
    fn several_owners_are_representable_and_never_collapse() {
        let m = map(&[("acme/shared", Some(T1)), ("acme/shared", Some(T2))]);
        assert_eq!(
            m["acme/shared"].tenants,
            vec![Uuid::parse_str(T1).unwrap(), Uuid::parse_str(T2).unwrap()]
        );
        // A duplicate of the same owner is one owner, not two.
        let dup = map(&[("acme/x", Some(T1)), ("acme/x", Some(T1))]);
        assert_eq!(dup["acme/x"].tenants.len(), 1);
    }

    /// The credential decision keeps treating ambiguity as ambiguity: several
    /// owners are `Unresolved`, never whichever row came last.
    #[test]
    fn several_owners_are_unresolved_as_a_credential_decision() {
        let m = map(&[("acme/shared", Some(T1)), ("acme/shared", Some(T2))]);
        assert_eq!(
            scope_from_lookup_all_bound(Ok(&m), "acme/shared"),
            TenantScope::Unresolved
        );
    }

    fn slug(s: &str) -> RepoProbe {
        RepoProbe::Slug(s.to_string())
    }

    #[test]
    fn a_single_owner_resolves_even_for_a_tenant_this_device_is_not_bound_to() {
        // The expectation is an observation, not a credential: no binding gate.
        let m = map(&[("acme/pizzeria", Some(T1))]);
        assert_eq!(
            classify_cwd_tenant(&slug("acme/pizzeria"), Ok(&m), "t0"),
            CwdTenant::Resolved {
                tenant_id: Uuid::parse_str(T1).unwrap(),
                repo: "acme/pizzeria".to_string(),
                source: "canonical_repos".to_string(),
                observed_at: "t0".to_string(),
            }
        );
    }

    #[test]
    fn several_owners_classify_as_several_with_every_id() {
        let m = map(&[("acme/shared", Some(T1)), ("acme/shared", Some(T2))]);
        assert_eq!(
            classify_cwd_tenant(&slug("acme/shared"), Ok(&m), "t0"),
            CwdTenant::Several {
                repo: "acme/shared".to_string(),
                tenant_ids: vec![Uuid::parse_str(T1).unwrap(), Uuid::parse_str(T2).unwrap()],
            }
        );
    }

    #[test]
    fn absent_and_unowned_repos_are_unregistered() {
        let m = map(&[("acme/unscoped", None)]);
        for s in ["acme/unscoped", "acme/absent"] {
            assert_eq!(
                classify_cwd_tenant(&slug(s), Ok(&m), "t0"),
                CwdTenant::RepoUnregistered {
                    repo: s.to_string()
                }
            );
        }
    }

    /// THE acceptance arm: coord unreachable is UNKNOWN, never
    /// "unregistered" — the two used to render identically.
    #[test]
    fn a_coord_failure_is_unknown_not_unregistered() {
        let t = classify_cwd_tenant(
            &slug("acme/pizzeria"),
            Err("GET /coord/canonical-repos: connection refused"),
            "t0",
        );
        match t {
            CwdTenant::Unknown { reason } => assert!(reason.contains("connection refused")),
            other => panic!("coord unreachable must be Unknown, got {other:?}"),
        }
    }

    #[test]
    fn an_unparseable_owner_is_unknown_not_a_pick_among_the_rest() {
        let m = map(&[("acme/bad", Some(T1)), ("acme/bad", Some("nope"))]);
        assert!(matches!(
            classify_cwd_tenant(&slug("acme/bad"), Ok(&m), "t0"),
            CwdTenant::Unknown { .. }
        ));
    }

    #[test]
    fn git_failure_is_unknown_and_no_checkout_is_no_repo() {
        let m = CanonicalRepos::new();
        assert_eq!(
            classify_cwd_tenant(&RepoProbe::NotACheckout, Ok(&m), "t0"),
            CwdTenant::NoRepo
        );
        assert!(matches!(
            classify_cwd_tenant(&RepoProbe::Failed("timed out".into()), Ok(&m), "t0"),
            CwdTenant::Unknown { .. }
        ));
    }

    /// A non-GitHub remote cannot be in coord's registry, and its display
    /// never carries an embedded credential.
    #[test]
    fn a_non_github_remote_is_unregistered_and_redacted() {
        let t = classify_cwd_tenant(
            &RepoProbe::NotGithub("https://user:s3cret@gitlab.com/acme/x.git".into()),
            Ok(&CanonicalRepos::new()),
            "t0",
        );
        match t {
            CwdTenant::RepoUnregistered { repo } => {
                assert!(!repo.contains("s3cret"), "{repo}");
                assert!(repo.contains("gitlab.com/acme/x.git"), "{repo}");
            }
            other => panic!("expected RepoUnregistered, got {other:?}"),
        }
    }

    /// End to end through the async resolver: a real checkout whose origin is
    /// a GitHub slug, with coord UNREACHABLE, is Unknown — not unregistered.
    /// A fresh checkout whose origin is `github.com/acme/pizzeria`, or `None`
    /// when this box has no `git` (the classifier tests still pin every arm).
    fn github_checkout() -> Option<tempfile::TempDir> {
        let d = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(d.path())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        (run(&["init", "-q"])
            && run(&[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/pizzeria.git",
            ]))
        .then_some(d)
    }

    #[tokio::test]
    async fn the_resolver_reports_coord_unreachable_as_unknown() {
        let Some(d) = github_checkout() else { return };
        let t = cwd_tenant_with(d.path(), Duration::from_secs(10), || async {
            Err("GET /coord/canonical-repos: connection refused".to_string())
        })
        .await;
        assert!(
            matches!(&t, CwdTenant::Unknown { reason } if reason.contains("connection refused")),
            "{t:?}"
        );

        // And the same checkout with coord answering resolves.
        let t = cwd_tenant_with(d.path(), Duration::from_secs(10), || async {
            Ok(map(&[("acme/pizzeria", Some(T1))]))
        })
        .await;
        assert!(matches!(t, CwdTenant::Resolved { .. }), "{t:?}");
    }

    #[tokio::test]
    async fn the_resolver_is_bounded() {
        let Some(d) = github_checkout() else { return };
        // A stalled coord read must end as Unknown inside the budget.
        let t = cwd_tenant_with(d.path(), Duration::from_millis(1500), || async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(CanonicalRepos::new())
        })
        .await;
        assert!(
            matches!(&t, CwdTenant::Unknown { reason } if reason.contains("did not finish")),
            "{t:?}"
        );
    }

    #[tokio::test]
    async fn a_directory_outside_any_checkout_is_no_repo() {
        let d = tempfile::tempdir().unwrap();
        let t = cwd_tenant_with(d.path(), Duration::from_secs(5), || async {
            Err("must not be asked".to_string())
        })
        .await;
        assert_eq!(t, CwdTenant::NoRepo);
    }

    /// The wire spelling the frontend and the persisted nonce store read.
    #[test]
    fn cwd_tenant_round_trips_with_a_state_tag() {
        let t = CwdTenant::Several {
            repo: "a/b".into(),
            tenant_ids: vec![Uuid::parse_str(T1).unwrap()],
        };
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(v["state"], "several");
        assert_eq!(v["tenantIds"][0], T1);
        assert_eq!(serde_json::from_value::<CwdTenant>(v).unwrap(), t);
        assert_eq!(
            serde_json::to_value(CwdTenant::NoRepo).unwrap(),
            serde_json::json!({ "state": "no_repo" })
        );
    }
}
