//! The preflight's view of rehoming: every session NOT in a holder must be
//! idle and moved into one before the cut-over, or the upgrade waits (plan D7
//! "Upgrade rule"; served `production-and-cost` `runner-self-upgrade`: "every
//! non-holder session idle and rehomed (a busy one defers the upgrade, and is
//! never killed mid-turn)").
//!
//! The rehome OPERATION — end a session at its turn boundary and resume the
//! same id into a holder in the same tab — is plan Phase 5b
//! (`rehome_non_holder_sessions(...) -> RehomeReport`). This module is the
//! contract the preflight reads it through, so wiring Phase 5b is one line in
//! [`active_rehomer`]. Until then the build has no rehome, and a non-holder
//! session present at preflight is UNKNOWN — it blocks, it is never assumed
//! safe and never killed.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;

use super::record::ResumeSpec;

/// What one rehome pass did — Phase 5b's report, field for field.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct RehomeReport {
    /// Sessions now holder-backed (by session id, else pid).
    pub rehomed: Vec<String>,
    /// Busy at the deadline: the upgrade defers and they are reported by
    /// name. Never killed.
    pub busy_deferred: Vec<String>,
    /// `(session, why)`.
    pub failed: Vec<(String, String)>,
}

/// The rehome step's outcome as the preflight sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RehomeOutcome {
    /// No session outside a holder: nothing to rehome.
    NothingToRehome,
    Report(RehomeReport),
    /// The step could not run (this build has no rehome, or it errored).
    Unavailable {
        reason: String,
    },
}

/// The rehome operation.
#[async_trait]
pub trait Rehomer: Send + Sync {
    async fn rehome_non_holder_sessions(
        &self,
        sessions: &[ResumeSpec],
    ) -> Result<RehomeReport, String>;
}

/// This build carries no rehome operation (plan Phase 5b has not landed on
/// it): every call answers why, and the preflight reads that as UNKNOWN.
pub struct NoRehomeOnThisBuild;

#[async_trait]
impl Rehomer for NoRehomeOnThisBuild {
    async fn rehome_non_holder_sessions(
        &self,
        sessions: &[ResumeSpec],
    ) -> Result<RehomeReport, String> {
        Err(format!(
            "{} session(s) run outside a PTY holder and this build has no rehome operation \
             (plan Phase 5b) to move them into one",
            sessions.len()
        ))
    }
}

/// THE wire point: the rehome the preflight calls. Phase 5b replaces the body
/// with its `rehome_non_holder_sessions` adapter.
pub fn active_rehomer() -> Arc<dyn Rehomer> {
    Arc::new(NoRehomeOnThisBuild)
}

/// Run the rehome over the `DIES-WITH-RUNNER` set.
pub async fn rehome(rehomer: &dyn Rehomer, sessions: &[ResumeSpec]) -> RehomeOutcome {
    if sessions.is_empty() {
        return RehomeOutcome::NothingToRehome;
    }
    match rehomer.rehome_non_holder_sessions(sessions).await {
        Ok(r) => RehomeOutcome::Report(r),
        Err(reason) => RehomeOutcome::Unavailable { reason },
    }
}
