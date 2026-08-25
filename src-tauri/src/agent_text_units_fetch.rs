//! The two-step corpus fetch shared by [`crate::agent_commands`] and
//! [`crate::agent_skills`]: **read the index, pull only the bodies that moved.**
//!
//! ## Why the account layer is not one `GET`
//!
//! It used to be. Both modules asked `GET /api/v1/agent-…?limit=500` for the
//! whole corpus, inside a 4 s budget on the spawn critical path, and resolution
//! is fail-soft — a link too slow to finish degrades to the cache and then to
//! the embedded defaults, so the account's corpus *silently vanishes* instead
//! of erroring. That was survivable while the store held two content-free rows.
//! It is not survivable at corpus scale. Measured over the real 87-unit corpus
//! 2026-08-25:
//!
//! | Projection | bytes | needed at 4 s |
//! |---|---:|---:|
//! | `GET /api/v1/agent-text-units` (full list) | 1,988,661 | 486 KB/s |
//! | …gzipped (the server compresses; see `Cargo.toml`) | ~701,500 | 171 KB/s |
//! | **`GET /api/v1/agent-text-units/index`** (no `files`) | **47,093** | **11 KB/s** |
//! | …gzipped | 4,823 | 1.2 KB/s |
//!
//! So the warm path — the one every spawn after the first takes — is the index
//! alone: 47 KB, and flat as the corpus grows, because the per-unit envelope is
//! ~590 B. `checksum` on each index row is the same canonical `files`-map digest
//! the full listing serves, which makes it a cache key: a unit whose digest
//! matches the cached copy needs no body at all.
//!
//! ## The budget did not move, and that is deliberate
//!
//! `FETCH_TIMEOUT` stays at 4 s in both callers, and it now bounds the WHOLE
//! two-step rather than one request. Raising it would not remove the fail-soft
//! cliff, only move it — and it would put the increase on every spawn, paid
//! twice (commands, then skills). What changed is what the 4 s is spent on.
//!
//! A cold device with an empty cache still has to pull every body, and 4 s is
//! still 171 KB/s of gzipped corpus. That is the same demand as before this
//! module existed, and it is now a much less serious failure: since
//! `2026-08-20-fleet-served-agent-skills` Phase 6 the embedded bundles are the
//! whole fleet corpus, so a device that cannot finish falls back to a COMPLETE
//! working set rather than to two commands and no skills.
//!
//! ## What is not here
//!
//! * **No concurrency.** The body requests are sequential. Inside one 4 s
//!   deadline, parallelism buys latency on a link that is not the bottleneck
//!   and costs a thread-pool slot on a device where it is.
//! * **No partial result.** An index row this module can resolve from neither
//!   the fetch nor the cache degrades the whole resolve to `Unavailable`. The
//!   on-disk cache is a COMPLETE snapshot of one backend's corpus by contract —
//!   a caller that stored a half-populated one would serve it as authoritative
//!   on the next spawn.
//! * **No `offset` paging on the index.** 500 (`FETCH_LIMIT`) is the route's
//!   documented ceiling and the corpus is 87 units. Paging is what a caller
//!   should add when [`AgentTextUnitIndexResponse::items`] comes back at the
//!   limit, and nothing here may assume it does not.

use std::collections::HashMap;
use std::time::Duration;

use qontinui_types::agent_text_units::{validate_agent_text_unit_name, AgentTextUnit};
use serde::Deserialize;
use tracing::{debug, warn};

/// Page size for both list routes. 500 is the documented `limit` ceiling;
/// nothing here may hardcode the corpus size.
pub(crate) const FETCH_LIMIT: u32 = 500;

/// Largest `names=` set one body request may carry — the server's own cap on
/// the parameter. The corpus is well inside it, so in practice the body fetch
/// is a single request; the chunking exists so it stays correct if it is not.
pub(crate) const NAMES_PER_REQUEST: usize = 500;

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// One row of `GET /api/v1/agent-text-units/index` — a unit WITHOUT its `files`
/// map.
///
/// Deliberately a private deserialization struct rather than a
/// `qontinui-schemas` type, exactly as `AgentTextUnitListResponse` is: it is a
/// wire projection of a canonical type, not a canonical type, and adding it to
/// the schemas crate would put a codegen'd binding and a schema-count bump in
/// front of a struct with no second consumer. Only the fields this module acts
/// on are named; serde ignores the rest.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct AgentTextUnitMetadata {
    pub name: String,
    /// The canonical `files`-map digest. `None` from a store that has not
    /// computed one yet, which this module treats as "always a miss" — never as
    /// "unchanged".
    #[serde(default)]
    pub checksum: Option<String>,
    /// Total size of the `files` map this row stands for. Not used to decide
    /// anything; logged, so a slow resolve says how many bytes it was about to
    /// pull.
    #[serde(default)]
    pub byte_count: u64,
}

/// The index route's envelope. `pagination` is ignored — see the module docs.
#[derive(Debug, Deserialize)]
struct AgentTextUnitIndexResponse {
    #[serde(default)]
    items: Vec<AgentTextUnitMetadata>,
}

/// The full list route's envelope.
#[derive(Debug, Deserialize)]
struct AgentTextUnitListResponse {
    #[serde(default)]
    items: Vec<AgentTextUnit>,
}

// ---------------------------------------------------------------------------
// Outcome
// ---------------------------------------------------------------------------

/// What one attempt at the account layer established.
///
/// **Absent** and **unknown** are not the same fact. `NoAccount` is an
/// authoritative "this device has no account layer" and therefore *invalidates*
/// a cache; `Unavailable` is "could not tell", which is exactly when the cache
/// is the right answer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum UnitFetchOutcome {
    /// The index resolved. `index` is the authoritative membership and order of
    /// the account layer; `fetched` holds bodies for the subset whose checksum
    /// moved. Everything else the caller reuses from its own cache.
    Fresh {
        index: Vec<AgentTextUnitMetadata>,
        fetched: Vec<AgentTextUnit>,
    },
    /// No usable credential, or the backend rejected it (401/403).
    NoAccount,
    /// Transport error, server error, an unparseable body, or the whole
    /// two-step overrunning its budget — UNKNOWN.
    Unavailable(String),
}

// ---------------------------------------------------------------------------
// URLs
// ---------------------------------------------------------------------------

/// `GET …/agent-text-units/index` for one kind.
///
/// `invocable_only=true` is not optional for a provisioning client: the corpus
/// carries underscore-prefixed copy-source specs (`_gate-registration`,
/// `_loop-control`) and writing one into `.claude/commands/` makes the harness
/// offer it as a slash command. The route's own parameter documentation states
/// the same rule from the server side.
pub(crate) fn index_url(base_url: &str, kind: &str) -> String {
    format!(
        "{base_url}/api/v1/agent-text-units/index\
         ?kind={kind}&invocable_only=true&limit={FETCH_LIMIT}"
    )
}

/// `GET …/agent-text-units?names=…` — bodies for exactly this set.
///
/// `names` is matched INSIDE the `kind` filter, so `kind` travels with it.
pub(crate) fn bodies_url(base_url: &str, kind: &str, names: &[String]) -> String {
    let mut url = format!(
        "{base_url}/api/v1/agent-text-units\
         ?kind={kind}&invocable_only=true&limit={FETCH_LIMIT}"
    );
    for name in names {
        url.push_str("&names=");
        url.push_str(name);
    }
    url
}

// ---------------------------------------------------------------------------
// The diff
// ---------------------------------------------------------------------------

/// The names whose bodies must be fetched: every index row whose digest is not
/// the digest the cache holds for that name.
///
/// A row with no `checksum`, and a cache entry with no `checksum`, are both
/// misses. An unknown digest is not an unchanged one — reusing a cached body on
/// that basis would serve stale text forever against a store that never
/// computes digests.
///
/// Names that fail the store's own name rule are dropped with a warning rather
/// than interpolated into a URL: a name the store cannot hold is a name no
/// request should carry, and this is the only place remote text reaches a URL.
pub(crate) fn names_needing_bodies(
    index: &[AgentTextUnitMetadata],
    cached_checksums: &HashMap<String, String>,
) -> Vec<String> {
    index
        .iter()
        .filter(
            |row| match (&row.checksum, cached_checksums.get(&row.name)) {
                (Some(fresh), Some(cached)) => fresh != cached,
                _ => true,
            },
        )
        .filter(|row| match validate_agent_text_unit_name(&row.name) {
            Ok(()) => true,
            Err(e) => {
                warn!(
                    "agent_text_units: index row {:?} is not a valid unit name ({e}) — \
                     skipping it rather than putting it in a request URL",
                    row.name
                );
                false
            }
        })
        .map(|row| row.name.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// Fetch
// ---------------------------------------------------------------------------

/// Run [`fetch_units_async`] on a dedicated thread with its own current-thread
/// tokio runtime.
///
/// Callers reach this from a *sync* provisioning function invoked from *async*
/// spawn paths. `Handle::block_on` panics when called from inside a runtime
/// worker, so the work is moved onto its own thread instead.
///
/// The join is bounded by `budget`, which
/// [`fetch_units_async`] applies to the whole two-step; everything else on that
/// thread is local. There is no separate join deadline, so a hypothetical hang
/// inside reqwest would block the caller — accepted because the alternative
/// (detaching the thread) leaks it on every spawn.
pub(crate) fn fetch_units_blocking(
    base_url: &str,
    kind: &'static str,
    cached_checksums: HashMap<String, String>,
    budget: Duration,
) -> UnitFetchOutcome {
    let url = base_url.to_string();
    let handle = std::thread::Builder::new()
        .name(format!("agent-{kind}s-fetch"))
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    return UnitFetchOutcome::Unavailable(format!("could not build a runtime: {e}"))
                }
            };
            rt.block_on(async {
                match tokio::time::timeout(
                    budget,
                    fetch_units_async(&url, kind, &cached_checksums, budget),
                )
                .await
                {
                    Ok(outcome) => outcome,
                    Err(_) => UnitFetchOutcome::Unavailable(format!(
                        "the {kind} corpus fetch did not finish inside {budget:?}"
                    )),
                }
            })
        });
    match handle {
        Ok(h) => h.join().unwrap_or_else(|_| {
            UnitFetchOutcome::Unavailable("the fetch thread panicked".to_string())
        }),
        Err(e) => UnitFetchOutcome::Unavailable(format!("could not spawn a fetch thread: {e}")),
    }
}

/// Index, then bodies for the misses, with the stored bearer.
async fn fetch_units_async(
    base_url: &str,
    kind: &str,
    cached_checksums: &HashMap<String, String>,
    per_request_timeout: Duration,
) -> UnitFetchOutcome {
    let auth = crate::auth::AuthManager::new();
    let token = match auth.get_access_token() {
        Ok(t) if !t.trim().is_empty() => t,
        Ok(_) | Err(_) => {
            debug!(
                "agent_text_units: no stored access token — resolving the embedded {kind} \
                 defaults (sign in to use account units)"
            );
            return UnitFetchOutcome::NoAccount;
        }
    };

    let client = match reqwest::Client::builder()
        .timeout(per_request_timeout)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return UnitFetchOutcome::Unavailable(format!("could not build an HTTP client: {e}"))
        }
    };

    let index_url = index_url(base_url, kind);
    let index: Vec<AgentTextUnitMetadata> =
        match get_json::<AgentTextUnitIndexResponse>(&client, &index_url, &token).await {
            Ok(body) => body.items,
            Err(GetError::NoAccount) => return UnitFetchOutcome::NoAccount,
            Err(GetError::Unavailable(why)) => return UnitFetchOutcome::Unavailable(why),
        };

    let wanted = names_needing_bodies(&index, cached_checksums);
    if wanted.is_empty() {
        debug!(
            "agent_text_units: {kind} index has {} unit(s), all matching the cached \
             checksums — no bodies fetched",
            index.len()
        );
        return UnitFetchOutcome::Fresh {
            index,
            fetched: Vec::new(),
        };
    }

    let bytes: u64 = index
        .iter()
        .filter(|row| wanted.contains(&row.name))
        .map(|row| row.byte_count)
        .sum();
    debug!(
        "agent_text_units: {kind} index has {} unit(s); {} moved ({bytes} B of bodies to pull)",
        index.len(),
        wanted.len()
    );

    let mut fetched = Vec::with_capacity(wanted.len());
    for chunk in wanted.chunks(NAMES_PER_REQUEST) {
        let url = bodies_url(base_url, kind, chunk);
        match get_json::<AgentTextUnitListResponse>(&client, &url, &token).await {
            Ok(body) => fetched.extend(body.items),
            Err(GetError::NoAccount) => return UnitFetchOutcome::NoAccount,
            Err(GetError::Unavailable(why)) => return UnitFetchOutcome::Unavailable(why),
        }
    }

    UnitFetchOutcome::Fresh { index, fetched }
}

/// The two ways one GET can fail, kept apart because they mean opposite things
/// about the cache.
enum GetError {
    NoAccount,
    Unavailable(String),
}

async fn get_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    token: &str,
) -> Result<T, GetError> {
    let resp = match client.get(url).bearer_auth(token).send().await {
        Ok(r) => r,
        Err(e) => return Err(GetError::Unavailable(format!("GET {url} failed: {e}"))),
    };
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(GetError::NoAccount);
    }
    if !status.is_success() {
        return Err(GetError::Unavailable(format!(
            "GET {url} returned HTTP {status}"
        )));
    }
    resp.json::<T>()
        .await
        .map_err(|e| GetError::Unavailable(format!("GET {url} returned an unreadable body: {e}")))
}

// ---------------------------------------------------------------------------
// Assembly
// ---------------------------------------------------------------------------

/// Rebuild the account layer from an index plus the two places a body can come
/// from: this fetch, or the caller's cache.
///
/// `from_cache` is the caller's lookup into its own cached representation —
/// `agent_commands` caches `AgentCommand`, `agent_skills` caches
/// `AgentTextUnit`, and neither has to convert the other's shape to reuse a
/// body.
///
/// Returns `Err` naming the first index row that resolved from neither side.
/// That is not a per-unit shrug: the caller stores its result as a complete
/// snapshot of the backend's corpus, so an incomplete one must degrade the
/// whole resolve rather than be written down as authoritative.
pub(crate) fn assemble<T: Clone>(
    index: &[AgentTextUnitMetadata],
    fetched: Vec<AgentTextUnit>,
    mut from_cache: impl FnMut(&str) -> Option<T>,
    from_fetched: impl Fn(AgentTextUnit) -> Option<T>,
) -> Result<Vec<T>, String> {
    let mut by_name: HashMap<String, AgentTextUnit> = fetched
        .into_iter()
        .map(|unit| (unit.name.clone(), unit))
        .collect();

    let mut out = Vec::with_capacity(index.len());
    for row in index {
        let resolved = match by_name.remove(&row.name) {
            Some(unit) => from_fetched(unit),
            None => from_cache(&row.name),
        };
        match resolved {
            Some(item) => out.push(item),
            None => {
                return Err(format!(
                    "the index lists {:?} but neither the body fetch nor the cache produced \
                     it — treating the whole resolve as unavailable rather than caching a \
                     partial corpus",
                    row.name
                ))
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn meta(name: &str, checksum: Option<&str>) -> AgentTextUnitMetadata {
        AgentTextUnitMetadata {
            name: name.to_string(),
            checksum: checksum.map(str::to_string),
            byte_count: 100,
        }
    }

    fn cached(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    /// The warm path: every digest matches, so nothing is fetched. This is the
    /// whole point of the projection — 47 KB instead of 1.99 MB.
    #[test]
    fn a_matching_checksum_needs_no_body() {
        let index = vec![meta("vet-plan", Some("sha-1")), meta("gate", Some("sha-2"))];
        let have = cached(&[("vet-plan", "sha-1"), ("gate", "sha-2")]);
        assert!(names_needing_bodies(&index, &have).is_empty());
    }

    /// Moved, new, and dropped-from-cache each pull a body.
    #[test]
    fn a_moved_or_missing_unit_pulls_its_body() {
        let index = vec![
            meta("vet-plan", Some("sha-NEW")),
            meta("gate", Some("sha-2")),
            meta("brand-new", Some("sha-3")),
        ];
        let have = cached(&[("vet-plan", "sha-1"), ("gate", "sha-2")]);
        assert_eq!(
            names_needing_bodies(&index, &have),
            vec!["vet-plan".to_string(), "brand-new".to_string()]
        );
    }

    /// An unknown digest is a MISS, never "unchanged" — on either side.
    #[test]
    fn an_absent_checksum_is_always_a_miss() {
        let index = vec![meta("vet-plan", None)];
        assert_eq!(
            names_needing_bodies(&index, &cached(&[("vet-plan", "sha-1")])),
            vec!["vet-plan".to_string()]
        );
        let index = vec![meta("vet-plan", Some("sha-1"))];
        assert_eq!(
            names_needing_bodies(&index, &HashMap::new()),
            vec!["vet-plan".to_string()]
        );
    }

    /// A name the store could not hold never reaches a URL.
    #[test]
    fn a_malformed_index_name_is_dropped_before_the_url() {
        let index = vec![
            meta("../../etc/passwd", Some("sha-1")),
            meta("ok-unit", Some("sha-2")),
        ];
        assert_eq!(
            names_needing_bodies(&index, &HashMap::new()),
            vec!["ok-unit".to_string()]
        );
    }

    /// The two query parameters that are load-bearing rather than cosmetic, on
    /// both routes.
    #[test]
    fn both_urls_filter_by_kind_and_invocability() {
        let index = index_url("https://api.example", "skill");
        assert!(
            index.starts_with("https://api.example/api/v1/agent-text-units/index?"),
            "{index}"
        );
        assert!(index.contains("kind=skill"), "{index}");
        assert!(
            index.contains("invocable_only=true"),
            "without invocable_only the copy-source specs are provisioned to disk: {index}"
        );
        assert!(index.contains(&format!("limit={FETCH_LIMIT}")), "{index}");

        let bodies = bodies_url(
            "https://api.example",
            "command",
            &["vet-plan".to_string(), "gate".to_string()],
        );
        assert!(
            bodies.starts_with("https://api.example/api/v1/agent-text-units?"),
            "{bodies}"
        );
        assert!(bodies.contains("kind=command"), "{bodies}");
        assert!(bodies.contains("invocable_only=true"), "{bodies}");
        assert!(bodies.contains("&names=vet-plan&names=gate"), "{bodies}");
    }

    fn unit(name: &str, body: &str) -> AgentTextUnit {
        crate::agent_skills::tests::simple_unit(name, body)
    }

    /// Assembly takes each unit from whichever side has it, and keeps the
    /// index's membership and order.
    #[test]
    fn assembly_prefers_the_fetch_and_falls_back_to_the_cache() {
        let index = vec![
            meta("a", Some("sha-a")),
            meta("b", Some("sha-b")),
            meta("c", Some("sha-c")),
        ];
        let fetched = vec![unit("b", "# fresh b\n")];
        let have: HashMap<&str, &str> = [("a", "# cached a\n"), ("c", "# cached c\n")]
            .into_iter()
            .collect();

        let out: Vec<String> = assemble(
            &index,
            fetched,
            |name| have.get(name).map(|b| format!("cache:{b}")),
            |u| Some(format!("fetch:{}", u.files["SKILL.md"])),
        )
        .expect("every row resolves");
        assert_eq!(
            out,
            vec![
                "cache:# cached a\n".to_string(),
                "fetch:# fresh b\n".to_string(),
                "cache:# cached c\n".to_string(),
            ]
        );
    }

    /// A row neither side can produce fails the WHOLE resolve — the cache is a
    /// complete snapshot or it is not written.
    #[test]
    fn an_unresolvable_row_fails_the_whole_assembly() {
        let index = vec![meta("a", Some("sha-a")), meta("gone", Some("sha-g"))];
        let err = assemble(
            &index,
            vec![unit("a", "# a\n")],
            |_| None::<String>,
            |u| Some(u.name),
        )
        .expect_err("a missing row must not be silently dropped");
        assert!(err.contains("gone"), "{err}");
    }

    /// A unit the caller's converter rejects (a `command` row with no body at
    /// its entrypoint, say) is the same failure, not a silent drop.
    #[test]
    fn a_unit_the_converter_rejects_fails_the_assembly() {
        let index = vec![meta("a", Some("sha-a"))];
        let err = assemble(
            &index,
            vec![unit("a", "# a\n")],
            |_| None::<String>,
            |_| None,
        )
        .expect_err("a rejected conversion must not be silently dropped");
        assert!(err.contains('a'), "{err}");
    }
}
