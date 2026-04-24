//! The ONE rule that holds the `working_directory` capability.
//!
//! Historically the hardener had no legitimate reason to rewrite
//! `working_directory` on any step, yet several ad-hoc sanitizer paths
//! would accidentally clobber the field to `"."` — breaking cargo
//! commands that must run in the `src-tauri/` subdirectory. The
//! [`super::super::engine::RuleEngine`] now snapshots the field before
//! every rule invocation and reverts unauthorized changes, so only a
//! rule that explicitly opts in (via
//! [`HardenRule::requires_working_dir`]) can legitimately write the
//! field.
//!
//! This rule opts in and currently leaves the field alone. Its purpose
//! is to:
//! 1. Anchor the capability pattern so future working-directory fix-ups
//!    have a home that is verified to hold the token.
//! 2. Act as the canonical `requires_working_dir = true` example for
//!    test and review.
//!
//! If the field is ever missing on a step that needs one, extend this
//! rule — don't sprinkle the fix across other sanitizer rules.

use crate::workflow_generation::hardener::rule::{HardenReport, HardenRule, RuleCtx, StepMut};

pub struct WorkingDirectoryPreserved;

impl HardenRule for WorkingDirectoryPreserved {
    fn name(&self) -> &'static str {
        "working_directory_preserved"
    }

    fn description(&self) -> &'static str {
        "Canonical holder of the working_directory capability; currently a no-op sentinel"
    }

    fn requires_working_dir(&self) -> bool {
        true
    }

    fn apply(&self, _step: &mut StepMut<'_>, _ctx: &RuleCtx<'_>, _report: &mut HardenReport) {
        // Intentional no-op: no working_directory fix-ups are needed today.
        // When one becomes necessary, implement it here. The engine will
        // grant the capability token via `ctx.working_dir_cap` (Some), and
        // we can call `step.set_working_directory(token, "...")` to write.
    }
}
