//! Typed resolution of this machine's tenant pin.
//!
//! Plan: `2026-08-05-runner-memory-injection-and-tenant-fail-closed` Phase 2.
//!
//! ## Why a type and not an `Option<Uuid>`
//!
//! `session::dual_write::resolve_active_tenant_id` collapses **five**
//! distinct outcomes into one `None`: no home dir, missing file, unparseable
//! JSON, missing `active_tenant_id` field, unparseable UUID. Two of those are a
//! correctly-configured single-tenant install (its `machine.json` legitimately
//! has no field, and MSI ships it that way); the other three are a machine that
//! cannot state its tenant at all. Collapsing them is why the proxy's
//! fail-closed decision could not be made without either refusing legitimate
//! installs or letting broken ones through.
//!
//! [`TenantPin`] separates them:
//!
//! | Variant | Meaning | Proxy behavior |
//! |---|---|---|
//! | [`TenantPin::Pinned`] | an explicit `active_tenant_id` | that tenant's slot |
//! | [`TenantPin::Unpinned`] | file readable, field simply absent | the default credential slot |
//! | [`TenantPin::Unresolvable`] | no home dir / unreadable / unparseable / malformed UUID | **refuse**, unless the device JWT's own `tenant_id` claim resolves it |
//!
//! The "Proxy behavior" column above is this type's own contract — a fixed
//! mapping from *how this machine's pin classifies* to *what selecting a
//! credential should do about it*. It is deliberately narrower than the
//! proxy's actual, request-time selection rule, which also weighs a session's
//! frozen-at-mint binding pin ahead of the live read above: see
//! [`crate::coord_mcp::resolve_session_tenant`]
//! (`2026-08-31-coord-mcp-credential-selection-by-binding-provenance` Phase 1b)
//! for that full authority order, kept in one place rather than duplicated
//! here so the two cannot drift against each other again.
//!
//! ## Unpinned is NOT a failure
//!
//! `Unpinned` is the single-tenant operator's normal state and must keep
//! working. As of Phase 1b it resolves to the *default* credential slot
//! (`crate::auth::device_bearer_for(None)`) rather than to the device JWT's
//! `tenant_id` claim — that fallback is reserved for `Unresolvable`, the arm
//! that has no other route to a tenant at all. Before Phase 1b, `Unpinned`
//! itself fell back to the claim (verified on a machine with no
//! `machine.json` at all, whose runner still served the correct tenant); that
//! behavior moved, it was not removed.
//!
//! ## Deliberately NOT in `dual_write`
//!
//! The old resolver lives in `session::dual_write`, which is Phase-10
//! cutover scaffolding kept "self-contained for Phase 9 deletion". This type is
//! an authorization primitive that must outlive that scaffolding, so it lives
//! here and `dual_write`'s function delegates to it.

use uuid::Uuid;

/// What this machine can say about its own tenant.
///
/// See the module docs for the mapping from the five raw failure modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TenantPin {
    /// `machine.json` carried a well-formed `active_tenant_id`.
    Pinned(Uuid),
    /// `machine.json` was readable and parseable, but carried no
    /// `active_tenant_id` (or carried an explicit JSON `null`). The legitimate
    /// single-tenant shape — resolve via the device JWT's claim, never refuse.
    Unpinned,
    /// This machine cannot state its tenant: no home dir, the file is missing
    /// or unreadable, it is not valid JSON, or `active_tenant_id` is present
    /// but not a parseable UUID. The only variant that fails closed.
    Unresolvable,
}

impl TenantPin {
    /// The pinned tenant, if there is one. `Unpinned` and `Unresolvable` both
    /// yield `None` — callers that must distinguish them have to match on the
    /// variant, which is the entire point of this type.
    pub fn pinned(self) -> Option<Uuid> {
        match self {
            TenantPin::Pinned(t) => Some(t),
            TenantPin::Unpinned | TenantPin::Unresolvable => None,
        }
    }

    /// Whether this pin state must fail closed at a credential-selection site.
    pub fn is_unresolvable(self) -> bool {
        matches!(self, TenantPin::Unresolvable)
    }
}

/// Resolve this machine's tenant pin from `~/.qontinui/machine.json`.
///
/// The five raw outcomes map per the module docs. Note the asymmetry that makes
/// this worth typing: a *missing field* is `Unpinned` (fine), while a *malformed
/// value* for that same field is `Unresolvable` (refuse) — a machine that tried
/// to state its tenant and produced garbage is not the same as one that never
/// tried.
pub fn resolve_tenant_pin() -> TenantPin {
    // `None` folds the two I/O failures (no home dir, unreadable/missing file)
    // into the single input `pin_from_bytes` classifies, so every one of the
    // five raw outcomes is reachable from a test without touching `$HOME`.
    let bytes = dirs::home_dir()
        .map(|home| home.join(".qontinui").join("machine.json"))
        .and_then(|path| std::fs::read(path).ok());
    pin_from_bytes(bytes.as_deref())
}

/// Classify the raw bytes of `machine.json`.
///
/// `None` means the file could not be read at all (no home dir, missing file,
/// permissions) — indistinguishable to us and identically `Unresolvable`.
pub(crate) fn pin_from_bytes(bytes: Option<&[u8]>) -> TenantPin {
    let Some(bytes) = bytes else {
        return TenantPin::Unresolvable;
    };
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(value) => parse_pin_from_value(&value),
        Err(_) => TenantPin::Unresolvable,
    }
}

/// The parse half of [`resolve_tenant_pin`], split out so the field/UUID
/// asymmetry is testable without touching the filesystem or `$HOME`.
pub(crate) fn parse_pin_from_value(value: &serde_json::Value) -> TenantPin {
    match value.get("active_tenant_id") {
        // Absent, or an explicit null: the operator never stated a tenant.
        None | Some(serde_json::Value::Null) => TenantPin::Unpinned,
        Some(v) => match v.as_str() {
            Some(raw) => match Uuid::parse_str(raw) {
                Ok(t) => TenantPin::Pinned(t),
                // Present but malformed — a stated tenant we cannot honor.
                Err(_) => TenantPin::Unresolvable,
            },
            // Present but not even a string (a number, an object): same class.
            None => TenantPin::Unresolvable,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> serde_json::Value {
        serde_json::from_str(s).expect("test fixture must be valid JSON")
    }

    #[test]
    fn well_formed_field_is_pinned() {
        let id = Uuid::new_v4();
        let got = parse_pin_from_value(&v(&format!(r#"{{"active_tenant_id":"{id}"}}"#)));
        assert_eq!(got, TenantPin::Pinned(id));
        assert_eq!(got.pinned(), Some(id));
        assert!(!got.is_unresolvable());
    }

    #[test]
    fn absent_field_is_unpinned_not_unresolvable() {
        // The MSI shape. This MUST NOT fail closed — it is the legitimate
        // single-tenant install, and refusing it was option (A) the plan rejected.
        let got = parse_pin_from_value(&v(r#"{"device_id":"abc","hostname":"msi"}"#));
        assert_eq!(got, TenantPin::Unpinned);
        assert!(!got.is_unresolvable());
    }

    #[test]
    fn explicit_null_field_is_unpinned() {
        assert_eq!(
            parse_pin_from_value(&v(r#"{"active_tenant_id":null}"#)),
            TenantPin::Unpinned
        );
    }

    #[test]
    fn malformed_uuid_is_unresolvable() {
        // Asymmetry under test: a STATED tenant we cannot honor is not the same
        // as an unstated one, even though both used to collapse to `None`.
        let got = parse_pin_from_value(&v(r#"{"active_tenant_id":"not-a-uuid"}"#));
        assert_eq!(got, TenantPin::Unresolvable);
        assert!(got.is_unresolvable());
    }

    #[test]
    fn non_string_field_is_unresolvable() {
        assert_eq!(
            parse_pin_from_value(&v(r#"{"active_tenant_id":12345}"#)),
            TenantPin::Unresolvable
        );
    }

    // ---- the two I/O arms, reachable via `pin_from_bytes` ----

    #[test]
    fn unreadable_or_absent_file_is_unresolvable() {
        // Covers BOTH "no home dir" and "missing file": `resolve_tenant_pin`
        // folds them into `None` before calling here.
        assert_eq!(pin_from_bytes(None), TenantPin::Unresolvable);
    }

    #[test]
    fn malformed_json_is_unresolvable() {
        assert_eq!(
            pin_from_bytes(Some(b"{ this is not json")),
            TenantPin::Unresolvable
        );
    }

    #[test]
    fn all_five_raw_outcomes_are_classified() {
        // The plan's Phase 2 acceptance: five inputs, three classes, and the
        // two legitimate ones must NOT be Unresolvable.
        let id = Uuid::new_v4();
        let cases: [(&str, Option<&[u8]>, TenantPin); 5] = [
            ("no home dir / missing file", None, TenantPin::Unresolvable),
            ("malformed JSON", Some(b"{ nope"), TenantPin::Unresolvable),
            (
                "absent active_tenant_id field",
                Some(br#"{"device_id":"d"}"#),
                TenantPin::Unpinned,
            ),
            (
                "malformed UUID",
                Some(br#"{"active_tenant_id":"xyz"}"#),
                TenantPin::Unresolvable,
            ),
            (
                "well-formed pin",
                None, // replaced below (needs the runtime uuid)
                TenantPin::Pinned(id),
            ),
        ];
        for (name, bytes, want) in cases.iter().take(4) {
            assert_eq!(pin_from_bytes(*bytes), *want, "case: {name}");
        }
        let pinned = format!(r#"{{"active_tenant_id":"{id}"}}"#);
        assert_eq!(
            pin_from_bytes(Some(pinned.as_bytes())),
            TenantPin::Pinned(id),
            "case: well-formed pin"
        );
    }

    #[test]
    fn pinned_accessor_is_none_for_both_non_pinned_variants() {
        assert_eq!(TenantPin::Unpinned.pinned(), None);
        assert_eq!(TenantPin::Unresolvable.pinned(), None);
    }
}
