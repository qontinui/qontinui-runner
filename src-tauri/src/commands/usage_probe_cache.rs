//! Single-flight + TTL cache for the per-account OAuth usage probe.
//!
//! # Why this exists
//!
//! [`crate::commands::ai_settings::probe_account_usage`] issues a live call
//! to Anthropic's API for one Claude account. It has four independent
//! callers — the Settings/Terminal `check_accounts_usage` command, the
//! `/analytics/account-usage` HTTP route, the 10-minute
//! `refresh_account_usage_snapshot` timer, and `account_migration`'s
//! usage-limit confirmation — none of which knew about the others. Each
//! fanned out one request PER CONFIGURED ACCOUNT, uncoalesced and uncached:
//! measured at 25 usage checks per 5-minute tick and **18,981 HTTP 429s per
//! day** in the dev logs.
//!
//! The probe hits the same per-account quota the CLI uses, so this is not
//! merely noisy — a stampede of probes competes with real work for the
//! account's rate limit, and the 429s it earns are then read back as
//! "account exhausted".
//!
//! # What it does
//!
//! Two mechanisms, both required:
//!
//! * **Single-flight.** Concurrent callers for the same key await ONE
//!   in-flight request rather than each issuing their own. This is what
//!   collapses the simultaneous burst (all four callers waking on the same
//!   tick).
//! * **TTL.** A result — success *or* failure — is served from cache for
//!   [`CoalescingCache::ttl`]. This is what collapses the sequential
//!   re-asks, and caching the failures matters most: without it a
//!   rate-limited account is re-probed immediately and earns another 429.
//!
//! # Why the in-flight future rather than a notification channel
//!
//! Waiters hold a clone of the leader's [`Shared`] future, so if the leader
//! is cancelled or dropped mid-flight any remaining waiter simply drives the
//! same future to completion. A channel-based design would leave waiters
//! blocked on a sender that never fires, which trades a stampede for a
//! hang.
//!
//! Nothing here is Anthropic-specific; it is keyed by `String` and generic
//! over the value so the coalescing semantics can be tested with a counting
//! fetcher instead of a live API.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use futures::future::{BoxFuture, FutureExt, Shared};

/// One key's slot in the cache.
enum Slot<V: Clone> {
    /// A completed result, valid until `at + ttl`.
    Ready { value: V, at: Instant },
    /// A request is in flight; every caller awaits this same future.
    ///
    /// `generation` distinguishes successive leaders for the same key, so a
    /// straggler still awaiting an OLD future cannot publish its stale value
    /// over a newer leader's slot.
    InFlight {
        generation: u64,
        future: Shared<BoxFuture<'static, V>>,
    },
}

/// A keyed single-flight + TTL cache.
pub(crate) struct CoalescingCache<V: Clone + Send> {
    ttl: Duration,
    slots: Mutex<HashMap<String, Slot<V>>>,
    next_generation: AtomicU64,
}

impl<V: Clone + Send + 'static> CoalescingCache<V> {
    /// Build a cache whose entries stay fresh for `ttl`.
    pub(crate) fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            slots: Mutex::new(HashMap::new()),
            next_generation: AtomicU64::new(0),
        }
    }

    /// Return `key`'s value, issuing at most one `fetch` for all concurrent
    /// callers and reusing a completed result for the cache's TTL.
    ///
    /// `fetch` is invoked only when this call becomes the leader: on a cache
    /// hit or a join it is dropped uncalled.
    pub(crate) async fn get_or_fetch<F, Fut>(&self, key: &str, fetch: F) -> V
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = V> + Send + 'static,
    {
        // Decide hit / join / lead under ONE lock, so two callers can never
        // both become leader. Constructing the future does not poll it, so
        // nothing is awaited while the lock is held (and the non-`Send`
        // guard never crosses an await point).
        let (leader_generation, future) = {
            let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());

            if let Some(Slot::Ready { value, at }) = slots.get(key) {
                if at.elapsed() < self.ttl {
                    return value.clone();
                }
            }

            // JOIN: await the leader's request instead of issuing our own.
            let joining = match slots.get(key) {
                Some(Slot::InFlight { future, .. }) => Some(future.clone()),
                _ => None,
            };

            match joining {
                Some(future) => (None, future),
                None => {
                    let generation = self.next_generation.fetch_add(1, Ordering::Relaxed) + 1;
                    let future = (fetch)().boxed().shared();
                    slots.insert(
                        key.to_string(),
                        Slot::InFlight {
                            generation,
                            future: future.clone(),
                        },
                    );
                    (Some(generation), future)
                }
            }
        };

        let value = future.await;

        // Only the leader publishes, and only over ITS OWN in-flight slot.
        // If a later leader has already taken the key (we were slow, our
        // result expired, a new request started), leave its slot alone.
        let Some(leader_generation) = leader_generation else {
            return value;
        };
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let still_ours = matches!(
            slots.get(key),
            Some(Slot::InFlight { generation, .. }) if *generation == leader_generation
        );
        if still_ours {
            slots.insert(
                key.to_string(),
                Slot::Ready {
                    value: value.clone(),
                    at: Instant::now(),
                },
            );
        }
        value
    }

    /// Drop every cached entry. Test-only: production code has no reason to
    /// invalidate, and exposing one would let a caller reinstate the
    /// stampede.
    #[cfg(test)]
    pub(crate) fn clear(&self) {
        self.slots.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    /// N concurrent callers for the same key must produce exactly ONE
    /// upstream request — the stampede half of the fix.
    #[tokio::test]
    async fn concurrent_callers_produce_one_upstream_request() {
        let cache: Arc<CoalescingCache<u32>> =
            Arc::new(CoalescingCache::new(Duration::from_secs(60)));
        let calls = Arc::new(AtomicUsize::new(0));
        // A LATCHING gate: `watch` remembers the release, so this test cannot
        // deadlock on a wake that fires before the leader is polled (which is
        // exactly the hazard a `Notify` would introduce here).
        let (release, gate) = tokio::sync::watch::channel(false);

        let mut handles = Vec::new();
        for _ in 0..25 {
            let cache = cache.clone();
            let calls = calls.clone();
            let mut gate = gate.clone();
            handles.push(tokio::spawn(async move {
                cache
                    .get_or_fetch("acct-a", move || async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        // Hold the request open so every caller is
                        // demonstrably in flight at once.
                        while !*gate.borrow_and_update() {
                            if gate.changed().await.is_err() {
                                break;
                            }
                        }
                        7u32
                    })
                    .await
            }));
        }

        // Let all 25 arrive and join, then release the single request.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "while the one request is in flight, no caller may issue a second"
        );
        release.send(true).expect("gate receiver alive");

        for h in handles {
            assert_eq!(h.await.unwrap(), 7);
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "25 concurrent callers must coalesce into ONE upstream request"
        );
    }

    /// A second call inside the TTL must produce ZERO additional upstream
    /// requests — the re-ask half of the fix.
    #[tokio::test]
    async fn a_second_call_within_the_ttl_produces_no_request() {
        let cache: CoalescingCache<u32> = CoalescingCache::new(Duration::from_secs(60));
        let calls = Arc::new(AtomicUsize::new(0));

        let fetch = |calls: Arc<AtomicUsize>| {
            move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                11u32
            }
        };

        assert_eq!(cache.get_or_fetch("acct-a", fetch(calls.clone())).await, 11);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        for _ in 0..10 {
            assert_eq!(cache.get_or_fetch("acct-a", fetch(calls.clone())).await, 11);
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "calls within the TTL must add ZERO upstream requests"
        );
    }

    /// Caching FAILURES is the point, not a side effect: an account that
    /// just 429'd must not be re-probed on the next caller's tick.
    #[tokio::test]
    async fn a_failed_result_is_cached_too() {
        let cache: CoalescingCache<Result<u32, String>> =
            CoalescingCache::new(Duration::from_secs(60));
        let calls = Arc::new(AtomicUsize::new(0));

        for _ in 0..5 {
            let calls = calls.clone();
            let out = cache
                .get_or_fetch("acct-a", move || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err::<u32, String>("API error (429)".to_string())
                })
                .await;
            assert!(out.is_err());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// The cache must not conflate accounts.
    #[tokio::test]
    async fn distinct_keys_do_not_share_a_result() {
        let cache: CoalescingCache<String> = CoalescingCache::new(Duration::from_secs(60));
        let a = cache
            .get_or_fetch("acct-a", || async { "a".to_string() })
            .await;
        let b = cache
            .get_or_fetch("acct-b", || async { "b".to_string() })
            .await;
        assert_eq!(a, "a");
        assert_eq!(b, "b");
    }

    /// Past the TTL a fresh request IS issued — the cache suppresses the
    /// stampede, it does not freeze the data.
    #[tokio::test]
    async fn a_call_after_the_ttl_refetches() {
        let cache: CoalescingCache<u32> = CoalescingCache::new(Duration::from_millis(30));
        let calls = Arc::new(AtomicUsize::new(0));

        let fetch = |calls: Arc<AtomicUsize>| {
            move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                1u32
            }
        };

        cache.get_or_fetch("acct-a", fetch(calls.clone())).await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        cache.get_or_fetch("acct-a", fetch(calls.clone())).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn clear_drops_cached_entries() {
        let cache: CoalescingCache<u32> = CoalescingCache::new(Duration::from_secs(60));
        let calls = Arc::new(AtomicUsize::new(0));
        let fetch = |calls: Arc<AtomicUsize>| {
            move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                1u32
            }
        };
        cache.get_or_fetch("k", fetch(calls.clone())).await;
        cache.clear();
        cache.get_or_fetch("k", fetch(calls.clone())).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
