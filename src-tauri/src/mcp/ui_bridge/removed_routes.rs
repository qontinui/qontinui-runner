//! Routes the UI Bridge deliberately deleted, and what replaced each one.
//!
//! An agent that calls a deleted route used to get the same bare
//! `No route for POST /ui-bridge/control/screenshot` as a typo, and the
//! discovery hint pointed it at `GET /ui-bridge/_routes`, where nothing is
//! called "screenshot". This table lets the 404 fallback
//! (`mcp_api::not_found_message`) say the route was removed and name its
//! replacement, so the caller does not have to rediscover the vocabulary.
//!
//! Source of truth for the entries: the deletion record at the top of
//! `screenshots.rs` (vision-pipeline plan, Phase 2), which moved every
//! runner-direct screenshot / element-image / stuck-screen / media-audit
//! handler onto `/ui-bridge/vision/*` (`vision_routes.rs`).
//!
//! The 404's status and `NOT_FOUND` code are unchanged; only the message text
//! gains the replacement. A path listed here must never also be served — the
//! test `no_removed_route_is_served` pins that against `route_manifest()`.

/// The replacement every deleted capture route points at.
const VISION_CAPTURE: &str = "POST /ui-bridge/vision/capture \
     (bytes at GET /ui-bridge/vision/cache/{sha256})";

/// The replacement for a deleted element-crop route: the same capture call,
/// scoped by its `element` or `region` field.
const VISION_CAPTURE_ELEMENT: &str = "POST /ui-bridge/vision/capture \
     with an `element` id or `region` for the crop \
     (bytes at GET /ui-bridge/vision/cache/{sha256})";

/// The replacement for the deleted `/ai/media/*` family.
const VISION_FAMILY: &str = "the /ui-bridge/vision/* family, starting at \
     POST /ui-bridge/vision/capture \
     (bytes at GET /ui-bridge/vision/cache/{sha256})";

/// `(path, replacement)`. A `path` ending in `*` matches every path with that
/// prefix; any other `path` matches exactly (a trailing `/` on the request is
/// ignored). Matching is method-independent: the route is gone for every
/// method.
pub(crate) const REMOVED_UI_BRIDGE_ROUTES: &[(&str, &str)] = &[
    ("/ui-bridge/control/screenshot", VISION_CAPTURE),
    ("/ui-bridge/control/annotated-screenshot", VISION_CAPTURE),
    (
        "/ui-bridge/control/element-screenshot",
        VISION_CAPTURE_ELEMENT,
    ),
    (
        "/ui-bridge/control/capture-element-images",
        VISION_CAPTURE_ELEMENT,
    ),
    (
        "/ui-bridge/control/get-element-images",
        VISION_CAPTURE_ELEMENT,
    ),
    ("/ui-bridge/control/diagnose-stuck-screen", VISION_CAPTURE),
    ("/ui-bridge/control/diagnose-stuck", VISION_CAPTURE),
    ("/ui-bridge/ai/element-screenshot", VISION_CAPTURE_ELEMENT),
    ("/ui-bridge/sdk/screenshot", VISION_CAPTURE),
    ("/ui-bridge/ai/media/*", VISION_FAMILY),
];

/// The replacement for `path` when it names a deliberately removed UI Bridge
/// route, `None` otherwise.
pub(crate) fn removed_route_replacement(path: &str) -> Option<&'static str> {
    let trimmed = match path.strip_suffix('/') {
        Some(t) if !t.is_empty() => t,
        _ => path,
    };
    REMOVED_UI_BRIDGE_ROUTES
        .iter()
        .find(|(removed, _)| match removed.strip_suffix('*') {
            Some(prefix) => path.starts_with(prefix),
            None => trimmed == *removed,
        })
        .map(|(_, replacement)| *replacement)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_names_vision_capture() {
        for (path, replacement) in REMOVED_UI_BRIDGE_ROUTES {
            assert!(
                replacement.contains("POST /ui-bridge/vision/capture"),
                "{path}: replacement must name POST /ui-bridge/vision/capture, got {replacement:?}"
            );
            assert!(
                replacement.contains("GET /ui-bridge/vision/cache/{sha256}"),
                "{path}: replacement must say where the bytes are, got {replacement:?}"
            );
        }
    }

    #[test]
    fn exact_entries_match_exactly() {
        assert!(removed_route_replacement("/ui-bridge/control/screenshot").is_some());
        assert!(removed_route_replacement("/ui-bridge/control/screenshot/").is_some());
        // A served sibling that merely shares a prefix is not "removed".
        assert!(removed_route_replacement("/ui-bridge/control/screenshots").is_none());
        assert!(removed_route_replacement("/ui-bridge/sdk/diagnose-stuck").is_none());
        assert!(removed_route_replacement("/ui-bridge/vision/capture").is_none());
    }

    #[test]
    fn prefix_entry_matches_the_family() {
        assert_eq!(
            removed_route_replacement("/ui-bridge/ai/media/audit/accessibility"),
            Some(VISION_FAMILY)
        );
        assert!(removed_route_replacement("/ui-bridge/ai/mediax").is_none());
    }

    /// The table must never claim a route the runner still serves: that would
    /// be a lie in the 404 path's vocabulary (and the 404 could never fire
    /// for it anyway, so the entry would be dead). Compared against the same
    /// manifest `GET /ui-bridge/_routes` serves.
    #[test]
    fn no_removed_route_is_served() {
        let served: Vec<&str> = super::super::route_manifest()
            .iter()
            .map(|(_, p)| *p)
            .filter(|p| removed_route_replacement(p).is_some())
            .collect();
        assert!(
            served.is_empty(),
            "REMOVED_UI_BRIDGE_ROUTES names routes the runner still serves: {served:?}"
        );
        assert!(
            super::super::route_manifest().len() > 100,
            "route_manifest() unexpectedly small — the check above would be vacuous"
        );
    }
}
