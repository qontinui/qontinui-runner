//! The interception **gate decision** — a pure, total `should_block` predicate
//! (plan §3 A3 step 2 / A4 / A6 + §4 Phase 3).
//!
//! The runtime gate decision lives in the materialized shim *script* (it is the
//! process that has the parsed argv + the pre-call gate verdict in hand). But
//! the SEMANTICS of that decision must be testable + documented in Rust, so the
//! script and this module stay lock-stepped on ONE truth table. The shim's
//! gate branch is a line-for-line shell transliteration of [`should_block`];
//! the unit tests here are the canonical spec it must match.
//!
//! ## The five inputs (everything the shim has locally at gate time)
//! - `mode`        — the injected `QONTINUI_INSTALL_INTERCEPT_MODE` (observe |
//!   gate; anything else is normalized to observe — fail-open, see
//!   [`InterceptMode::parse`]).
//! - `gate`        — the producer's pre-call verdict (`"gate":"escalate"` ⇒
//!   `escalate=true`). The producer controls the response shape, so the shim's
//!   substring check (`"gate":"escalate"`) is robust.
//! - `override_on` — `QONTINUI_INSTALL_OVERRIDE=1` was set by the agent (the
//!   audited re-run path; the shim flips `override_escalation:true` on the
//!   pre-call so the producer records `+overridden` provenance).
//! - `lockfile_sync` — the shim's OWN classification said this is a lockfile
//!   reconciliation / no-new-package install (A6) — NEVER gated even if the
//!   server escalated. This rides from the shim's local classify, not the
//!   server verdict.
//! - `never_gate`  — the tool is execution-shaped (npx) and is never blocked
//!   even in gate mode ([`super::classify::ShimTool::never_gate`]). Also a
//!   LOCAL property the shim knows from its own `$TOOL`.
//!
//! ## The truth table (the ONLY combination that blocks)
//! A block happens **iff** ALL of:
//!   mode == gate  AND  escalate  AND  !override_on  AND  !lockfile_sync  AND
//!   !never_gate.
//! Every other combination proceeds (runs the real tool). This is the
//! fail-open-by-default posture: observe never blocks; a missing/garbled mode
//! degrades to observe; lockfile-sync and npx are carved out unconditionally;
//! and the agent can always override.

/// The injected interception mode. `observe` (Phase 1/2) never blocks; `gate`
/// (Phase 3) honors an `escalate` verdict. Any other value normalizes to
/// `Observe` (fail-open — a typo in the runner env must never start blocking).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterceptMode {
    /// Declare + observe + verify, but ALWAYS proceed (never block).
    Observe,
    /// Honor the `escalate` verdict: block a predicted-risky non-lockfile-sync,
    /// non-never-gate install unless overridden.
    Gate,
}

impl InterceptMode {
    /// Parse the injected mode token. `gate` ⇒ [`InterceptMode::Gate`];
    /// EVERYTHING else (including `observe`, an empty string, a typo, or absent)
    /// ⇒ [`InterceptMode::Observe`]. Case-insensitive + trim-tolerant. This is
    /// the validation §3 step 3 mandates: anything other than `gate` fails open
    /// to observe.
    pub fn parse(token: &str) -> InterceptMode {
        match token.trim().to_ascii_lowercase().as_str() {
            "gate" => InterceptMode::Gate,
            _ => InterceptMode::Observe,
        }
    }

    /// The wire token the seam injects (`observe` / `gate`).
    pub fn as_str(self) -> &'static str {
        match self {
            InterceptMode::Observe => "observe",
            InterceptMode::Gate => "gate",
        }
    }
}

/// Should the shim BLOCK this install (print the A4 UX + `exit 1` WITHOUT
/// running the real tool)? Pure + total — the canonical spec the materialized
/// shim's gate branch transliterates (plan §3 A3/A4/A6).
///
/// Blocks **iff** every condition holds:
/// - `mode == Gate` — observe never blocks (Phase 1/2 posture).
/// - `escalate` — the producer's pre-call verdict was `escalate`.
/// - `!override_on` — the agent did NOT set `QONTINUI_INSTALL_OVERRIDE=1`.
/// - `!lockfile_sync` — not a lockfile reconciliation / no-new-package install
///   (A6 — these are observed but never gated).
/// - `!never_gate` — not an execution-shaped tool (npx) that is never blocked.
///
/// Any other combination ⇒ `false` (proceed: run the real tool). Fail-open is
/// the default — a garbled mode is `Observe` (never blocks) via
/// [`InterceptMode::parse`].
pub fn should_block(
    mode: InterceptMode,
    escalate: bool,
    override_on: bool,
    lockfile_sync: bool,
    never_gate: bool,
) -> bool {
    mode == InterceptMode::Gate && escalate && !override_on && !lockfile_sync && !never_gate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parse_only_gate_is_gate_else_observe() {
        assert_eq!(InterceptMode::parse("gate"), InterceptMode::Gate);
        assert_eq!(InterceptMode::parse("GATE"), InterceptMode::Gate);
        assert_eq!(InterceptMode::parse(" gate "), InterceptMode::Gate);
        // Everything else fails open to observe.
        for t in [
            "observe", "OBSERVE", "", "  ", "block", "on", "1", "true", "garbage",
        ] {
            assert_eq!(
                InterceptMode::parse(t),
                InterceptMode::Observe,
                "{t:?} must normalize to observe (fail-open)"
            );
        }
    }

    #[test]
    fn mode_as_str_roundtrips() {
        assert_eq!(InterceptMode::Observe.as_str(), "observe");
        assert_eq!(InterceptMode::Gate.as_str(), "gate");
        assert_eq!(
            InterceptMode::parse(InterceptMode::Gate.as_str()),
            InterceptMode::Gate
        );
        assert_eq!(
            InterceptMode::parse(InterceptMode::Observe.as_str()),
            InterceptMode::Observe
        );
    }

    // ---- the should_block truth table ------------------------------------
    // The ONLY blocking combination is gate + escalate + !override +
    // !lockfile_sync + !never_gate.

    #[test]
    fn observe_mode_never_blocks() {
        // Even an escalate, no override, normal install never blocks in observe.
        assert!(!should_block(
            InterceptMode::Observe,
            true,  // escalate
            false, // override_on
            false, // lockfile_sync
            false, // never_gate
        ));
        // And in observe, no input combination blocks.
        for &esc in &[true, false] {
            for &ov in &[true, false] {
                for &lf in &[true, false] {
                    for &ng in &[true, false] {
                        assert!(
                            !should_block(InterceptMode::Observe, esc, ov, lf, ng),
                            "observe must never block (esc={esc} ov={ov} lf={lf} ng={ng})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn gate_escalate_normal_install_no_override_blocks() {
        assert!(should_block(
            InterceptMode::Gate,
            true,  // escalate
            false, // override_on
            false, // lockfile_sync
            false, // never_gate
        ));
    }

    #[test]
    fn gate_proceed_never_blocks() {
        // A non-escalate verdict in gate mode proceeds.
        assert!(!should_block(
            InterceptMode::Gate,
            false, // escalate (proceed)
            false,
            false,
            false,
        ));
    }

    #[test]
    fn override_unblocks_even_in_gate_escalate() {
        assert!(!should_block(
            InterceptMode::Gate,
            true, // escalate
            true, // override_on ⇒ proceed (records +overridden)
            false,
            false,
        ));
    }

    #[test]
    fn lockfile_sync_never_blocks_even_in_gate_escalate() {
        // A6: a lockfile reconciliation / no-new-package install is never gated.
        assert!(!should_block(
            InterceptMode::Gate,
            true, // escalate
            false,
            true, // lockfile_sync ⇒ never gate
            false,
        ));
    }

    #[test]
    fn never_gate_tool_npx_never_blocks_even_in_gate_escalate() {
        // npx (execution-shaped) is never blocked even on an escalate.
        assert!(!should_block(
            InterceptMode::Gate,
            true, // escalate
            false,
            false,
            true, // never_gate (npx)
        ));
    }

    #[test]
    fn exhaustive_only_one_combination_blocks() {
        // Brute-force all 2^4 input combinations in gate mode; assert exactly
        // ONE (escalate && !override && !lockfile_sync && !never_gate) blocks.
        let mut blockers = Vec::new();
        for &esc in &[true, false] {
            for &ov in &[true, false] {
                for &lf in &[true, false] {
                    for &ng in &[true, false] {
                        if should_block(InterceptMode::Gate, esc, ov, lf, ng) {
                            blockers.push((esc, ov, lf, ng));
                        }
                    }
                }
            }
        }
        assert_eq!(
            blockers,
            vec![(true, false, false, false)],
            "exactly one gate-mode combination blocks"
        );
    }
}
