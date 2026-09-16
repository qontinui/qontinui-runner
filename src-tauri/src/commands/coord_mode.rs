//! `get_coord_mode` — the config-derived coord connection mode the frontend
//! `CoordModeContext` renders its connected/isolated surfaces from.
//!
//! Rehomed from `commands::productivity` by Phase 3 of
//! `2026-09-12-consolidate-local-orchestration-onto-conductor`; the command
//! name is unchanged so `src/contexts/CoordModeContext.tsx` needs no edit.

/// `get_coord_mode` — the runner's connected-vs-isolated mode, for the
/// frontend (plan `2026-08-18-runner-embedded-pg-parity-and-coord-http-migration`
/// §6.4).
///
/// Returns `{ mode: "connected" | "isolated", base: string | null, source }`.
/// `base` is populated iff `mode === "connected"`, and is the same string every
/// Option-family coord call site in the backend resolves — so a surface the UI
/// renders as enabled is one the backend will actually reach.
///
/// The isolated arm is the point: the plan resolves isolated-mode UI as showing
/// the plan / task / review / worktree surfaces **disabled**, with an explicit
/// "connect a qontinui account to enable" reason, driven by this same
/// config-derived mode rather than by a failed request. Deriving it from
/// reachability instead would flip a connected runner to isolated on any coord
/// blip, and would disable the surfaces mid-session.
///
/// `source` distinguishes the two isolated reasons, which need different
/// operator messages: `dev_localhost_fallback` = no account configured (the
/// "connect a qontinui account" copy), `unknown_tier_prod_default` =
/// `settings.json` could not be read, so the tier is unknown and the runner is
/// fail-closed until that file is fixed.
#[tauri::command]
pub fn get_coord_mode() -> qontinui_runner_lib::profiles::CoordModeReport {
    qontinui_runner_lib::profiles::coord_mode()
}

// ---------------------------------------------------------------------------
// get_coord_mode — the connected-vs-isolated seam the frontend consumes.
//
// The MAPPING itself (tier / env / profile ⇒ connected | isolated, and which
// `source` each arm reports) is pinned hermetically in
// `profiles.rs::tests::coord_mode_*`, which can isolate `QONTINUI_CONFIG_DIR`
// behind the lib's `env_lock`. These cover what those cannot: that the command
// is a faithful projection, and that its WIRE shape is the one the TypeScript
// side destructures.
//
// Deliberately ONE `get_coord_mode()` call per test. The command reads process
// env, and sibling tests in this binary mutate `COORD_HTTP_URL`; asserting
// across two reads would be a flake, not a stronger test.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod coord_mode_tests {
    use super::*;
    use qontinui_runner_lib::profiles::{CoordConnectionMode, CoordModeReport};

    /// `base` is populated iff the mode is `Connected`. A UI that enables a
    /// surface then finds `base === null` would fall back to some other
    /// endpoint — exactly the hardcoded-dev-localhost behaviour this seam
    /// exists to remove.
    #[test]
    fn base_is_present_exactly_when_connected() {
        let got = get_coord_mode();
        assert_eq!(
            got.mode == CoordConnectionMode::Connected,
            got.base.is_some(),
            "mode and base disagree: {got:?}"
        );
    }

    /// `source` is always one of the five documented arms — in BOTH modes.
    /// It is what makes the isolated reason actionable, so an empty or novel
    /// string is a defect, not a cosmetic issue.
    #[test]
    fn source_is_always_one_of_the_documented_arms() {
        let got = get_coord_mode();
        assert!(
            [
                "env",
                "profile",
                "tier_default",
                "dev_localhost_fallback",
                "unknown_tier_prod_default",
            ]
            .contains(&got.source.as_str()),
            "unknown coord base source: {got:?}"
        );
    }

    /// The exact JSON the webview receives. `invoke<CoordMode>('get_coord_mode')`
    /// destructures these three keys by name and switches on the `mode`
    /// string; a serde rename here breaks the UI with no Rust failure.
    #[test]
    fn wire_shape_is_mode_base_source() {
        let json = serde_json::to_value(CoordModeReport {
            mode: CoordConnectionMode::Isolated,
            base: None,
            source: "dev_localhost_fallback".to_string(),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "mode": "isolated",
                "base": null,
                "source": "dev_localhost_fallback",
            })
        );

        let connected = serde_json::to_value(CoordModeReport {
            mode: CoordConnectionMode::Connected,
            base: Some("https://coord.qontinui.io".to_string()),
            source: "tier_default".to_string(),
        })
        .unwrap();
        assert_eq!(
            connected,
            serde_json::json!({
                "mode": "connected",
                "base": "https://coord.qontinui.io",
                "source": "tier_default",
            })
        );
    }
}
