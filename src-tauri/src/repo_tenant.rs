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

/// `repo slug → owning tenant id` as coord reports it on
/// `GET /coord/canonical-repos`. The value is `None` for repos coord has
/// registered but not tenant-scoped (`canonical_repos.tenant_id IS NULL`,
/// the unscoped-pilot default), which is distinct from "repo absent" — the
/// KEY set is what [`is_repo_registered`] answers from.
pub type CanonicalRepos = HashMap<String, Option<String>>;

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
/// Blocking: shells out. Callers on an async runtime go through
/// [`tenant_scope_for_path`], which dispatches it to `spawn_blocking` and
/// caches the answer.
pub fn detect_repo_slug(working_dir: &str) -> Option<String> {
    if !has_git_ancestor(Path::new(working_dir)) {
        return None;
    }
    // Bounded. This is not periodic, but it IS burst-prone: it fires once per
    // `terminal_create` / `spawn_worker_session`, and ~130 concurrent session
    // spawns were observed during the 2026-08-30 wedge. 130 unbounded
    // `.output()` calls behind one wedged git is 130 blocking-pool threads.
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.args(["-C", working_dir, "remote", "get-url", "origin"]);
    let crate::process_helpers::ProbeOutcome::Captured(stdout) = crate::process_helpers::run_probe(
        cmd,
        std::time::Duration::from_secs(20),
        "repo_tenant: git remote get-url origin",
    ) else {
        return None;
    };
    let url = String::from_utf8_lossy(&stdout).trim().to_string();
    parse_repo_slug(&url)
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

/// `owner/name` from a git remote URL (SSH or HTTP(S)). Split from the process
/// spawn so the shapes are unit-testable.
pub fn parse_repo_slug(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }

    // SSH: git@github.com:owner/name.git
    if let Some(rest) = url.strip_prefix("git@") {
        let after_colon = rest.split_once(':')?.1;
        let slug = after_colon.trim_end_matches(".git");
        if slug.contains('/') && !slug.is_empty() {
            return Some(slug.to_string());
        }
    }

    // HTTPS: https://github.com/owner/name.git (or http)
    if url.starts_with("https://") || url.starts_with("http://") {
        if let Ok(parsed) = url::Url::parse(url) {
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

    Ok(parse_canonical_repos(&body))
}

/// Project coord's `GET /coord/canonical-repos` body into `repo → tenant_id`.
/// Split out from the transport so the shape contract is unit-testable.
fn parse_canonical_repos(body: &serde_json::Value) -> CanonicalRepos {
    body.get("canonical_repos")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let repo = item.get("repo").and_then(|r| r.as_str())?;
                    let tenant = item
                        .get("tenant_id")
                        .and_then(|t| t.as_str())
                        .map(String::from);
                    Some((repo.to_string(), tenant))
                })
                .collect()
        })
        .unwrap_or_default()
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
fn scope_from_lookup(repos: Result<&CanonicalRepos, &str>, slug: &str) -> TenantScope {
    let repos = match repos {
        Ok(r) => r,
        // Coord did not answer. UNKNOWN, never "no tenant".
        Err(_) => return TenantScope::Unresolved,
    };
    match repos.get(slug) {
        Some(Some(raw)) => match Uuid::parse_str(raw.trim()) {
            Ok(t) => TenantScope::Owned(t),
            // A tenant_id coord served that will not parse is a shape we do
            // not understand, not an absence.
            Err(_) => TenantScope::Unresolved,
        },
        // Registered, `tenant_id IS NULL` — the state ALL FIVE live rows were
        // in when Phase 1 measured them. Coord answered honestly, and the
        // answer is "nobody has claimed this repo yet".
        Some(None) => TenantScope::Unresolved,
        // Repo absent from coord's registry entirely.
        None => TenantScope::Unresolved,
    }
}

/// Resolve the tenant that owns `slug` (an `owner/name` canonical repo) as a
/// credential decision.
pub async fn tenant_scope_for_repo_slug(slug: &str) -> TenantScope {
    let snapshot = canonical_repos().await;
    if let Err(e) = snapshot.as_ref() {
        debug!("repo_tenant: tenant_scope_for_repo_slug({slug}) — coord lookup failed: {e}");
    }
    scope_from_lookup(snapshot.as_ref().map_err(|e| e.as_str()), slug)
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

#[cfg(test)]
mod tests {
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
        let map = parse_canonical_repos(&body);
        // Tenant-scoped repo resolves.
        assert_eq!(
            map.get("acme/pizzeria").cloned().flatten().as_deref(),
            Some("6b1f4b0e-0000-4000-8000-000000000001")
        );
        // Registered but unscoped: PRESENT as a key (so `is_repo_registered`
        // still says yes) with no tenant to infer.
        assert!(map.contains_key("acme/unscoped"));
        assert_eq!(map.get("acme/unscoped").cloned().flatten(), None);
        // An item with no `repo` is skipped entirely rather than keyed on "".
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn canonical_repos_tolerates_missing_or_malformed_body() {
        assert!(parse_canonical_repos(&serde_json::json!({})).is_empty());
        assert!(
            parse_canonical_repos(&serde_json::json!({ "canonical_repos": "nope" })).is_empty()
        );
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

    fn map(pairs: &[(&str, Option<&str>)]) -> CanonicalRepos {
        pairs
            .iter()
            .map(|(r, t)| (r.to_string(), t.map(String::from)))
            .collect()
    }

    const T1: &str = "6b1f4b0e-0000-4000-8000-000000000001";

    /// The only arm that may name a tenant.
    #[test]
    fn a_tenant_scoped_repo_resolves_to_owned() {
        let m = map(&[("acme/pizzeria", Some(T1))]);
        assert_eq!(
            scope_from_lookup(Ok(&m), "acme/pizzeria"),
            TenantScope::Owned(Uuid::parse_str(T1).unwrap())
        );
    }

    /// `tenant_id IS NULL` — the state ALL FIVE live rows were in on
    /// 2026-08-30. Coord answered; nobody owns the repo.
    #[test]
    fn a_null_tenant_row_is_unresolved_not_device() {
        let m = map(&[("acme/unscoped", None)]);
        assert_eq!(
            scope_from_lookup(Ok(&m), "acme/unscoped"),
            TenantScope::Unresolved
        );
    }

    /// A repo coord has never heard of.
    #[test]
    fn an_absent_repo_is_unresolved() {
        let m = map(&[("acme/other", Some(T1))]);
        assert_eq!(
            scope_from_lookup(Ok(&m), "acme/missing"),
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
            scope_from_lookup(Err("GET /coord/canonical-repos returned 503"), "acme/x"),
            TenantScope::Unresolved
        );
    }

    /// A served `tenant_id` that will not parse is a shape we do not
    /// understand — still not an absence, and still never a guessed owner.
    #[test]
    fn an_unparseable_tenant_id_is_unresolved() {
        let m = map(&[("acme/bad", Some("not-a-uuid"))]);
        assert_eq!(
            scope_from_lookup(Ok(&m), "acme/bad"),
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
                scope_from_lookup(Ok(&m), slug),
                TenantScope::Device,
                "{slug} must not classify as Device"
            );
            assert_ne!(
                scope_from_lookup(Err("boom"), slug),
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
            assert_eq!(snap.get("a/null").cloned().flatten(), None);
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
            scope_from_lookup(Ok(&first), "a/repo"),
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
            scope_from_lookup(Ok(&stale), "a/repo"),
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
            scope_from_lookup(Ok(&fresh), "a/repo"),
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
                scope_from_lookup(snap.as_ref().map_err(|e| e.as_str()), "a/repo"),
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
}
