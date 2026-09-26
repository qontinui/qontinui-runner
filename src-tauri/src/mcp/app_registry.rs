//! App Registry Module
//!
//! In-memory registry for apps that phone home via
//! `POST /ui-bridge/apps/register`. Replaces the earlier behavior where the
//! register handler echoed a `DiscoveredApp` back without persisting it.
//!
//! Entries are keyed by `appId`. The SDK re-POSTs on a heartbeat (~10s);
//! a background sweeper evicts entries older than `REGISTRATION_TTL_MS`.
//!
//! Phase 1 (wrapper framework): entries now carry an `AppTransport` discriminator.
//! Apps that speak HTTP register as `Http` (the runner reaches them via
//! their `base_url`); apps that speak WebSocket register as `Websocket` and
//! the runner dispatches commands through `CommandRelay` over the
//! `/ui-bridge/ws` connection identified by `websocket_conn_id`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use super::app_discovery::DiscoveredApp;
use super::relay_binding::{
    BindingMode, Principal, Refusal, RelayBinding, BINDING_TOMBSTONE_MS, RULE_R1, RULE_R1_SLOT,
    RULE_R2, RULE_R5, RULE_R5_KEEP_ALIVE,
};

/// Entries older than this (in ms) are considered stale and evicted.
pub const REGISTRATION_TTL_MS: i64 = 30_000;
const SWEEP_INTERVAL_MS: u64 = 15_000;

/// Transport an app uses to receive commands from the runner.
///
/// - `Http`: runner reaches the app by POSTing to its `base_url`. This is the
///   legacy / same-origin case (desktop apps hosting their own UI Bridge HTTP
///   server, dev servers that proxy runner requests, etc.).
/// - `Websocket`: the app opened a WebSocket to `/ui-bridge/ws` on the runner
///   and is receiving commands on that socket. This is the wrapper-framework
///   case — it works for third-party browser tabs, headless Playwright shims,
///   extensions, and anywhere else the runner cannot reach the app via HTTP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AppTransport {
    #[default]
    Http,
    Websocket,
}

#[derive(Debug, Clone)]
pub struct RegisteredApp {
    pub app: DiscoveredApp,
    /// The origin the registrant DECLARED in its body / frame. Display only —
    /// a browser cannot be trusted about its own origin, so R4 pins this to
    /// the header origin and [`Self::verified_origin`] is what callers read.
    pub declared_origin: Option<String>,
    /// WHO registered this entry. Only this principal (or operator trust) may
    /// displace, refresh or release it (R1, R2, R5).
    pub principal: Principal,
    pub last_seen_ms: i64,
    /// How the runner delivers commands to this app. Defaults to `Http`.
    pub transport: AppTransport,
    /// If `transport == Websocket`, the `conn_id` of the active
    /// `/ui-bridge/ws` connection. `None` for HTTP apps.
    pub websocket_conn_id: Option<u64>,
    /// Optional per-entry TTL override (in ms). When `Some(ms)`, the registry's
    /// freshness check compares against `ms` instead of the global
    /// `REGISTRATION_TTL_MS`. Useful for tests, scripts, and synthetic
    /// injections that need entries to stay alive longer than the default
    /// 30-second heartbeat window without sending heartbeats. `None` means
    /// "use the global default."
    pub keep_alive_ms: Option<i64>,
    /// When the holder EXPLICITLY released this id (`DELETE`, WebSocket
    /// teardown), if it did. The row is kept after a release so the id stays
    /// reserved for its holder — see [`Self::reserved_until`] — but a released
    /// row is never live, so it is invisible to `list_live` and `get_live` and
    /// therefore to `/ui-bridge/apps/registered` and to every routing reader.
    pub released_at_ms: Option<i64>,
}

/// Registry ceiling. Rows now outlive their TTL (they carry the reservation),
/// so residency is longer and needs a bound of its own. A claimant may only
/// ever give up a row of its OWN to fit under it.
pub const MAX_ROWS: usize = 4096;

/// Rows at the top of [`MAX_ROWS`] that only operator trust may occupy.
///
/// Browser principals are held to `MAX_ROWS - OPERATOR_HEADROOM`, so no
/// quantity of them can consume the last slice. Without it the ceiling was an
/// autonomy defect, not just a capacity one: a page that registered
/// `MAX_ROWS` fresh ids and re-POSTed each inside the 30 s TTL (~137 req/s on
/// loopback, one tab) left every row LIVE, so nothing was reservation-only,
/// nothing was self-evictable, and an AGENT registering a synthetic app got
/// `UIB_REGISTRY_FULL` indefinitely — `agent_flow_unchanged`'s first step
/// denied by a web page.
///
/// A reservation rather than a wider eviction right on purpose: letting an
/// agent take a browser's row would put back a RANKING over other principals'
/// rows, and every ranking tried has been measured attacker-steerable.
/// Reserving capacity picks no victim, so there is nothing to steer.
pub const OPERATOR_HEADROOM: usize = 256;

/// The browser ceiling is `MAX_ROWS - OPERATOR_HEADROOM`, an unsigned
/// subtraction. Raise the headroom to or past `MAX_ROWS` and it UNDERFLOWS to
/// `usize::MAX` in release, so the browser ceiling becomes unbounded and M2 is
/// silently reinstated — while every test stays green, because they all
/// compute the bound from this same expression. A zero headroom is the other
/// end of the same mistake: it removes the reservation without removing any
/// code that claims to rely on it. Both are compile errors now.
const _: () = assert!(OPERATOR_HEADROOM > 0 && OPERATOR_HEADROOM < MAX_ROWS);

/// Longest accepted `appId`. The ceilings bound the COUNT of rows, not their
/// BYTES: without this, one origin could hold its quota of rows keyed on
/// multi-megabyte strings for a whole reservation window, against a 100 MB
/// body limit.
pub const MAX_APP_ID_LEN: usize = 256;

impl RegisteredApp {
    /// The header origin the runner VERIFIED this registrant at. `None` for
    /// operator trust (which sends none) and for a keyed tab.
    pub fn verified_origin(&self) -> Option<String> {
        self.principal.verified_origin()
    }

    /// The requester class the registration was admitted under.
    pub fn principal_class(&self) -> &'static str {
        self.principal.class_str()
    }

    /// Within its TTL (per-entry `keep_alive_ms`, else the global default) and
    /// not explicitly released. This is what `list_live` and `get_live` serve,
    /// so a reservation-only row is invisible to every reader that routes.
    pub(crate) fn is_live(&self, now: i64) -> bool {
        self.released_at_ms.is_none()
            && now - self.last_seen_ms <= self.keep_alive_ms.unwrap_or(REGISTRATION_TTL_MS)
    }

    /// How long this id stays RESERVED for its holder after the registration
    /// ends — `BINDING_TOMBSTONE_MS` past whichever way it ended.
    ///
    /// BOTH endings are carried by the row, which is the whole point. An
    /// earlier shape kept expiry on the row but put an explicit release into a
    /// globally-bounded side map that unrelated principals could displace.
    /// That inverted the incentive: an SDK that behaves well and sends its
    /// `beforeunload` DELETE moved its reservation from the unforgeable row
    /// into an attackable map, and ended up LESS protected than an app that
    /// simply vanished.
    pub(crate) fn reserved_until(&self) -> i64 {
        match self.released_at_ms {
            Some(released) => released + BINDING_TOMBSTONE_MS,
            None => {
                self.last_seen_ms
                    + self.keep_alive_ms.unwrap_or(REGISTRATION_TTL_MS)
                    + BINDING_TOMBSTONE_MS
            }
        }
    }
}

/// What [`AppRegistry::claim`] actually wrote. Today only the EFFECTIVE
/// `keep_alive_ms`, which R5 may have capped below what the registrant asked
/// for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Claimed {
    pub keep_alive_ms: Option<i64>,
}

pub struct AppRegistry {
    inner: RwLock<HashMap<String, RegisteredApp>>,
}

impl AppRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(HashMap::new()),
        })
    }

    /// Claim `app.app_id` for `principal`, then insert or refresh the entry —
    /// the CHECK and the WRITE under ONE write-lock acquisition, so two
    /// concurrent registrations can never both pass against the same prior
    /// holder (plan `2026-09-17-ui-bridge-relay-registration-is-unauthenticated`,
    /// "Holding a claim is atomic").
    ///
    /// Refuses with `UIB_REGISTRATION_HELD` when a LIVE row, or a row still
    /// inside its RESERVATION, belongs to a principal this one may not
    /// displace (R1, R5).
    ///
    /// The `transport` + `websocket_conn_id` pair must be consistent:
    /// `Websocket` requires `Some(conn_id)`, `Http` requires `None`. The
    /// caller (register handler / WS relay) is responsible for that invariant.
    ///
    /// Returns what was actually written, so the caller can tell the
    /// registrant its EFFECTIVE `keep_alive_ms` rather than letting a browser
    /// that asked for an hour discover in 30 s that its entry is gone.
    #[allow(clippy::too_many_arguments)]
    pub async fn claim(
        &self,
        binding: &RelayBinding,
        principal: &Principal,
        route: &'static str,
        slot_holder: Option<&Principal>,
        app: DiscoveredApp,
        declared_origin: Option<String>,
        transport: AppTransport,
        websocket_conn_id: Option<u64>,
        keep_alive_ms: Option<i64>,
    ) -> Result<Claimed, Refusal> {
        let mode = binding.config.binding;
        let now = chrono::Utc::now().timestamp_millis();
        let mut w = self.inner.write().await;

        let mut keep_alive_ms = keep_alive_ms;
        if mode != BindingMode::Off {
            // R1 against the LIVE WebSocket routing slot. It lives INSIDE this
            // lock, so BOTH registrant doors inherit it. The HTTP register
            // door is the easier route to the same takeover and the one that
            // actually decides the dispatch target — `AppDispatcher` reads the
            // REGISTRY, so an attacker pointing it at its own `baseUrl` wins
            // even while the victim's socket is still open.
            if let Some(slot) = slot_holder {
                if !principal.may_displace(slot) {
                    binding.meter(
                        mode,
                        principal,
                        route,
                        Refusal::registration_held(RULE_R1_SLOT),
                    )?;
                }
            }
            match w.get(&app.app_id) {
                // R1: a live holder is displaced only by itself or operator trust.
                Some(existing) if existing.is_live(now) => {
                    if !principal.may_displace(&existing.principal) {
                        binding.meter(
                            mode,
                            principal,
                            route,
                            Refusal::registration_held(RULE_R1),
                        )?;
                    }
                }
                // R5, held ON THE ROW, for BOTH ways a registration ends —
                // expiry and explicit release. `reserved_until` knows which.
                //
                // There is no side map. Two earlier shapes are recorded so
                // they are not re-invented: a row past its TTL but not yet
                // swept used to fall through to a tombstone map that was
                // EMPTY until the sweeper ran 15 s later, while `list_live`
                // had already dropped the row from
                // `/ui-bridge/apps/registered` — the attacker's signal; and an
                // explicitly released id used to move into that same globally
                // bounded map, where unrelated principals could evict it, so
                // an SDK that sent its `beforeunload` DELETE ended up LESS
                // protected than one that simply vanished.
                Some(existing) => {
                    // The operator-trust carve-out: an agent's id is free the
                    // moment its registration ends, expiry included. Without
                    // it, an agent that registered with `keepAliveSecs: 3600`
                    // and stopped heartbeating would lock a legitimate browser
                    // page out of that id for 60 s past a one-hour TTL.
                    if now <= existing.reserved_until()
                        && !existing.principal.is_operator_trust()
                        && !principal.may_displace(&existing.principal)
                    {
                        binding.meter(
                            mode,
                            principal,
                            route,
                            Refusal::registration_held(RULE_R5),
                        )?;
                    }
                }
                // No row: the reservation, if there was one, has lapsed and
                // the sweeper has been through.
                None => {}
            }
            // R5's second arm: a browser principal may not park an entry past
            // the heartbeat window. COUNTED in both modes so Phase 4 can see
            // whether it would bite; APPLIED only under `enforce`, because in
            // `shadow` no rule may change behaviour.
            if !principal.is_operator_trust() {
                if let Some(asked) = keep_alive_ms {
                    if asked > REGISTRATION_TTL_MS {
                        binding
                            .counters
                            .record(RULE_R5_KEEP_ALIVE, mode == BindingMode::Enforce);
                        if mode == BindingMode::Enforce {
                            tracing::warn!(
                                class = principal.class_str(),
                                app_id = %app.app_id,
                                asked_ms = asked,
                                capped_ms = REGISTRATION_TTL_MS,
                                route = route,
                                "ui-bridge binding (R5): capped a browser principal's keepAliveSecs to the registration TTL; the effective value is echoed as keepAliveMs"
                            );
                            keep_alive_ms = Some(REGISTRATION_TTL_MS);
                        }
                    }
                }
            }
        }

        // Row ceiling — RESERVED HEADROOM plus SELF-EVICTION.
        //
        // Two separate jobs, and conflating them was the defect here:
        //
        // 1. A browser principal's ceiling is `MAX_ROWS - OPERATOR_HEADROOM`.
        //    Operator trust gets the full `MAX_ROWS`, so the last
        //    `OPERATOR_HEADROOM` rows are unreachable to every browser
        //    principal no matter how many of them there are. This is what
        //    keeps `agent_flow_unchanged`'s very first step working while a
        //    page floods: an agent registering a synthetic app must never be
        //    refused because a web page filled the map.
        //
        //    A quota is the only shape that survives here. Widening eviction
        //    so an agent could take a browser row would reintroduce a RANKING
        //    over other principals' rows, and every such ranking has been
        //    measured steerable (below). Reserving capacity selects no victim
        //    at all, so there is nothing to steer.
        //
        // 2. Under its own ceiling a claimant may still give up a row of ITS
        //    OWN — its soonest-to-lapse reservation-only row — so a
        //    legitimately churning app is not throttled by its own history.
        //    It may NEVER cause another principal's row to be dropped.
        //
        //    One kind is excluded from that relief, deliberately: an
        //    `Opaque` principal can never self-evict, because
        //    `Principal::same` is false for it even against another
        //    `Opaque` — two principal-less requests are not the same
        //    principal, which is the whole point of R-opaque. So an opaque
        //    claimant at the ceiling is always refused rather than recycling
        //    "its own" rows. That is correct (it has no identity to own a row
        //    WITH), but it means the sentence above does not cover it.
        //
        // The rankings this refuses to re-invent, from the deleted tombstone
        // map: "evict the soonest to expire" is "evict the oldest", which is
        // always the victim's because an attacker's rows are newer by
        // construction; "evict the largest bucket" is attacker-chosen because
        // bucket size is; and `share = MAX / buckets` divides to ZERO once the
        // bucket count reaches the ceiling, at which point every bucket is
        // "over its share" and the tie falls to whichever origin the comparison
        // ranks last — an origin an attacker simply picks. Note that an
        // operator-trust claimant would hit the same trap: `may_displace` is
        // true for it against everyone, so "its own" would have degenerated
        // into exactly that global soonest-to-lapse ranking. The headroom
        // removes any need for an agent to evict anyone.
        //
        // Consequences, stated so they are chosen rather than discovered:
        //
        // - A full registry cannot be used to TAKE an id. Claiming an id that
        //   already has a row replaces it rather than growing the map, so the
        //   ceiling is not even consulted — a holder reclaiming its own live
        //   or reserved id is admitted at a full map.
        // - The cost of saturation is that a BRAND NEW browser id cannot be
        //   registered until capacity frees up. An attacker DOES control
        //   whether saturation exists and can sustain it by re-POSTing inside
        //   the TTL; what it cannot do is pick WHICH existing row pays, or
        //   reach the operator's headroom.
        // - It applies in EVERY mode, `off` included, because an unbounded
        //   in-process map is a memory-exhaustion defect rather than a
        //   routing rule, and the kill switch must not re-open it. It is
        //   counted on `/health` as `registryFullRefusals`, never under
        //   `rules`.
        let ceiling = if principal.is_operator_trust() {
            MAX_ROWS
        } else {
            MAX_ROWS - OPERATOR_HEADROOM
        };
        if !w.contains_key(&app.app_id) && w.len() >= ceiling {
            // The claimant's own soonest-to-lapse reservation-only row,
            // tie-broken by key so the choice never depends on `HashMap`
            // iteration order. `Principal::same`, never `may_displace`: this
            // must mean "mine" literally, or an operator-trust claimant would
            // rank every row in the map and take the oldest — the very
            // ranking the paragraph above refuses.
            //
            // Written as a fold rather than `min_by_key` so the tie-break
            // costs no allocation: `min_by_key` would clone every key it
            // compares, and this runs under the registry's write lock.
            let mine = {
                let mut best: Option<(&String, i64)> = None;
                for (k, e) in w.iter() {
                    if e.is_live(now) || !principal.same(&e.principal) {
                        continue;
                    }
                    let until = e.reserved_until();
                    let better = match best {
                        None => true,
                        Some((bk, buntil)) => (until, k.as_str()) < (buntil, bk.as_str()),
                    };
                    if better {
                        best = Some((k, until));
                    }
                }
                best.map(|(k, _)| k.clone())
            };
            match mine {
                Some(v) => {
                    w.remove(&v);
                }
                None => {
                    binding.count_registry_full();
                    tracing::warn!(
                        class = principal.class_str(),
                        ceiling = ceiling,
                        max_rows = MAX_ROWS,
                        route = route,
                        "ui-bridge binding: the app registry is at this principal's row ceiling and it holds no row of its own to give up; the claim is refused rather than displacing another principal's row (counted on /health uiBridgeBinding.registryFullRefusals)"
                    );
                    return Err(Refusal::registry_full());
                }
            }
        }

        w.insert(
            app.app_id.clone(),
            RegisteredApp {
                app,
                declared_origin,
                principal: principal.clone(),
                last_seen_ms: now,
                transport,
                websocket_conn_id,
                keep_alive_ms,
                released_at_ms: None,
            },
        );
        Ok(Claimed { keep_alive_ms })
    }

    /// Release `app_id` (R2).
    ///
    /// - `conn_guard: Some(c)` removes the entry only while `c` still owns it,
    ///   so a DISPLACED WebSocket's teardown cannot delete the connection that
    ///   displaced it. `None` is the HTTP `DELETE` path, which has no conn.
    /// - A caller that is neither the holder principal nor operator trust is
    ///   refused with `UIB_REGISTRATION_HELD`.
    /// - A browser or keyed holder's released id stays RESERVED on its own
    ///   row for `BINDING_TOMBSTONE_MS`; operator trust frees it at once.
    ///
    /// `Ok(false)` means "nothing to remove", which includes the conn-guard
    /// miss: that is not a refusal, it is a stale teardown.
    pub async fn release(
        &self,
        binding: &RelayBinding,
        app_id: &str,
        principal: &Principal,
        conn_guard: Option<u64>,
        route: &'static str,
    ) -> Result<bool, Refusal> {
        let mode = binding.config.binding;
        let mut w = self.inner.write().await;
        let Some(entry) = w.get(app_id) else {
            return Ok(false);
        };
        if let Some(conn_id) = conn_guard {
            if entry.websocket_conn_id != Some(conn_id) {
                return Ok(false);
            }
        }
        if entry.released_at_ms.is_some() {
            // Already released and sitting out its reservation.
            return Ok(false);
        }
        if mode != BindingMode::Off && !principal.may_displace(&entry.principal) {
            binding.meter(mode, principal, route, Refusal::registration_held(RULE_R2))?;
        }
        // Operator trust frees its id immediately; a browser or keyed holder
        // keeps it RESERVED, on the row, for `BINDING_TOMBSTONE_MS`. Either
        // way the row stops being live, so it leaves `list_live` and every
        // routing reader at once — a release is still a release.
        let holder_is_operator = entry.principal.is_operator_trust();
        if mode == BindingMode::Off || holder_is_operator {
            return Ok(w.remove(app_id).is_some());
        }
        let now = chrono::Utc::now().timestamp_millis();
        if let Some(e) = w.get_mut(app_id) {
            e.released_at_ms = Some(now);
        }
        Ok(true)
    }

    /// Lightweight liveness refresh — bumps `last_seen_ms` to "now" without
    /// requiring the caller to provide a full `DiscoveredApp`. Intended for
    /// the WS relay's hot path (every inbound frame and every outbound
    /// heartbeat tick) where cloning the `DiscoveredApp` for `claim` would
    /// be wasteful.
    ///
    /// `conn_guard: Some(c)` refreshes only while `c` still owns the entry
    /// (R2's liveness arm): a DISPLACED socket's frames and heartbeat ticks
    /// must not keep the holder's registration alive. `None` skips that check
    /// — the kill-switch-`off` path.
    ///
    /// The conn guard applies only to a `Websocket` entry: an `Http` row has
    /// no conn to compare against, so holding the guard against it would make
    /// every `touch` return `false` and let the row age out under a live
    /// socket (reachable whenever a same-origin phone-home re-registers an id
    /// a WebSocket holds, which sets `websocket_conn_id: None`).
    ///
    /// For that `Http` case the socket's own `principal` decides instead.
    /// "R1 already vetted whoever took the id" is NOT sufficient, and was
    /// wrong across a reservation lapse: an attacker that first-claimed an
    /// unheld id over WebSocket (an accepted non-goal) and keeps its socket
    /// open would otherwise refresh the VICTIM's row on every inbound frame
    /// once the victim re-registered that id over HTTP.
    ///
    /// Returns `true` if an entry was refreshed.
    pub async fn touch(
        &self,
        app_id: &str,
        conn_guard: Option<u64>,
        principal: &Principal,
    ) -> bool {
        let now = chrono::Utc::now().timestamp_millis();
        let mut w = self.inner.write().await;
        if let Some(entry) = w.get_mut(app_id) {
            if let Some(conn_id) = conn_guard {
                if entry.transport == AppTransport::Websocket {
                    if entry.websocket_conn_id != Some(conn_id) {
                        return false;
                    }
                } else if !principal.may_displace(&entry.principal) {
                    return false;
                }
            }
            entry.last_seen_ms = now;
            true
        } else {
            false
        }
    }

    /// Look up an entry by app_id REGARDLESS of freshness, reservation-only
    /// rows included.
    ///
    /// **Named the long way on purpose.** Round 4 converted every production
    /// reader to [`Self::get_live`], and there is now no production caller at
    /// all — the short name `get` was exactly what four routing readers
    /// reached for by reflex while a row could still be a dead reservation,
    /// which is how a crashed wrapper turned into 90 s of hard 400s. Phase 2
    /// adds tab-side readers with the same choice in front of them, so the
    /// name has to state which one this is.
    ///
    /// If you are routing, dispatching, listing or answering "is this app
    /// there?", you want [`Self::get_live`].
    pub async fn get_including_reservations(&self, app_id: &str) -> Option<RegisteredApp> {
        let r = self.inner.read().await;
        r.get(app_id).cloned()
    }

    /// Look up an entry only while it is FRESH, i.e. the same predicate
    /// `list_live` and `/ui-bridge/apps/registered` use.
    ///
    /// Routing reads this rather than [`Self::get`]. The registry now keeps an
    /// expired row for `BINDING_TOMBSTONE_MS` past its TTL so the id stays
    /// reserved for its holder; without this split that retention would also
    /// extend the window in which a dispatch targets an app that stopped
    /// heartbeating, from the old sweeper-tick skew to the full reservation.
    /// Reservation and reachability are different questions.
    pub async fn get_live(&self, app_id: &str) -> Option<RegisteredApp> {
        let now = chrono::Utc::now().timestamp_millis();
        let r = self.inner.read().await;
        r.get(app_id).filter(|e| e.is_live(now)).cloned()
    }

    /// Returns entries that haven't been stale-evicted (last_seen_ms within
    /// each entry's TTL — per-entry `keep_alive_ms` if set, else
    /// `REGISTRATION_TTL_MS`).
    pub async fn list_live(&self) -> Vec<RegisteredApp> {
        let now = chrono::Utc::now().timestamp_millis();
        let r = self.inner.read().await;
        r.values()
            // ONE definition of freshness, shared with `get_live` and the
            // reservation arm. A second inline copy here is what let a
            // released row keep appearing in `/ui-bridge/apps/registered`.
            .filter(|e| e.is_live(now))
            .cloned()
            .collect()
    }

    /// Drop rows whose RESERVATION has lapsed — see
    /// [`RegisteredApp::reserved_until`], which covers expiry AND explicit
    /// release. Returns the number dropped.
    ///
    /// The row IS the reservation, so this takes no binding and writes
    /// nothing: R5 cannot depend on when the sweeper happens to run, and
    /// there is no side map for a queued writer to race.
    ///
    /// Readers are unaffected: `list_live` and [`Self::get_live`] filter on
    /// `is_live`, which a retained row fails. A reservation-only row is
    /// visible only to [`Self::get`] and to `claim`'s reservation arm.
    pub async fn sweep(&self) -> usize {
        let now = chrono::Utc::now().timestamp_millis();
        let mut w = self.inner.write().await;
        let before = w.len();
        w.retain(|_, e| now <= e.reserved_until());
        before - w.len()
    }

    /// Test-only shorthand for the pre-binding `upsert`: an operator-trust
    /// claim under a default [`crate::mcp::relay_binding::BindingConfig`],
    /// which every rule admits. Sibling modules' tests register fixture apps
    /// by the dozen and none of them is about the binding — the rules
    /// themselves are exercised in `relay_binding/tests.rs`, over a real
    /// socket, behind the real origin guard. Production has exactly ONE write
    /// path into this map, [`Self::claim`].
    #[cfg(test)]
    pub async fn upsert(
        &self,
        app: DiscoveredApp,
        declared_origin: Option<String>,
        transport: AppTransport,
        websocket_conn_id: Option<u64>,
        keep_alive_ms: Option<i64>,
    ) {
        let binding = RelayBinding::new(crate::mcp::relay_binding::BindingConfig::default());
        let principal = Principal::OperatorTrust {
            class: crate::mcp::origin_guard::OriginClass::NonBrowser,
        };
        self.claim(
            &binding,
            &principal,
            "test",
            None,
            app,
            declared_origin,
            transport,
            websocket_conn_id,
            keep_alive_ms,
        )
        .await
        .expect("an operator-trust claim is always admitted");
    }

    /// Test-only shorthand for the pre-binding `remove`. See [`Self::upsert`].
    #[cfg(test)]
    pub async fn remove(&self, app_id: &str) -> bool {
        let binding = RelayBinding::new(crate::mcp::relay_binding::BindingConfig::default());
        let principal = Principal::OperatorTrust {
            class: crate::mcp::origin_guard::OriginClass::NonBrowser,
        };
        self.release(&binding, app_id, &principal, None, "test")
            .await
            .expect("an operator-trust release is always admitted")
    }

    /// Test-only: drop a row outright (no reservation), so a test can isolate
    /// a rule that must hold on the LIVE routing slot alone. Ageing a row cannot
    /// do this deterministically: a WebSocket holder's client auto-pongs the
    /// 20 s ping, and every inbound frame refreshes `last_seen_ms`, so a test
    /// that ages the row races that refresh and ends up exercising plain R1
    /// against a still-live row instead.
    #[cfg(test)]
    pub async fn test_drop_row(&self, app_id: &str) -> bool {
        let mut w = self.inner.write().await;
        w.remove(app_id).is_some()
    }

    /// Test-only helper that subtracts `delta_ms` from an entry's
    /// `last_seen_ms` to simulate the passage of time. Returns `true` if the
    /// entry exists. Used by integration tests in sibling modules
    /// (`app_discovery`) to drive freshness math without sleeping.
    #[cfg(test)]
    pub async fn test_age_entry(&self, app_id: &str, delta_ms: i64) -> bool {
        let mut w = self.inner.write().await;
        if let Some(entry) = w.get_mut(app_id) {
            entry.last_seen_ms -= delta_ms;
            true
        } else {
            false
        }
    }
}

/// Spawn a background sweeper. Call once from AppHandle setup.
pub fn spawn_sweeper(registry: Arc<AppRegistry>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(SWEEP_INTERVAL_MS));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let evicted = registry.sweep().await;
            if evicted > 0 {
                tracing::debug!(
                    "[app-registry] dropped {} row(s) whose reservation lapsed",
                    evicted
                );
            }
        }
    });
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    fn sample_app(app_id: &str) -> DiscoveredApp {
        DiscoveredApp {
            app_id: app_id.to_string(),
            app_name: "Sample".to_string(),
            app_type: "web".to_string(),
            framework: None,
            url: "http://127.0.0.1:3000".to_string(),
            port: 3000,
            base_path: "".to_string(),
            version: None,
            capabilities: vec![],
            element_count: None,
            component_count: None,
            discovered_at: 0,
        }
    }

    #[test]
    fn transport_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&AppTransport::Http).unwrap(),
            "\"http\""
        );
        assert_eq!(
            serde_json::to_string(&AppTransport::Websocket).unwrap(),
            "\"websocket\""
        );
    }

    #[test]
    fn transport_deserializes_lowercase() {
        let http: AppTransport = serde_json::from_str("\"http\"").unwrap();
        let ws: AppTransport = serde_json::from_str("\"websocket\"").unwrap();
        assert_eq!(http, AppTransport::Http);
        assert_eq!(ws, AppTransport::Websocket);
    }

    #[test]
    fn transport_default_is_http() {
        let t: AppTransport = Default::default();
        assert_eq!(t, AppTransport::Http);
    }

    #[tokio::test]
    async fn upsert_http_then_upgrade_to_websocket() {
        let reg = AppRegistry::new();
        reg.upsert(sample_app("a1"), None, AppTransport::Http, None, None)
            .await;
        let entry = reg.get_including_reservations("a1").await.unwrap();
        assert_eq!(entry.transport, AppTransport::Http);
        assert_eq!(entry.websocket_conn_id, None);

        reg.upsert(
            sample_app("a1"),
            None,
            AppTransport::Websocket,
            Some(42),
            None,
        )
        .await;
        let entry = reg.get_including_reservations("a1").await.unwrap();
        assert_eq!(entry.transport, AppTransport::Websocket);
        assert_eq!(entry.websocket_conn_id, Some(42));
    }

    #[tokio::test]
    async fn remove_returns_true_when_present() {
        let reg = AppRegistry::new();
        reg.upsert(sample_app("a1"), None, AppTransport::Http, None, None)
            .await;
        assert!(reg.remove("a1").await);
        assert!(!reg.remove("a1").await);
    }

    #[tokio::test]
    async fn list_live_filters_stale() {
        let reg = AppRegistry::new();
        reg.upsert(sample_app("a1"), None, AppTransport::Http, None, None)
            .await;
        // Manually backdate the entry.
        {
            let mut w = reg.inner.write().await;
            w.get_mut("a1").unwrap().last_seen_ms -= REGISTRATION_TTL_MS + 1;
        }
        assert!(reg.list_live().await.is_empty());
    }

    #[tokio::test]
    async fn touch_refreshes_last_seen_ms() {
        let reg = AppRegistry::new();
        reg.upsert(
            sample_app("a1"),
            None,
            AppTransport::Websocket,
            Some(1),
            None,
        )
        .await;
        // Backdate so the entry is "stale" and would be filtered by list_live().
        {
            let mut w = reg.inner.write().await;
            w.get_mut("a1").unwrap().last_seen_ms -= REGISTRATION_TTL_MS + 1;
        }
        assert!(
            reg.list_live().await.is_empty(),
            "precondition: backdated entry should not be live"
        );

        let agent = Principal::OperatorTrust {
            class: crate::mcp::origin_guard::OriginClass::NonBrowser,
        };
        let refreshed = reg.touch("a1", Some(1), &agent).await;
        assert!(refreshed, "touch must return true for the owning conn");
        assert!(
            !reg.touch("a1", Some(2), &agent).await,
            "R2: a displaced conn's touch must NOT refresh the holder"
        );

        let live = reg.list_live().await;
        assert_eq!(live.len(), 1, "touched entry should be live again");
        assert_eq!(live[0].app.app_id, "a1");

        // last_seen_ms should now be very close to "now".
        let now = chrono::Utc::now().timestamp_millis();
        assert!(
            now - live[0].last_seen_ms < 1_000,
            "touch should set last_seen_ms close to now (delta={}ms)",
            now - live[0].last_seen_ms
        );
    }

    #[tokio::test]
    async fn touch_returns_false_for_unknown_app() {
        let reg = AppRegistry::new();
        assert!(
            !reg.touch(
                "nope",
                None,
                &Principal::OperatorTrust {
                    class: crate::mcp::origin_guard::OriginClass::NonBrowser,
                }
            )
            .await,
            "touch on empty registry must return false"
        );
        // Sanity: registry is still empty (touch doesn't create entries).
        assert!(reg.list_live().await.is_empty());
    }

    #[tokio::test]
    async fn keep_alive_extends_freshness_beyond_default_ttl() {
        let reg = AppRegistry::new();
        // Register with a 5-minute keep-alive (300_000ms). An operator-trust
        // registrant is NOT capped by R5 — only a browser principal is.
        reg.upsert(
            sample_app("long-lived"),
            None,
            AppTransport::Http,
            None,
            Some(300_000),
        )
        .await;

        // Advance "synthetic time" past the global 30s TTL but well within
        // the per-entry 5-minute override. The entry MUST still be live.
        {
            let mut w = reg.inner.write().await;
            w.get_mut("long-lived").unwrap().last_seen_ms -= REGISTRATION_TTL_MS + 5_000;
        }

        let live = reg.list_live().await;
        assert_eq!(
            live.len(),
            1,
            "entry with 5min keep_alive must still be live after default TTL elapses"
        );
        assert_eq!(live[0].app.app_id, "long-lived");
        assert_eq!(live[0].keep_alive_ms, Some(300_000));

        // Sweep must NOT evict the entry either.
        let evicted = reg.sweep().await;
        assert_eq!(evicted, 0, "sweep must respect per-entry keep_alive_ms");
        assert!(reg.get_including_reservations("long-lived").await.is_some());
    }

    #[tokio::test]
    async fn keep_alive_still_evicted_once_its_own_window_elapses() {
        let reg = AppRegistry::new();
        // Short 2s keep-alive.
        reg.upsert(
            sample_app("brief"),
            None,
            AppTransport::Http,
            None,
            Some(2_000),
        )
        .await;

        // Push past 2s.
        {
            let mut w = reg.inner.write().await;
            w.get_mut("brief").unwrap().last_seen_ms -= 2_500;
        }

        assert!(
            reg.list_live().await.is_empty(),
            "entry past its own keep_alive_ms must be filtered"
        );
        // FRESHNESS and PRESENCE are now different questions. `list_live`
        // still filters at the entry's own keep-alive (asserted above, and it
        // is what `/ui-bridge/apps/registered` serves), but `sweep` retains
        // the row for BINDING_TOMBSTONE_MS beyond it, because the row IS the
        // R5 reservation — plan
        // `2026-09-17-ui-bridge-relay-registration-is-unauthenticated`. This
        // test asserted the old contract, where the two coincided.
        assert_eq!(
            reg.sweep().await,
            0,
            "a row inside its reservation window is retained, not swept"
        );
        assert!(
            reg.get_including_reservations("brief").await.is_some(),
            "…and it is still there to answer `who holds this id`"
        );

        // Past the reservation too: now it goes.
        {
            let mut w = reg.inner.write().await;
            w.get_mut("brief").unwrap().last_seen_ms -= BINDING_TOMBSTONE_MS;
        }
        assert_eq!(
            reg.sweep().await,
            1,
            "sweep must evict once the reservation has lapsed too"
        );
        assert!(reg.get_including_reservations("brief").await.is_none());
    }

    #[tokio::test]
    async fn keep_alive_none_uses_global_default() {
        let reg = AppRegistry::new();
        reg.upsert(sample_app("default"), None, AppTransport::Http, None, None)
            .await;

        // Backdate past the global TTL.
        {
            let mut w = reg.inner.write().await;
            w.get_mut("default").unwrap().last_seen_ms -= REGISTRATION_TTL_MS + 1;
        }

        assert!(
            reg.list_live().await.is_empty(),
            "None keep_alive_ms must fall through to global default"
        );
    }

    // ------------------------------------------------------------------
    // The row ceiling (R5's resource bound)
    // ------------------------------------------------------------------

    const VICTIM_ORIGIN: &str = "https://good.example";

    fn browser(origin: &str) -> Principal {
        Principal::Browser {
            class: crate::mcp::origin_guard::OriginClass::Foreign,
            origin: crate::mcp::origin_guard::NormOrigin::parse(origin).unwrap(),
        }
    }

    fn default_binding() -> Arc<RelayBinding> {
        RelayBinding::new(crate::mcp::relay_binding::BindingConfig::default())
    }

    /// Register `app_id` as `principal`, and optionally release it so the row
    /// is left as a reservation only.
    async fn claim_as(
        reg: &AppRegistry,
        binding: &RelayBinding,
        principal: &Principal,
        app_id: &str,
    ) -> Result<(), Refusal> {
        reg.claim(
            binding,
            principal,
            "test",
            None,
            sample_app(app_id),
            None,
            AppTransport::Http,
            None,
            None,
        )
        .await
        .map(|_| ())
    }

    /// Attacker origins that sort BOTH BELOW and ABOVE the victim's
    /// `https://good.example`, including `http://` ones (which sort below
    /// every `https://`).
    ///
    /// The spread is load-bearing history, not decoration. The rule this
    /// replaced ranked candidates by a comparison over OTHER principals'
    /// entries, and the repo's own fixture used `s{i}.evil.example`, which
    /// survived ONLY because `'g' < 's'`; flipping the prefix to `a{i}`
    /// evicted the victim on the first fresh write. A property that can pass
    /// by naming luck is not pinned, so every run spans the victim from both
    /// sides.
    ///
    /// The set is SMALL and reused on purpose: each origin ends up holding
    /// hundreds of rows, so the flood drives real self-evictions instead of
    /// stalling on the capacity refusal after one row apiece. That is what
    /// puts the eviction path under test while the victim's row is the oldest
    /// — the single row every "soonest to expire" ranking picked.
    const ATTACKER_ORIGINS: [&str; 8] = [
        "http://a.evil.example",
        "https://a.evil.example",
        "https://b.evil.example",
        "https://f.evil.example",
        "https://h.evil.example",
        "https://s.evil.example",
        "https://z.evil.example",
        "http://localhost:3001",
    ];

    fn attacker_origin(i: usize) -> &'static str {
        ATTACKER_ORIGINS[i % ATTACKER_ORIGINS.len()]
    }

    /// The victim's RESERVATION survives a flood that saturates the row
    /// ceiling, however the attackers' origins sort against it — and the id
    /// stays unclaimable by them while it does.
    #[tokio::test]
    async fn a_flood_at_the_row_ceiling_cannot_displace_another_principals_reservation() {
        let reg = AppRegistry::new();
        let binding = default_binding();
        let victim = browser(VICTIM_ORIGIN);

        // The victim registers and then releases — the well-behaved
        // `beforeunload` path. Its reservation is the OLDEST in the map, which
        // is exactly what every "evict the soonest to expire" rule picked.
        claim_as(&reg, &binding, &victim, "victim-app")
            .await
            .expect("the victim's own claim is admitted");
        assert!(reg
            .release(&binding, "victim-app", &victim, None, "test")
            .await
            .expect("the holder may release its own id"));

        // Well past the ceiling, from origins on both sides of the victim's.
        for i in 0..(MAX_ROWS + 1024) {
            let p = browser(attacker_origin(i));
            // Admitted or refused for capacity — either is fine. What is not
            // fine is the victim paying for it.
            let _ = claim_as(&reg, &binding, &p, &format!("squat-{i}")).await;
            let _ = reg
                .release(&binding, &format!("squat-{i}"), &p, None, "test")
                .await;
        }

        // m5: the flood must actually have EXERCISED self-eviction rather than
        // stalling on the capacity refusal after one row apiece — otherwise a
        // change that simply refused every post-ceiling claim would leave this
        // test green while testing nothing. Each attacker origin holds
        // hundreds of rows here, so every one of them has a row of its own to
        // give up and none is ever refused.
        assert_eq!(
            binding.health_json()["registryFullRefusals"],
            0,
            "no attacker was refused, so every post-ceiling claim self-evicted: {}",
            binding.health_json()
        );
        assert_eq!(
            reg.inner.read().await.len(),
            MAX_ROWS - OPERATOR_HEADROOM,
            "the browser ceiling bounds the map"
        );
        assert!(
            reg.get_including_reservations("victim-app").await.is_some(),
            "the victim's reservation row was evicted by a flood of other principals"
        );
        let evil = browser("https://evil.example");
        let refusal = claim_as(&reg, &binding, &evil, "victim-app")
            .await
            .expect_err("the victim's reserved id must still be held");
        assert_eq!(
            refusal.code,
            crate::mcp::relay_binding::CODE_REGISTRATION_HELD
        );
        claim_as(&reg, &binding, &victim, "victim-app")
            .await
            .expect("the holder must get its own reserved id back");
    }

    /// The same flood must not reach a LIVE row either. A ceiling that falls
    /// back to "evict the oldest live row" would be an R1 bypass by flooding:
    /// the victim's row is the oldest by construction.
    #[tokio::test]
    async fn a_flood_at_the_row_ceiling_cannot_evict_a_live_holder() {
        let reg = AppRegistry::new();
        let binding = default_binding();
        let victim = browser(VICTIM_ORIGIN);
        claim_as(&reg, &binding, &victim, "victim-app")
            .await
            .expect("the victim's own claim is admitted");

        // Live rows, never released, so nothing in the map is ever
        // reservation-only and the ceiling has no lawful victim at all.
        for i in 0..(MAX_ROWS + 1024) {
            let _ = claim_as(
                &reg,
                &binding,
                &browser(attacker_origin(i)),
                &format!("live-{i}"),
            )
            .await;
        }

        // m5: the mirror assertion. Nothing here is ever reservation-only, so
        // no attacker has a row of its own to give up and every post-ceiling
        // claim must be REFUSED. A zero here would mean the flood never
        // reached the ceiling and the test proved nothing.
        assert!(
            binding.health_json()["registryFullRefusals"]
                .as_u64()
                .unwrap_or(0)
                > 0,
            "the flood never reached the ceiling: {}",
            binding.health_json()
        );
        assert!(
            reg.get_live("victim-app").await.is_some(),
            "a live holder was evicted to make room for another principal's row"
        );
        let evil = browser("https://evil.example");
        let refusal = claim_as(&reg, &binding, &evil, "victim-app")
            .await
            .expect_err("a live holder is not displaceable");
        assert_eq!(
            refusal.code,
            crate::mcp::relay_binding::CODE_REGISTRATION_HELD
        );
    }

    /// M2: the autonomy invariant. A browser flood of LIVE rows must never
    /// deny an AGENT a registration.
    ///
    /// This is the case self-eviction alone could not carry, and it is not
    /// hypothetical: an attacker page registers `MAX_ROWS` fresh ids and
    /// re-POSTs each inside the 30 s TTL — roughly 137 req/s on loopback from
    /// one tab — so every row stays LIVE, nothing is reservation-only, and
    /// nothing is self-evictable by anyone. Under the ceiling this replaces,
    /// an agent's claim then got `UIB_REGISTRY_FULL` indefinitely, which is
    /// `agent_flow_unchanged`'s very first step denied by a web page.
    ///
    /// The headroom fixes it by RESERVING capacity rather than widening who
    /// may evict whom — the latter would put back a ranking over other
    /// principals' rows, and every such ranking has been measured steerable.
    #[tokio::test]
    async fn at_the_row_ceiling_operator_trust_is_not_starved_by_a_live_browser_flood() {
        let reg = AppRegistry::new();
        let binding = default_binding();

        // Fill the browser ceiling with rows that are all LIVE and never
        // released, from origins on both sides of any victim's.
        for i in 0..(MAX_ROWS - OPERATOR_HEADROOM) {
            claim_as(
                &reg,
                &binding,
                &browser(attacker_origin(i)),
                &format!("live-{i}"),
            )
            .await
            .expect("filling below the browser ceiling is admitted");
        }

        // A browser is now refused — that is the ceiling doing its job.
        let refusal = claim_as(&reg, &binding, &browser("https://evil.example"), "fresh")
            .await
            .expect_err("a browser principal is held to MAX_ROWS - OPERATOR_HEADROOM");
        assert_eq!(refusal.code, crate::mcp::relay_binding::CODE_REGISTRY_FULL);

        // …and the agent is NOT. This is the assertion the whole headroom
        // exists for.
        let agent = Principal::OperatorTrust {
            class: crate::mcp::origin_guard::OriginClass::NonBrowser,
        };
        for i in 0..OPERATOR_HEADROOM {
            claim_as(&reg, &binding, &agent, &format!("agent-{i}"))
                .await
                .unwrap_or_else(|e| {
                    panic!("a live browser flood starved operator trust at row {i}: {e:?}")
                });
        }
        assert_eq!(
            reg.inner.read().await.len(),
            MAX_ROWS,
            "the headroom is exactly the slice between the two ceilings"
        );

        // An agent past the FULL ceiling is refused too — the headroom is a
        // reservation, not an exemption, and it still evicts nobody.
        let refusal = claim_as(&reg, &binding, &agent, "agent-overflow")
            .await
            .expect_err("MAX_ROWS bounds operator trust as well");
        assert_eq!(refusal.code, crate::mcp::relay_binding::CODE_REGISTRY_FULL);
        assert!(
            reg.get_live("live-0").await.is_some(),
            "no browser row was evicted to serve operator trust"
        );
    }

    /// M3: at the FULL ceiling, operator trust is REFUSED rather than ranking
    /// other principals' rows.
    ///
    /// This is the case that discriminates `Principal::same` from
    /// `may_displace` in the self-eviction scan, and it exists because a
    /// mutant proved nothing else did: flipping `same` back to `may_displace`
    /// left every other test in this module green. `may_displace` is true for
    /// operator trust against EVERYONE, so "give up a row of its own"
    /// silently becomes "take the globally soonest-to-lapse row" — which is
    /// the oldest, which is the victim's, which is exactly the steerable rule
    /// this round deleted from the tombstone map. The headroom keeps an agent
    /// from ever needing that, and this test keeps the scan honest when the
    /// headroom is itself exhausted.
    ///
    /// Reaching the state takes both principals: browsers cannot pass
    /// `MAX_ROWS - OPERATOR_HEADROOM`, so the last rows must be the agent's
    /// own, and they are LIVE, so the agent has nothing of its own to give up
    /// either.
    #[tokio::test]
    async fn at_the_full_ceiling_operator_trust_is_refused_rather_than_ranking_other_rows() {
        let reg = AppRegistry::new();
        let binding = default_binding();
        let filler = browser("https://filler.example");
        let agent = Principal::OperatorTrust {
            class: crate::mcp::origin_guard::OriginClass::NonBrowser,
        };

        // Browser rows, all RELEASED, so every one is reservation-only and
        // would be a lawful victim for any rule that ranked other principals.
        for i in 0..(MAX_ROWS - OPERATOR_HEADROOM) {
            claim_as(&reg, &binding, &filler, &format!("f-{i}"))
                .await
                .expect("filling the browser ceiling is admitted");
            assert!(reg
                .release(&binding, &format!("f-{i}"), &filler, None, "test")
                .await
                .unwrap());
        }
        // The agent fills its own headroom with LIVE rows, so it has nothing
        // reservation-only of its own.
        for i in 0..OPERATOR_HEADROOM {
            claim_as(&reg, &binding, &agent, &format!("a-{i}"))
                .await
                .expect("the headroom is the agent's to use");
        }
        assert_eq!(
            reg.inner.read().await.len(),
            MAX_ROWS,
            "precondition: the map is exactly full"
        );
        let refusals_before = binding.health_json()["registryFullRefusals"]
            .as_u64()
            .expect("the counter must be served");

        // The agent asks for one more. `same` refuses; `may_displace` would
        // quietly evict the oldest BROWSER reservation and admit it.
        let refusal = claim_as(&reg, &binding, &agent, "a-overflow")
            .await
            .expect_err("operator trust ranked another principal's rows instead of being refused");
        assert_eq!(refusal.code, crate::mcp::relay_binding::CODE_REGISTRY_FULL);
        assert_eq!(
            binding.health_json()["registryFullRefusals"]
                .as_u64()
                .unwrap_or(0),
            refusals_before + 1,
            "the refusal was not counted: {}",
            binding.health_json()
        );
        assert_eq!(
            reg.inner.read().await.len(),
            MAX_ROWS,
            "a row was evicted to serve the refused claim"
        );
        // Every browser reservation is still there — none was ranked, let
        // alone taken.
        for i in 0..(MAX_ROWS - OPERATOR_HEADROOM) {
            assert!(
                reg.get_including_reservations(&format!("f-{i}"))
                    .await
                    .is_some(),
                "operator trust took browser reservation f-{i}"
            );
        }
    }

    /// The saturated arm, stated exactly: a principal with nothing of its own
    /// to give up is REFUSED (`UIB_REGISTRY_FULL`, 503) rather than taking
    /// someone else's row — while a principal that does hold a lapsing row of
    /// its own keeps being admitted by self-eviction, so the ceiling is not a
    /// self-DoS for a legitimately churning app.
    #[tokio::test]
    async fn at_the_row_ceiling_a_principal_with_nothing_of_its_own_is_refused() {
        let reg = AppRegistry::new();
        let binding = default_binding();
        let filler = browser("https://filler.example");

        for i in 0..(MAX_ROWS - OPERATOR_HEADROOM) {
            claim_as(&reg, &binding, &filler, &format!("f-{i}"))
                .await
                .expect("filling below the browser ceiling is admitted");
            assert!(reg
                .release(&binding, &format!("f-{i}"), &filler, None, "test")
                .await
                .unwrap());
        }

        // A principal holding nothing: refused, not served at someone's cost.
        let newcomer = browser("https://newcomer.example");
        let refusal = claim_as(&reg, &binding, &newcomer, "fresh")
            .await
            .expect_err("a full registry must refuse rather than displace");
        assert_eq!(refusal.code, crate::mcp::relay_binding::CODE_REGISTRY_FULL);
        assert_eq!(
            refusal.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(binding.health_json()["registryFullRefusals"], 1);
        // This one asserts an ABSENCE, so it is the shape that can pass while
        // observing nothing: `["rules"]["capacity"]` is `Null` both when the
        // key is correctly absent AND when `rules` itself has gone. Pin the
        // container first, so the assertion can only be satisfied by a `rules`
        // object that really is there and really lacks `capacity`.
        let health = binding.health_json();
        assert!(
            health["rules"].is_object(),
            "the rules block itself is missing, so the absence below proves nothing: {health}"
        );
        // …and make the absence MEANINGFUL rather than vacuous: record a real
        // rule verdict on this same binding, so `rules` demonstrably carries
        // rule entries while still carrying no `capacity` one. Without this,
        // an empty `rules` would satisfy the assertion below while proving
        // only that nothing at all had been counted.
        binding
            .counters
            .record(crate::mcp::relay_binding::RULE_R1, true);
        let health = binding.health_json();
        assert_eq!(
            health["rules"]["R1"]["refused"], 1,
            "the control entry did not land, so the absence below is vacuous: {health}"
        );
        assert!(
            health["rules"]["capacity"].is_null(),
            "a capacity refusal is an operational event, not a rule verdict: {health}"
        );

        // …while the principal that owns the lapsing rows keeps going, paying
        // out of its own.
        claim_as(&reg, &binding, &filler, "f-new")
            .await
            .expect("self-eviction must keep a churning holder admitted");
        assert_eq!(
            reg.inner.read().await.len(),
            MAX_ROWS - OPERATOR_HEADROOM,
            "the browser ceiling still bounds the map"
        );

        // And an operator-trust claim is never starved — here against a map
        // of RESERVATION-only browser rows. The live-row case, which
        // self-eviction alone could not carry, is
        // `at_the_row_ceiling_operator_trust_is_not_starved_by_a_live_browser_flood`.
        let agent = Principal::OperatorTrust {
            class: crate::mcp::origin_guard::OriginClass::NonBrowser,
        };
        // M3: the agent must be admitted into its HEADROOM, evicting NOBODY —
        // `mine` compares `Principal::same`, not `may_displace`, so operator
        // trust ranks nobody else's rows.
        //
        // Measured by the row COUNT, not by naming a survivor: the filler's
        // own `f-new` claim above legitimately self-evicted its oldest row, so
        // an assertion that `f-0` survives blames the agent for what the
        // filler did — that is exactly how this assertion first went red, and
        // it was the assertion that was wrong, not the code.
        let before = reg.inner.read().await.len();
        claim_as(&reg, &binding, &agent, "agent-app")
            .await
            .expect("operator trust must not be locked out by a browser flood");
        assert_eq!(
            reg.inner.read().await.len(),
            before + 1,
            "the agent displaced a row instead of taking its headroom"
        );
    }
}
