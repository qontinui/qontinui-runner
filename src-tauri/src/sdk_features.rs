//! SDK feature inventory baked at compile time.
//!
//! Lists the `@qontinui/ui-bridge` primitives this runner binary's embedded
//! frontend is known to support. **Bump when a new SDK feature lands** —
//! the staleness detection only works if this list mirrors the SDK's
//! actual capabilities at build time.
//!
//! Surfaced on `/health` as the top-level `sdkFeatures` array (sibling to
//! `data`, `uiBridge`, `timestamp` — matching the supervisor's `/health`
//! envelope). Test drivers compare against the features they need; an
//! absent feature means the binary predates that feature's SDK release.
//!
//! **Mixed-category flags.** Entries here cover both transport-level
//! primitives (e.g. `softNavigate`, `tabActivation`, `flatErrorEnvelope`)
//! AND data-shape contracts the host emits in its responses
//! (e.g. `snapshotF3`, `snapshotCanonicalElements`). Test drivers can
//! `sdkFeatures.includes("snapshotF3")` to feature-detect the snapshot
//! shape instead of probing field presence. See [`SDK_FEATURE_DOC_URL`]
//! for the canonical reference of every flag.

pub const SDK_FEATURES: &[&str] = &[
    "softNavigate",
    "snapshotActiveTab",
    "snapshotRegistration",
    "tabActivation",
    "flatErrorEnvelope",
    "actionOverlay",
    "bookmarksSingleton",
    "findBroadened",
    "waitForElement",
    "stubRegistry",
    "stubVerify",
    "pagePlaybook",
    "snapshotAvailableTabs",
    "componentTree",
    "errorClosestMatches",
    "frontendReadyFlag",
    // Snapshot-shape contracts (data-shape, not transport-level)
    // F3 metadata in snapshot envelope: registration{totalRegistered,
    // everHadRegistrations, byRoute} + route + snapshotTakenAtMs.
    // Added 2026-04-24 (ui-bridge commit d50ce72); full coverage
    // 2026-04-25 (a8a4bb4 patched the relay handler).
    "snapshotF3",
    // Snapshot elements use the canonical SDK serialization (bbox,
    // identifier, tagName, stableRef, kind, category, visible, origin,
    // route) rather than the legacy minimal {id, type, label, actions,
    // state} shape. Added 2026-04-26 via the Phase 1+6 audit fix.
    "snapshotCanonicalElements",
];

pub const SDK_FEATURE_DOC_URL: &str =
    "https://github.com/qontinui/ui-bridge/blob/main/docs-site/docs/api/runner-features.md";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_features_lists_post_2026_04_25_primitives() {
        assert!(SDK_FEATURES.contains(&"softNavigate"));
        assert!(SDK_FEATURES.contains(&"snapshotRegistration"));
        assert!(SDK_FEATURES.contains(&"actionOverlay"));
        assert!(SDK_FEATURES.contains(&"waitForElement"));
        assert!(!SDK_FEATURES.is_empty());
    }
}
