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
//! # Why a detached task behind a shared future
//!
//! The leader does not run `fetch` itself: it spawns it as a detached tokio
//! task, and that TASK — not any caller — publishes the result. Callers
//! (leader included) only await a [`Shared`] handle onto the task. Two
//! failure modes of a caller-driven future are why:
//!
//! * **Cancellation.** A caller-driven future runs only while some caller
//!   polls it. If the leader is cancelled (its HTTP request dropped) the
//!   slot must still be published by someone, and if EVERY caller drops
//!   mid-flight the request is merely paused — a later caller resumes it and
//!   caches a response that is arbitrarily old as fresh. Detached, the fetch
//!   runs to completion and publishes with its own completion time whether
//!   or not anyone is still waiting (qontinui-runner#1839 was the first half
//!   of this: a cancelled leader froze the slot on its first result).
//! * **Panic.** A panic inside a `Shared` poisons it, and a poisoned future
//!   left in the slot re-panics every later joiner until restart. Detached,
//!   the panic is contained in the task; its unwind clears the slot, so only
//!   the callers already waiting on that one fetch see it and the next call
//!   fetches afresh.
//!
//! A channel-based design would leave waiters blocked on a sender that never
//! fires, which trades a stampede for a hang; a `JoinHandle` always resolves.
//!
//! Nothing here is Anthropic-specific; it is keyed by `String` and generic
//! over the value so the coalescing semantics can be tested with a counting
//! fetcher instead of a live API.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::future::{BoxFuture, FutureExt, Shared};

/// One key's slot in the cache.
enum Slot<V: Clone> {
    /// A completed result, valid until `at + ttl`.
    Ready { value: V, at: Instant },
    /// A request is in flight; every caller awaits this same handle onto the
    /// detached fetch task. It resolves to `None` only if that task panicked
    /// or was cancelled (runtime shutdown).
    ///
    /// `generation` distinguishes successive leaders for the same key, so a
    /// fetch task finishing late cannot publish its stale value over (or
    /// clear) a newer leader's slot.
    InFlight {
        generation: u64,
        future: Shared<BoxFuture<'static, Option<V>>>,
    },
}

type Slots<V> = Arc<Mutex<HashMap<String, Slot<V>>>>;

/// Lock the slot map, recovering from poisoning: every critical section here
/// is a single `get`/`insert`/`remove`, so a panic elsewhere cannot leave the
/// map half-updated.
fn lock<V: Clone>(
    slots: &Mutex<HashMap<String, Slot<V>>>,
) -> std::sync::MutexGuard<'_, HashMap<String, Slot<V>>> {
    slots.lock().unwrap_or_else(|e| e.into_inner())
}

/// Owned by the detached fetch task. Publishes the result over the task's
/// own in-flight slot, and — if dropped without publishing, i.e. the fetch
/// panicked or the task was cancelled — removes that slot so the next caller
/// leads a fresh fetch instead of joining a dead one.
struct SlotPublisher<V: Clone> {
    slots: Slots<V>,
    key: String,
    generation: u64,
    published: bool,
}

impl<V: Clone> SlotPublisher<V> {
    fn publish(mut self, value: V) {
        let mut slots = lock(&self.slots);
        let still_ours = matches!(
            slots.get(&self.key),
            Some(Slot::InFlight { generation, .. }) if *generation == self.generation
        );
        if still_ours {
            slots.insert(
                self.key.clone(),
                Slot::Ready {
                    value,
                    at: Instant::now(),
                },
            );
        }
        drop(slots);
        self.published = true;
    }
}

impl<V: Clone> Drop for SlotPublisher<V> {
    fn drop(&mut self) {
        if !self.published {
            clear_if_ours(&self.slots, &self.key, self.generation);
        }
    }
}

/// Remove `key`'s slot if it is still generation `generation`'s in-flight
/// slot; a newer leader's slot, or a published result, is left alone.
fn clear_if_ours<V: Clone>(slots: &Mutex<HashMap<String, Slot<V>>>, key: &str, generation: u64) {
    let mut slots = lock(slots);
    if matches!(
        slots.get(key),
        Some(Slot::InFlight { generation: g, .. }) if *g == generation
    ) {
        slots.remove(key);
    }
}

/// A keyed single-flight + TTL cache.
pub(crate) struct CoalescingCache<V: Clone + Send> {
    ttl: Duration,
    slots: Slots<V>,
    next_generation: AtomicU64,
}

impl<V: Clone + Send + Sync + 'static> CoalescingCache<V> {
    /// Build a cache whose entries stay fresh for `ttl`.
    pub(crate) fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            slots: Arc::new(Mutex::new(HashMap::new())),
            next_generation: AtomicU64::new(0),
        }
    }

    /// Return `key`'s value, issuing at most one `fetch` for all concurrent
    /// callers and reusing a completed result for the cache's TTL.
    ///
    /// `fetch` is invoked only when this call becomes the leader: on a cache
    /// hit or a join it is dropped uncalled. The leader runs it on a detached
    /// tokio task, so it must be called from within a tokio runtime.
    ///
    /// # Panics
    ///
    /// If the fetch panics, every caller awaiting that one fetch panics too
    /// (there is no `V` to hand them). The slot is cleared first, so the
    /// panic is not cached: the next call leads a fresh fetch.
    pub(crate) async fn get_or_fetch<F, Fut>(&self, key: &str, fetch: F) -> V
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = V> + Send + 'static,
    {
        // Decide hit / join / lead under ONE lock, so two callers can never
        // both become leader. Nothing is awaited while the lock is held (and
        // the non-`Send` guard never crosses an await point). The fetch task
        // may start on another worker immediately, but it can only publish
        // by taking this same lock, so it cannot race ahead of the insert of
        // its own in-flight slot.
        let (generation, future) = {
            let mut slots = lock(&self.slots);

            if let Some(Slot::Ready { value, at }) = slots.get(key) {
                if at.elapsed() < self.ttl {
                    return value.clone();
                }
            }

            // JOIN: await the in-flight fetch instead of issuing our own.
            match slots.get(key) {
                Some(Slot::InFlight { generation, future }) => (*generation, future.clone()),
                _ => {
                    let generation = self.next_generation.fetch_add(1, Ordering::Relaxed) + 1;
                    let fetching = (fetch)();
                    let task_slots = Arc::clone(&self.slots);
                    let task_key = key.to_string();
                    let task = tokio::spawn(async move {
                        // Built on the task's first poll, NOT here: its
                        // `Drop` takes the slot lock, and a task that tokio
                        // drops unpolled (spawned onto a runtime that is
                        // shutting down) is dropped synchronously inside
                        // `spawn`, while this caller still holds that lock.
                        // Held across the fetch, so a panic unwinds through
                        // it and clears the slot.
                        let publisher = SlotPublisher {
                            slots: task_slots,
                            key: task_key,
                            generation,
                            published: false,
                        };
                        let value = fetching.await;
                        publisher.publish(value.clone());
                        value
                    });
                    let log_key = key.to_string();
                    let future = async move {
                        // Log the JoinError once here: it carries the panic
                        // payload, which the `None` arm below cannot see.
                        task.await
                            .map_err(|err| {
                                tracing::error!(key = %log_key, %err, "usage-probe fetch task died");
                            })
                            .ok()
                    }
                    .boxed()
                    .shared();
                    slots.insert(
                        key.to_string(),
                        Slot::InFlight {
                            generation,
                            future: future.clone(),
                        },
                    );
                    (generation, future)
                }
            }
        };

        match future.await {
            Some(value) => value,
            None => {
                // The task died without publishing. Its publisher normally
                // cleared the slot already; a task dropped before its first
                // poll never built one, so clear it here too, or every later
                // caller would join the dead handle.
                clear_if_ours(&self.slots, key, generation);
                panic!("usage-probe fetch for {key:?} panicked or was cancelled")
            }
        }
    }

    /// Drop every cached entry. Test-only: production code has no reason to
    /// invalidate, and exposing one would let a caller reinstate the
    /// stampede.
    #[cfg(test)]
    pub(crate) fn clear(&self) {
        lock(&self.slots).clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    /// N concurrent callers for the same key must produce exactly ONE
    /// upstream request — the stampede half of the fix.
    // Multi-threaded, so the callers genuinely race for the leader slot.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
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
        // `<=`, not `==`: on a stalled box no worker may have polled a caller
        // yet. The final count below is the real check.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            calls.load(Ordering::SeqCst) <= 1,
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

    /// A leader cancelled mid-flight must not strand the slot: the detached
    /// fetch task publishes it as `Ready`, so once the TTL lapses the next
    /// call refetches instead of being handed the first result forever (the
    /// stale-weekly-utilization bug, qontinui-runner#1839).
    #[tokio::test]
    async fn a_cancelled_leader_does_not_freeze_the_slot() {
        let cache: Arc<CoalescingCache<u32>> =
            Arc::new(CoalescingCache::new(Duration::from_millis(30)));
        let calls = Arc::new(AtomicUsize::new(0));

        let leader = {
            let cache = cache.clone();
            let calls = calls.clone();
            tokio::spawn(async move {
                cache
                    .get_or_fetch("acct-a", move || async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        1u32
                    })
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        // A joiner attaches, then the leader is cancelled while in flight.
        let joiner = {
            let cache = cache.clone();
            tokio::spawn(async move { cache.get_or_fetch("acct-a", || async { 99u32 }).await })
        };
        tokio::time::sleep(Duration::from_millis(5)).await;
        leader.abort();
        assert_eq!(joiner.await.unwrap(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        tokio::time::sleep(Duration::from_millis(60)).await;
        let calls2 = calls.clone();
        let v = cache
            .get_or_fetch("acct-a", move || async move {
                calls2.fetch_add(1, Ordering::SeqCst);
                2u32
            })
            .await;
        assert_eq!(
            v, 2,
            "past the TTL the slot must refetch, not replay the old result"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// A fetch every caller abandoned must still run to completion and
    /// publish. Caller-driven, it would sit paused in the slot and a later
    /// caller would resume it and cache an arbitrarily old response as fresh.
    // Paused clock: the timers fire in deadline order, deterministically. Safe
    // here because the TTL (std `Instant`) is never meant to lapse.
    #[tokio::test(start_paused = true)]
    async fn an_abandoned_fetch_still_completes_and_publishes() {
        let cache: Arc<CoalescingCache<u32>> =
            Arc::new(CoalescingCache::new(Duration::from_secs(60)));
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let leader = {
            let cache = cache.clone();
            let finished = finished.clone();
            tokio::spawn(async move {
                cache
                    .get_or_fetch("acct-a", move || async move {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        finished.store(true, Ordering::SeqCst);
                        1u32
                    })
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(5)).await;
        leader.abort();

        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(
            finished.load(Ordering::SeqCst),
            "the fetch must run to completion with nobody awaiting it"
        );
        let v = cache.get_or_fetch("acct-a", || async { 99u32 }).await;
        assert_eq!(v, 1, "the unattended result must have been published");
    }

    /// A panicking fetch must not poison the key: the callers of that one
    /// fetch see the panic, and the next call fetches afresh.
    #[tokio::test]
    async fn a_panicking_fetch_does_not_poison_the_slot() {
        let cache: Arc<CoalescingCache<u32>> =
            Arc::new(CoalescingCache::new(Duration::from_secs(60)));
        let calls = Arc::new(AtomicUsize::new(0));

        let first = {
            let cache = cache.clone();
            let calls = calls.clone();
            tokio::spawn(async move {
                cache
                    .get_or_fetch("acct-a", move || async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        panic!("probe blew up");
                        #[allow(unreachable_code)]
                        0u32
                    })
                    .await
            })
        };
        assert!(first.await.unwrap_err().is_panic());

        let calls2 = calls.clone();
        let v = cache
            .get_or_fetch("acct-a", move || async move {
                calls2.fetch_add(1, Ordering::SeqCst);
                3u32
            })
            .await;
        assert_eq!(v, 3, "a panicked fetch must not be replayed");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// A fetch that finishes after its slot was replaced must neither
    /// overwrite nor clear the newer slot (the generation guard).
    #[tokio::test]
    async fn a_late_fetch_does_not_overwrite_a_newer_slot() {
        let cache: Arc<CoalescingCache<u32>> =
            Arc::new(CoalescingCache::new(Duration::from_secs(60)));
        let (release, gate) = tokio::sync::oneshot::channel::<()>();
        let (started_tx, started) = tokio::sync::oneshot::channel::<()>();

        let old = {
            let cache = cache.clone();
            tokio::spawn(async move {
                cache
                    .get_or_fetch("acct-a", move || async move {
                        let _ = started_tx.send(());
                        let _ = gate.await;
                        1u32
                    })
                    .await
            })
        };
        // The fetch is running, so generation 1's in-flight slot is in place.
        started.await.unwrap();
        cache.clear();
        assert_eq!(cache.get_or_fetch("acct-a", || async { 2u32 }).await, 2);

        release.send(()).unwrap();
        assert_eq!(old.await.unwrap(), 1);
        assert_eq!(
            cache.get_or_fetch("acct-a", || async { 99u32 }).await,
            2,
            "the late generation must not replace the newer result"
        );
    }

    /// The panic arm of the generation guard: a late fetch that panics clears
    /// only its OWN slot, never a newer one.
    #[tokio::test]
    async fn a_late_panicking_fetch_does_not_clear_a_newer_slot() {
        let cache: Arc<CoalescingCache<u32>> =
            Arc::new(CoalescingCache::new(Duration::from_secs(60)));
        let (release, gate) = tokio::sync::oneshot::channel::<()>();
        let (started_tx, started) = tokio::sync::oneshot::channel::<()>();

        let old = {
            let cache = cache.clone();
            tokio::spawn(async move {
                cache
                    .get_or_fetch("acct-a", move || async move {
                        let _ = started_tx.send(());
                        let _ = gate.await;
                        panic!("late probe blew up");
                        #[allow(unreachable_code)]
                        0u32
                    })
                    .await
            })
        };
        started.await.unwrap();
        cache.clear();
        assert_eq!(cache.get_or_fetch("acct-a", || async { 2u32 }).await, 2);

        release.send(()).unwrap();
        assert!(old.await.unwrap_err().is_panic());
        assert_eq!(
            cache.get_or_fetch("acct-a", || async { 99u32 }).await,
            2,
            "the late generation's cleanup must not clear the newer result"
        );
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
