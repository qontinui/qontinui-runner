//! Trait definition for step-level hardener rules.
//!
//! A `HardenRule` transforms a single step (`serde_json::Value`) in-place,
//! returning a list of [`RuleEvent`]s describing the mutations it made.
//! The step is wrapped in a [`StepMut`] that protects the `working_directory`
//! field: rules may only mutate that field if they opt-in by returning
//! `true` from [`HardenRule::requires_working_dir`].
//!
//! This is the protection layer for the recurring bug where miscellaneous
//! sanitizer code would accidentally normalize `working_directory` to `"."`,
//! breaking cargo commands that run in `src-tauri/`. With this design, any
//! rule that does not declare `requires_working_dir` has its
//! `working_directory` mutations silently reverted by the engine — and the
//! reversion is logged as a `RuleEvent::WorkingDirBlocked` so we can see
//! which rule tried to touch the field.
//!
//! Design trade-off: instead of a fully typed setter API that would require
//! rewriting every rule to use `step.set_field(...)`, we use a
//! snapshot-and-revert strategy. Rules still receive a
//! `serde_json::Value` through `StepMut::raw_mut()` and can mutate it
//! freely — the engine snapshots `working_directory` before and restores
//! it after if the rule didn't have the capability. Net behavior: exactly
//! the same as the prior ad-hoc code paths for rules that never touched
//! `working_directory` (all of them, historically), plus a safety net for
//! future regressions.
//!
//! See `engine.rs` for the traversal and capability-gating logic.

use serde_json::Value;

/// Capability token granting a rule permission to mutate the
/// `working_directory` field. Only the engine can mint one; rules receive
/// it only when they declare `requires_working_dir() -> true`.
///
/// The inner `()` is deliberately private to this module so rules can't
/// construct their own token.
#[derive(Debug)]
pub struct WorkingDirCapability(pub(super) ());

/// An event emitted by a rule describing what it did to a step.
///
/// Events are collected by the engine into a [`HardenReport`] for
/// diagnostic logging and (future) telemetry. They intentionally carry
/// only strings so they can be serialized and compared in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleEvent {
    /// The rule mutated a field (or the whole step). `detail` is a short
    /// human-readable description.
    Mutated { rule: &'static str, detail: String },
    /// The rule's invocation was a no-op for this step.
    #[allow(dead_code)]
    NoOp { rule: &'static str },
    /// A warning produced by the rule (does not imply a mutation).
    #[allow(dead_code)]
    Warning { rule: &'static str, message: String },
    /// The engine reverted a `working_directory` mutation that a rule
    /// made without the capability. The original value is preserved.
    WorkingDirBlocked {
        rule: &'static str,
        attempted: Option<String>,
        original: Option<String>,
    },
}

/// Read-only context passed to every rule.
///
/// Currently only carries the working-dir capability token (when granted).
/// Extend this struct with additional read-only context (e.g. SDK status,
/// runner port) as we port more rules that need it — do NOT smuggle
/// mutable state through `RuleCtx`.
pub struct RuleCtx<'a> {
    /// Present iff the engine decided this rule is allowed to mutate
    /// `working_directory` (i.e. `rule.requires_working_dir()` returned
    /// true). Rules should pass this token to
    /// [`StepMut::set_working_directory`].
    pub working_dir_cap: Option<&'a WorkingDirCapability>,
}

impl<'a> RuleCtx<'a> {
    /// Construct a context with no working-dir capability granted.
    pub fn without_capability() -> Self {
        Self {
            working_dir_cap: None,
        }
    }
}

/// Mutable view into a single step.
///
/// Currently this is a thin wrapper around `&mut Value` — rules call
/// [`StepMut::raw_mut`] to get the underlying value and mutate freely.
/// The engine snapshots `working_directory` before/after each rule
/// invocation and reverts unauthorized changes.
///
/// Future refactor: expose a typed-setter surface (`set_command`,
/// `set_retry_count`, etc.) so individual field mutations can be
/// validated/telemetry'd without escape hatches. For now the raw-mut
/// escape hatch keeps this refactor behavior-identical.
pub struct StepMut<'a> {
    inner: &'a mut Value,
    /// Set by a rule that fully replaced the command string and wants
    /// downstream command-string rules to skip this step (mirrors the
    /// `continue` statement in the original `sanitize_commands_in_steps`
    /// loop after the jq→python replacement).
    skip_remaining_command_rules: bool,
}

impl<'a> StepMut<'a> {
    pub fn new(step: &'a mut Value) -> Self {
        Self {
            inner: step,
            skip_remaining_command_rules: false,
        }
    }

    /// Raw mutable access to the underlying step value. Rules may mutate
    /// any field here; the engine enforces the working_directory
    /// capability by snapshotting before and restoring after.
    pub fn raw_mut(&mut self) -> &mut Value {
        self.inner
    }

    /// Read-only view of the step.
    pub fn raw(&self) -> &Value {
        self.inner
    }

    /// Signal that all subsequent command-string rules should skip this
    /// step. Used by the jq→python rule to preserve the `continue`
    /// semantics of the original sanitizer loop.
    pub fn mark_command_replaced(&mut self) {
        self.skip_remaining_command_rules = true;
    }

    /// Check whether a prior rule on this step has marked the command
    /// as fully replaced. Command-string rules should bail out early
    /// when this returns true.
    pub fn command_replaced(&self) -> bool {
        self.skip_remaining_command_rules
    }

    /// Capability-gated setter for `working_directory`. Only rules that
    /// declared `requires_working_dir() -> true` receive the capability
    /// token from the engine, so only they can call this.
    ///
    /// This is not currently required for any rule — the working-directory
    /// rule reserves the right to use it. Left here so we can migrate
    /// call sites one by one.
    #[allow(dead_code)]
    pub fn set_working_directory(&mut self, _cap: &WorkingDirCapability, new_value: String) {
        if let Some(obj) = self.inner.as_object_mut() {
            obj.insert("working_directory".to_string(), Value::String(new_value));
        }
    }

    /// Read the current `working_directory` value (as an owned `String`).
    pub(super) fn working_directory(&self) -> Option<String> {
        self.inner
            .get("working_directory")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }
}

/// Report of all events produced by a single engine run.
#[derive(Debug, Default, Clone)]
pub struct HardenReport {
    /// All rule events, in the order they were produced.
    pub events: Vec<RuleEvent>,
    /// Total number of mutations across all rules and steps.
    pub mutation_count: usize,
    /// Total number of `WorkingDirBlocked` reversions across all rules.
    pub working_dir_blocked_count: usize,
}

impl HardenReport {
    pub fn push(&mut self, event: RuleEvent) {
        match &event {
            RuleEvent::Mutated { .. } => self.mutation_count += 1,
            RuleEvent::WorkingDirBlocked { .. } => self.working_dir_blocked_count += 1,
            _ => {}
        }
        self.events.push(event);
    }
}

/// Trait implemented by every step-level hardener rule.
///
/// Rules are applied exactly once per step by the engine, in the order
/// they were registered with [`super::engine::RuleEngine::new`].
pub trait HardenRule: Send + Sync {
    /// Stable short name used in logs and events. Should match the file
    /// name the rule lives in (snake_case).
    fn name(&self) -> &'static str;

    /// Optional longer description. Default empty.
    #[allow(dead_code)]
    fn description(&self) -> &'static str {
        ""
    }

    /// Whether this rule needs capability to mutate `working_directory`.
    /// Default `false` — the vast majority of rules must NOT touch this
    /// field, and the engine enforces that by snapshotting and restoring.
    fn requires_working_dir(&self) -> bool {
        false
    }

    /// Apply the rule to a single step. Push [`RuleEvent`]s onto the
    /// report to describe any mutations or warnings.
    ///
    /// Rules should be idempotent: running them twice on the same step
    /// must produce the same result on the second run as the first.
    fn apply(&self, step: &mut StepMut<'_>, ctx: &RuleCtx<'_>, report: &mut HardenReport);
}
