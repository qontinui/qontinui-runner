//! Shared helpers for building enriched component-error messages, used by
//! both the runner's outer-wrapper handler at `/ui-bridge/sdk/component/:id`
//! (in `mcp/sdk_client.rs`) and the runner's inner Rust HTTP handler at
//! `/ui-bridge/control/component/:id` (in `mcp/ui_bridge/elements.rs`).
//!
//! Mirrors the shape produced by `buildComponentNotFoundError` in the SDK
//! package: includes `Available components: [...]`, the boilerplate
//! "Components are only available when their page is active" hint, plus a
//! cross-route suggestion when the missing id appears in
//! `registration.byRoute[other].ids` (Phase 1.2's per-route id list).
//!
//! Phase 1.1 (plan 2026-05-03) — extracted from `sdk_client.rs` so the
//! runner's two component-not-found code paths stay in lock-step.

/// Build an enriched "component not found" error string from a UI Bridge
/// snapshot. Returns the canonical message shape produced by
/// `buildComponentNotFoundError` in the SDK package.
pub fn build_component_not_found_message(id: &str, snapshot: &serde_json::Value) -> String {
    let available: Vec<&str> = snapshot
        .get("components")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.get("id").and_then(|v| v.as_str()))
                .collect()
        })
        .unwrap_or_default();

    let current_route = snapshot
        .get("route")
        .and_then(|v| v.as_str())
        .unwrap_or("(unknown)");

    let mut other_route_hint = String::new();
    if let Some(by_route) = snapshot
        .get("registration")
        .and_then(|r| r.get("byRoute"))
        .and_then(|v| v.as_object())
    {
        for (route, entry) in by_route {
            if route == current_route {
                continue;
            }
            let ids = entry.get("ids").and_then(|v| v.as_array());
            if let Some(ids) = ids {
                let hit = ids
                    .iter()
                    .any(|v| v.as_str().map(|s| s == id).unwrap_or(false));
                if hit {
                    other_route_hint = format!(
                        " (registered on route '{}', current route is '{}')",
                        route, current_route
                    );
                    break;
                }
            }
        }
    }

    format!(
        "Component \"{}\" not found{}. Available components: [{}]. \
         Components are only available when their page is active — \
         navigate to the page that contains this component and try again.",
        id,
        other_route_hint,
        available.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::build_component_not_found_message;
    use serde_json::json;

    #[test]
    fn includes_available_components_list() {
        let snap = json!({
            "components": [
                { "id": "alpha" },
                { "id": "beta" },
            ],
            "route": "/home",
            "registration": { "byRoute": {} },
        });
        let msg = build_component_not_found_message("missing", &snap);
        assert!(msg.contains("Component \"missing\" not found"));
        assert!(msg.contains("Available components: [alpha, beta]"));
        assert!(msg.contains("Components are only available when their page is active"));
        // No other-route hint when id appears nowhere.
        assert!(!msg.contains("registered on route"));
    }

    #[test]
    fn surfaces_cross_route_hint_when_id_lives_elsewhere() {
        let snap = json!({
            "components": [{ "id": "alpha" }],
            "route": "/home",
            "registration": {
                "byRoute": {
                    "/home": { "count": 1, "ids": ["alpha"] },
                    "/settings": { "count": 1, "ids": ["target-id"] },
                }
            },
        });
        let msg = build_component_not_found_message("target-id", &snap);
        assert!(
            msg.contains("registered on route '/settings'"),
            "expected cross-route hint, got: {msg}"
        );
        assert!(msg.contains("current route is '/home'"));
    }

    #[test]
    fn unknown_route_falls_back_gracefully() {
        let snap = json!({
            "components": [],
            "registration": { "byRoute": {} },
        });
        let msg = build_component_not_found_message("anything", &snap);
        assert!(msg.contains("Available components: []"));
    }

    #[test]
    fn empty_snapshot_does_not_panic() {
        let msg = build_component_not_found_message("x", &json!({}));
        assert!(msg.contains("Component \"x\" not found"));
        assert!(msg.contains("Available components: []"));
    }
}
