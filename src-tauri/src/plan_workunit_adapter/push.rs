//! Work-unit push client (Phase 2).
//!
//! Turns a parsed work-unit ([`super::parser::ParsedWorkUnit`]) into calls
//! against the P1 work-unit API:
//!
//! - `POST /coord/work-units/upsert`           — slug-keyed upsert
//! - `POST /coord/work-units/:slug/transition` — guarded status transition
//! - `GET  /coord/work-units?slug_prefix=…`    — read current status
//!
//! All authed with the runner's device-JWT via [`crate::auth::attach_device_auth`]
//! (the same bearer the rest of the runner's coord calls present) — the runner
//! step is server-side, so it holds the device JWT directly and does NOT need
//! the loopback write-forwarder (which serves `register-plan`/`attest` and —
//! since the device-session coord-surface-hardening follow-up — the work-unit
//! registry routes, for nonce-only in-terminal sessions).
//!
//! ## Edge-triggered + idempotent
//!
//! The push carries a client-side **last-applied-status** memory (the
//! `last_applied` argument, threaded by [`super::trigger`]), mirroring coord's
//! old `ingested_status` edge-trigger but now on the client. A re-push of an
//! unchanged file yields NO transition (no phantom history row); only a status
//! that differs from what we last applied emits a `transition` call.
//!
//! ## Loud conflict surfacing
//!
//! Before applying a status edge we read the remote status. If it diverged from
//! what we last applied (someone — e.g. an agent — transitioned the unit
//! directly), we surface it LOUDLY (warn + a conflict counter in
//! [`super::trigger`]) and let the file win, exactly as coord's worker did.

use super::parser::ParsedWorkUnit;
use anyhow::{Context, Result};
use serde::Serialize;

/// Actor stamped on adapter-driven upserts/transitions.
pub const ADAPTER_ACTOR: &str = "harness-markdown-adapter";

/// True when `by_actor` denotes a real (non-proxy) actor that owns the unit —
/// i.e. anything other than this adapter's own actor (and not empty). Used to
/// decide whether the markdown proxy should DEFER its transition so it does not
/// collapse an agent-initiated transition back to the system actor.
fn is_real_agent_actor(by_actor: &str) -> bool {
    by_actor != ADAPTER_ACTOR && !by_actor.is_empty()
}

/// `POST /coord/work-units/upsert` body. Mirrors coord's `UpsertRequest`;
/// `None` fields are omitted so an upsert that only refreshes title/metadata
/// does not clobber `status`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UpsertBody {
    pub slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by_actor: Option<String>,
}

/// `POST /coord/work-units/:slug/transition` body. Mirrors coord's
/// `TransitionRequest`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TransitionBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_status: Option<String>,
    pub to_status: String,
    pub by_actor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Build the `metadata` JSON pushed alongside the work-unit: the enrichment
/// coord's old slug+status projection discarded — phase sub-units, dependency
/// edges, and the source-file back-link.
pub fn build_metadata(u: &ParsedWorkUnit) -> serde_json::Value {
    serde_json::json!({
        "depends_on": u.depends_on,
        "phases": u
            .phases
            .iter()
            .map(|p| serde_json::json!({"index": p.index, "name": p.name}))
            .collect::<Vec<_>>(),
        "source_path": u.source_path,
    })
}

/// The pure edge-trigger decision: given the file's parsed status and the
/// status we last applied for this slug, what should the push do?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushAction {
    /// No prior application by us — upsert WITH status (creates the row /
    /// sets the initial status).
    UpsertWithStatus,
    /// Status unchanged since our last application — refresh title/metadata
    /// only; no transition, no history row (idempotent re-sync).
    RefreshOnly,
    /// The file's status changed since our last application — transition.
    Transition { from: String, to: String },
}

/// Pure edge-trigger: mirrors coord's `decide_status_action`, client-side.
pub fn decide_push(parsed_status: &str, last_applied: Option<&str>) -> PushAction {
    match last_applied {
        None => PushAction::UpsertWithStatus,
        Some(prev) if prev == parsed_status => PushAction::RefreshOnly,
        Some(prev) => PushAction::Transition {
            from: prev.to_string(),
            to: parsed_status.to_string(),
        },
    }
}

/// What a single [`push_work_unit`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcomeKind {
    Created,
    Refreshed,
    Transitioned { from: String, to: String },
}

/// Result of pushing one work-unit, including whether a remote conflict was
/// detected (a direct write diverged from our last-applied status).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushOutcome {
    pub slug: String,
    pub kind: PushOutcomeKind,
    pub conflict: bool,
}

/// Outcome of a [`WorkUnitSink::set_deps`] call. Distinguishes the benign
/// "table not migrated yet" 503 (the edge table hasn't landed — the
/// `metadata.depends_on` JSONB fallback covers it, so this is NOT an error)
/// from a real applied write or a hard error (returned as `Err`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetDepsOutcome {
    /// Edges were set (2xx). `edges_set` is coord's reported count.
    Ok { edges_set: u64 },
    /// Coord returned 503 — the `work_unit_deps` table is not yet migrated.
    /// Benign: the JSONB fallback in `metadata.depends_on` covers dependencies
    /// until the migration lands.
    TableNotMigrated,
}

/// The coord side of a push, abstracted so [`push_work_unit`] is testable
/// without live HTTP. Implemented by [`HttpWorkUnitSink`] in production and a
/// fake in tests.
#[async_trait::async_trait]
pub trait WorkUnitSink: Send + Sync {
    /// Current opaque status of the unit, or `None` if it doesn't exist yet.
    async fn current_status(&self, slug: &str) -> Result<Option<String>>;
    /// The `by_actor` of the unit's most-recent status-history row, or None if
    /// the unit has no history. Used to defer when a real (non-proxy) actor owns
    /// the unit. Reads GET /coord/work-units/<slug>/history (newest-first).
    async fn last_actor(&self, slug: &str) -> Result<Option<String>>;
    async fn upsert(&self, body: &UpsertBody) -> Result<()>;
    async fn transition(&self, slug: &str, body: &TransitionBody) -> Result<()>;
    /// Replace the complete upstream dependency set of `slug` in coord's
    /// first-class edge table (`POST /coord/work-units/:slug/deps`). This is a
    /// REPLACE-SET: `depends_on` is the full upstream set; `&[]` clears all.
    /// Idempotent, so re-sending an unchanged set is harmless.
    async fn set_deps(&self, slug: &str, depends_on: &[String]) -> Result<SetDepsOutcome>;
}

/// Push one parsed work-unit through the edge-trigger + conflict logic.
///
/// `last_applied` is the status this adapter last applied for `u.slug` (its
/// client-side memory). Returns the [`PushOutcome`]; the caller updates its
/// last-applied memory to `u.status` on success.
pub async fn push_work_unit<S: WorkUnitSink + ?Sized>(
    sink: &S,
    u: &ParsedWorkUnit,
    last_applied: Option<&str>,
) -> Result<PushOutcome> {
    let metadata = build_metadata(u);
    let action = decide_push(&u.status, last_applied);

    // Deferral (graduation-bootstrap P2a): only a `Transition` would OVERWRITE
    // an existing unit's status. Before emitting it, read the unit's latest
    // status-history `by_actor`; if a real (non-adapter) actor last drove the
    // unit, a real agent owns its lifecycle now — DEFER (skip this cycle's
    // transition, emit nothing) so we don't collapse the agent's transition back
    // to the system actor. A brand-new unit (`UpsertWithStatus`) or an idempotent
    // `RefreshOnly` has no agent owner to defer to, so those are never gated.
    if let PushAction::Transition { .. } = &action {
        if let Some(actor) = sink.last_actor(&u.slug).await? {
            if is_real_agent_actor(&actor) {
                tracing::info!(
                    slug = %u.slug,
                    last_actor = %actor,
                    "markdown proxy defers: real agent owns this unit"
                );
                return Ok(PushOutcome {
                    slug: u.slug.clone(),
                    kind: PushOutcomeKind::Refreshed,
                    conflict: false,
                });
            }
        }
    }

    // Conflict detection: did the remote status diverge from what we last
    // applied? (A direct transition by someone else.) File wins, but loudly.
    let mut conflict = false;
    if let Some(prev) = last_applied {
        if let Ok(Some(remote)) = sink.current_status(&u.slug).await {
            if remote != prev {
                conflict = true;
                tracing::warn!(
                    slug = %u.slug,
                    last_applied = %prev,
                    remote = %remote,
                    parsed = %u.status,
                    "plan adapter: remote work-unit status diverged from last-applied; \
                     file wins (loud override)"
                );
            }
        }
    }

    let kind = match &action {
        PushAction::UpsertWithStatus => {
            sink.upsert(&UpsertBody {
                slug: u.slug.clone(),
                title: u.title.clone(),
                status: Some(u.status.clone()),
                metadata: Some(metadata),
                by_actor: Some(ADAPTER_ACTOR.to_string()),
            })
            .await?;
            PushOutcomeKind::Created
        }
        PushAction::RefreshOnly => {
            sink.upsert(&UpsertBody {
                slug: u.slug.clone(),
                title: u.title.clone(),
                status: None,
                metadata: Some(metadata),
                by_actor: Some(ADAPTER_ACTOR.to_string()),
            })
            .await?;
            PushOutcomeKind::Refreshed
        }
        PushAction::Transition { from, to } => {
            // Refresh title/metadata first (no status change), then transition
            // so the history row carries the from->to edge.
            sink.upsert(&UpsertBody {
                slug: u.slug.clone(),
                title: u.title.clone(),
                status: None,
                metadata: Some(metadata),
                by_actor: Some(ADAPTER_ACTOR.to_string()),
            })
            .await?;
            sink.transition(
                &u.slug,
                &TransitionBody {
                    // On a detected conflict the CAS would fail (remote != from),
                    // so drop the guard and let the row's current status be the
                    // from_status — the file still wins.
                    from_status: if conflict { None } else { Some(from.clone()) },
                    to_status: to.clone(),
                    by_actor: ADAPTER_ACTOR.to_string(),
                    reason: Some(format!("plan file status edge: {from} -> {to}")),
                },
            )
            .await?;
            PushOutcomeKind::Transitioned {
                from: from.clone(),
                to: to.clone(),
            }
        }
    };

    Ok(PushOutcome {
        slug: u.slug.clone(),
        kind,
        conflict,
    })
}

/// Production [`WorkUnitSink`]: HTTP against coord with the device-JWT bearer.
pub struct HttpWorkUnitSink {
    base: String,
    client: reqwest::Client,
}

impl HttpWorkUnitSink {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            client: reqwest::Client::new(),
        }
    }

    /// Resolve from the active coord profile (env `COORD_HTTP_URL` or the
    /// profile's `coord_url`). `None` when coord is unconfigured — the trigger
    /// then no-ops, never localhost-guessing.
    pub fn from_profile() -> Option<Self> {
        match crate::profiles::resolve_coord_base() {
            crate::profiles::CoordBase::Configured(base) => Some(Self::new(base)),
            _ => None,
        }
    }
}

#[async_trait::async_trait]
impl WorkUnitSink for HttpWorkUnitSink {
    async fn current_status(&self, slug: &str) -> Result<Option<String>> {
        // Slugs are URL-safe kebab tokens, so inline them into the query
        // string (avoids depending on `RequestBuilder::query`, which is
        // version-fragile in this tree).
        let url = format!(
            "{}/coord/work-units?slug_prefix={}&limit=500",
            self.base, slug
        );
        let resp = crate::auth::attach_device_auth(self.client.get(&url))
            .send()
            .await
            .context("GET /coord/work-units")?;
        if !resp.status().is_success() {
            anyhow::bail!("GET /coord/work-units returned {}", resp.status());
        }
        let body: serde_json::Value = resp.json().await.context("parse work-units list")?;
        // Tolerant of array or {units|work_units: [...]} envelope.
        let rows = match &body {
            serde_json::Value::Array(a) => a.clone(),
            serde_json::Value::Object(o) => o
                .get("units")
                .or_else(|| o.get("work_units"))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        for row in rows {
            if row.get("slug").and_then(|s| s.as_str()) == Some(slug) {
                return Ok(row
                    .get("status")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string()));
            }
        }
        Ok(None)
    }

    async fn last_actor(&self, slug: &str) -> Result<Option<String>> {
        // GET /coord/work-units/<slug>/history returns
        // {"work_unit_id":..,"slug":..,"history":[{..,"by_actor":..,"to_status":..,
        //  "transitioned_at":..}, ...]} ordered newest-first (coord's SQL
        // `ORDER BY transitioned_at DESC`). We want the newest row's `by_actor`.
        let url = format!("{}/coord/work-units/{}/history", self.base, slug);
        let resp = crate::auth::attach_device_auth(self.client.get(&url))
            .send()
            .await
            .context("GET /coord/work-units/:slug/history")?;
        // No such unit yet ⇒ no history ⇒ no owner to defer to.
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            anyhow::bail!(
                "GET /coord/work-units/:slug/history returned {}",
                resp.status()
            );
        }
        let body: serde_json::Value = resp.json().await.context("parse work-unit history")?;
        // Newest-first: the first `history` element is the most-recent transition.
        // `by_actor` is nullable (serialized as JSON null) — `as_str` yields None,
        // and an empty history array yields None, both meaning "no owner".
        let by_actor = body
            .get("history")
            .and_then(|h| h.as_array())
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("by_actor"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        Ok(by_actor)
    }

    async fn upsert(&self, body: &UpsertBody) -> Result<()> {
        let url = format!("{}/coord/work-units/upsert", self.base);
        let resp = crate::auth::attach_device_auth(self.client.post(&url).json(body))
            .send()
            .await
            .context("POST /coord/work-units/upsert")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("upsert {} -> {} {}", body.slug, status, text);
        }
        Ok(())
    }

    async fn transition(&self, slug: &str, body: &TransitionBody) -> Result<()> {
        let url = format!("{}/coord/work-units/{}/transition", self.base, slug);
        let resp = crate::auth::attach_device_auth(self.client.post(&url).json(body))
            .send()
            .await
            .context("POST /coord/work-units/:slug/transition")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("transition {} -> {} {}", slug, status, text);
        }
        Ok(())
    }

    async fn set_deps(&self, slug: &str, depends_on: &[String]) -> Result<SetDepsOutcome> {
        let url = format!("{}/coord/work-units/{}/deps", self.base, slug);
        let body = serde_json::json!({ "depends_on": depends_on });
        let resp = crate::auth::attach_device_auth(self.client.post(&url).json(&body))
            .send()
            .await
            .context("POST /coord/work-units/:slug/deps")?;
        let status = resp.status();
        // 503 => the edge table hasn't been migrated yet; benign, the JSONB
        // fallback in metadata.depends_on covers it.
        if status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            return Ok(SetDepsOutcome::TableNotMigrated);
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("set_deps {} -> {} {}", slug, status, text);
        }
        let parsed: serde_json::Value = resp.json().await.unwrap_or_default();
        let edges_set = parsed
            .get("edges_set")
            .and_then(|v| v.as_u64())
            .unwrap_or(depends_on.len() as u64);
        Ok(SetDepsOutcome::Ok { edges_set })
    }
}

#[cfg(test)]
mod tests {
    use super::super::parser::{ParsedPhase, ParsedWorkUnit};
    use super::*;
    use std::sync::Mutex;

    fn unit(slug: &str, status: &str) -> ParsedWorkUnit {
        ParsedWorkUnit {
            slug: slug.to_string(),
            title: Some("T".to_string()),
            status: status.to_string(),
            depends_on: vec!["2026-01-01-dep".to_string()],
            phases: vec![ParsedPhase {
                index: 1,
                name: "Phase 1 — x".to_string(),
            }],
            source_path: format!("plans/{slug}.md"),
            content: String::new(),
        }
    }

    #[test]
    fn decide_push_edge_trigger() {
        assert_eq!(decide_push("vetted", None), PushAction::UpsertWithStatus);
        assert_eq!(
            decide_push("vetted", Some("vetted")),
            PushAction::RefreshOnly
        );
        assert_eq!(
            decide_push("shipped", Some("vetted")),
            PushAction::Transition {
                from: "vetted".to_string(),
                to: "shipped".to_string()
            }
        );
    }

    #[test]
    fn build_metadata_carries_enrichment() {
        let m = build_metadata(&unit("s", "vetted"));
        assert_eq!(m["depends_on"][0], "2026-01-01-dep");
        assert_eq!(m["phases"][0]["index"], 1);
        assert_eq!(m["source_path"], "plans/s.md");
    }

    #[test]
    fn upsert_body_omits_none_status() {
        let b = UpsertBody {
            slug: "s".to_string(),
            title: None,
            status: None,
            metadata: None,
            by_actor: None,
        };
        let j = serde_json::to_value(&b).unwrap();
        assert_eq!(j, serde_json::json!({"slug": "s"}));
    }

    #[derive(Default)]
    struct FakeSink {
        remote: Option<String>,
        /// Configured `by_actor` of the unit's latest history row (default None).
        last_actor: Option<String>,
        upserts: Mutex<Vec<UpsertBody>>,
        transitions: Mutex<Vec<(String, TransitionBody)>>,
        deps_calls: Mutex<Vec<(String, Vec<String>)>>,
    }

    #[async_trait::async_trait]
    impl WorkUnitSink for FakeSink {
        async fn current_status(&self, _slug: &str) -> Result<Option<String>> {
            Ok(self.remote.clone())
        }
        async fn last_actor(&self, _slug: &str) -> Result<Option<String>> {
            Ok(self.last_actor.clone())
        }
        async fn upsert(&self, body: &UpsertBody) -> Result<()> {
            self.upserts.lock().unwrap().push(body.clone());
            Ok(())
        }
        async fn transition(&self, slug: &str, body: &TransitionBody) -> Result<()> {
            self.transitions
                .lock()
                .unwrap()
                .push((slug.to_string(), body.clone()));
            Ok(())
        }
        async fn set_deps(&self, slug: &str, depends_on: &[String]) -> Result<SetDepsOutcome> {
            self.deps_calls
                .lock()
                .unwrap()
                .push((slug.to_string(), depends_on.to_vec()));
            Ok(SetDepsOutcome::Ok {
                edges_set: depends_on.len() as u64,
            })
        }
    }

    #[tokio::test]
    async fn fake_sink_set_deps_records_call_and_returns_ok() {
        let sink = FakeSink::default();
        let out = sink
            .set_deps("p4", &["p1".to_string(), "p2".to_string()])
            .await
            .unwrap();
        assert_eq!(out, SetDepsOutcome::Ok { edges_set: 2 });
        let calls = sink.deps_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "p4");
        assert_eq!(calls[0].1, vec!["p1".to_string(), "p2".to_string()]);
    }

    #[tokio::test]
    async fn first_push_creates_with_status_no_transition() {
        let sink = FakeSink::default();
        let out = push_work_unit(&sink, &unit("s", "vetted"), None)
            .await
            .unwrap();
        assert_eq!(out.kind, PushOutcomeKind::Created);
        assert!(!out.conflict);
        let ups = sink.upserts.lock().unwrap();
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0].status.as_deref(), Some("vetted"));
        assert!(sink.transitions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unchanged_status_refreshes_only_no_transition() {
        let sink = FakeSink {
            remote: Some("vetted".to_string()),
            ..Default::default()
        };
        let out = push_work_unit(&sink, &unit("s", "vetted"), Some("vetted"))
            .await
            .unwrap();
        assert_eq!(out.kind, PushOutcomeKind::Refreshed);
        assert!(!out.conflict);
        // Refresh upsert carries NO status (doesn't clobber), no transition.
        let ups = sink.upserts.lock().unwrap();
        assert_eq!(ups.len(), 1);
        assert!(ups[0].status.is_none());
        assert!(sink.transitions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn status_edge_emits_transition_with_cas() {
        let sink = FakeSink {
            remote: Some("vetted".to_string()),
            ..Default::default()
        };
        let out = push_work_unit(&sink, &unit("s", "shipped"), Some("vetted"))
            .await
            .unwrap();
        assert_eq!(
            out.kind,
            PushOutcomeKind::Transitioned {
                from: "vetted".to_string(),
                to: "shipped".to_string()
            }
        );
        assert!(!out.conflict);
        let trs = sink.transitions.lock().unwrap();
        assert_eq!(trs.len(), 1);
        assert_eq!(trs[0].1.from_status.as_deref(), Some("vetted")); // CAS guard set
        assert_eq!(trs[0].1.to_status, "shipped");
    }

    #[tokio::test]
    async fn remote_divergence_is_conflict_and_drops_cas() {
        // We last applied "vetted", but remote is "in_progress" (someone moved
        // it). File says "shipped": conflict surfaced, file wins, CAS dropped.
        let sink = FakeSink {
            remote: Some("in_progress".to_string()),
            ..Default::default()
        };
        let out = push_work_unit(&sink, &unit("s", "shipped"), Some("vetted"))
            .await
            .unwrap();
        assert!(out.conflict);
        assert_eq!(
            out.kind,
            PushOutcomeKind::Transitioned {
                from: "vetted".to_string(),
                to: "shipped".to_string()
            }
        );
        let trs = sink.transitions.lock().unwrap();
        assert_eq!(trs[0].1.from_status, None); // CAS dropped on conflict
        assert_eq!(trs[0].1.to_status, "shipped");
    }
}
