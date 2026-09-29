//! Stamping a session's OWNING tenant into owner-only outbox rows at enqueue.
//!
//! Plan
//! `2026-09-28-anyone-holding-a-session-uuid-can-write-its-transcript-because-session-output-and-event-writes-are-anonymous`,
//! Phase 2 ("Stamp the tenant at enqueue").
//!
//! The drain picks each row's credential slot with
//! [`super::coord_sync`]'s `record_session_tenant`: the payload's top-level
//! `tenant_id` first, the live [`super::SessionRegistry`] second. After a
//! runner restart the registry no longer holds the session, so on a device
//! bound to more than one tenant an UNSTAMPED row resolves
//! [`crate::auth::TenantScope::Unresolved`] forever — no credential is
//! presentable for it, and it can only be held until it ages out. Stamping the
//! tenant while the session is still known makes the row self-describing.
//!
//! The transcript lane already stamps through
//! [`crate::claude_session::coord_register::AiCoordRegistrar::stamp_tenant`]
//! (its sessions live in that registrar, not in the session registry). This
//! module is the stamp for the two `/events` producers —
//! [`super::restore_record_emitter`] and [`super::coord_transport_rung`] —
//! whose session ids may belong to EITHER registry, so `main.rs` installs one
//! lookup that asks both.
//!
//! The carrier key is the same top-level `tenant_id` the registrar uses. It is
//! runner-internal: the `/events` arms strip it before the payload goes on the
//! wire ([`strip_tenant_carrier`]), so coord's stored event bodies are unchanged.

use std::sync::{Arc, OnceLock};

use serde_json::Value as JsonValue;
use uuid::Uuid;

/// Resolves a coord session id to the tenant that owns it, or `None` when this
/// process does not know it.
pub type TenantLookup = Arc<dyn Fn(Uuid) -> Option<Uuid> + Send + Sync>;

/// The payload key the drain reads the stamped tenant from.
pub const TENANT_CARRIER_KEY: &str = "tenant_id";

static LOOKUP: OnceLock<TenantLookup> = OnceLock::new();

/// Install the process-wide lookup. `false` if one was already installed (the
/// first is kept). Called once from `main.rs`, after both the session registry
/// and the AI-session registrar exist.
pub fn install(lookup: TenantLookup) -> bool {
    LOOKUP.set(lookup).is_ok()
}

/// The owning tenant of `session_id` through the installed lookup; `None` when
/// none is installed (unit tests, a runner whose session subsystem never came
/// up) or the session is unknown.
pub fn lookup(session_id: Uuid) -> Option<Uuid> {
    LOOKUP.get().and_then(|f| f(session_id))
}

/// Stamp `tenant` into `payload` as the carrier key. A payload that already
/// names a tenant keeps it (its producer knew better), and a non-object
/// payload or an unknown tenant is returned unchanged.
pub fn stamp(mut payload: JsonValue, tenant: Option<Uuid>) -> JsonValue {
    if let (Some(t), Some(obj)) = (tenant, payload.as_object_mut()) {
        obj.entry(TENANT_CARRIER_KEY)
            .or_insert_with(|| JsonValue::String(t.to_string()));
    }
    payload
}

/// The payload as it goes on the wire: the runner-internal tenant carrier
/// removed. Only for payloads forwarded VERBATIM (the `/events` arms); the
/// output-chunk body is rebuilt from named fields and never carried it.
pub fn strip_tenant_carrier(payload: &JsonValue) -> JsonValue {
    let mut out = payload.clone();
    if let Some(obj) = out.as_object_mut() {
        obj.remove(TENANT_CARRIER_KEY);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stamp_adds_the_tenant_and_never_overwrites_one() {
        let t = Uuid::now_v7();
        assert_eq!(
            stamp(json!({"a": 1}), Some(t)),
            json!({"a": 1, "tenant_id": t.to_string()})
        );
        let theirs = Uuid::now_v7();
        assert_eq!(
            stamp(json!({"tenant_id": theirs.to_string()}), Some(t)),
            json!({"tenant_id": theirs.to_string()}),
            "a producer's own tenant wins"
        );
        assert_eq!(stamp(json!({"a": 1}), None), json!({"a": 1}));
        assert_eq!(stamp(json!("scalar"), Some(t)), json!("scalar"));
    }

    #[test]
    fn the_wire_payload_never_carries_the_stamp() {
        let t = Uuid::now_v7();
        let stamped = stamp(json!({"provider": "claude"}), Some(t));
        assert_eq!(
            strip_tenant_carrier(&stamped),
            json!({"provider": "claude"})
        );
    }
}
