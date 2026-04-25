//! SDK feature inventory baked at compile time.
//!
//! Lists the `@qontinui/ui-bridge` primitives this runner binary's embedded
//! frontend is known to support. **Bump when a new SDK feature lands** —
//! the staleness detection only works if this list mirrors the SDK's
//! actual capabilities at build time.
//!
//! Surfaced on `/health` as the `sdkFeatures` array. Test drivers compare
//! against the features they need; an absent feature means the binary
//! predates that feature's SDK release.

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
