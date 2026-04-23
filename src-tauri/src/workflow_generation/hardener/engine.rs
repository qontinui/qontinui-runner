//! Rule engine: walks a list of steps exactly once and applies every
//! registered rule in order to each step.
//!
//! The engine enforces the `working_directory` protection invariant:
//! rules that did not declare `requires_working_dir()` never see their
//! `working_directory` mutations persist. The engine snapshots the field
//! before each rule invocation and restores it afterwards when the rule
//! is not allowed to change it.

use serde_json::Value;
use tracing::warn;

use super::rule::{
    HardenReport, HardenRule, RuleCtx, RuleEvent, StepMut, WorkingDirCapability,
};

/// Ordered collection of rules that operates as a single traversal.
pub struct RuleEngine {
    rules: Vec<Box<dyn HardenRule>>,
}

impl RuleEngine {
    pub fn new(rules: Vec<Box<dyn HardenRule>>) -> Self {
        Self { rules }
    }

    /// Apply the rule pipeline to every step in `steps`, in order.
    /// Returns the accumulated report.
    pub fn apply(&self, steps: &mut [Value]) -> HardenReport {
        let mut report = HardenReport::default();

        for step in steps.iter_mut() {
            self.apply_to_step(step, &mut report);
        }

        report
    }

    fn apply_to_step(&self, step: &mut Value, report: &mut HardenReport) {
        for rule in &self.rules {
            // Snapshot working_directory before the rule runs so we can
            // revert unauthorized mutations.
            let before = step
                .get("working_directory")
                .and_then(|v| v.as_str())
                .map(str::to_string);

            // Grant the capability iff the rule declared it.
            let cap_storage;
            let ctx = if rule.requires_working_dir() {
                cap_storage = WorkingDirCapability(());
                RuleCtx {
                    working_dir_cap: Some(&cap_storage),
                }
            } else {
                RuleCtx::without_capability()
            };

            let mut view = StepMut::new(step);
            rule.apply(&mut view, &ctx, report);

            if !rule.requires_working_dir() {
                let after = view.working_directory();
                if before != after {
                    let attempted = after.clone();
                    // Revert: put the original value back (or delete it
                    // if it wasn't there before).
                    if let Some(obj) = step.as_object_mut() {
                        match &before {
                            Some(original) => {
                                obj.insert(
                                    "working_directory".to_string(),
                                    Value::String(original.clone()),
                                );
                            }
                            None => {
                                obj.remove("working_directory");
                            }
                        }
                    }
                    warn!(
                        rule = rule.name(),
                        attempted = ?attempted,
                        original = ?before,
                        "Hardener rule attempted to mutate working_directory without capability; reverted",
                    );
                    report.push(RuleEvent::WorkingDirBlocked {
                        rule: rule.name(),
                        attempted,
                        original: before,
                    });
                }
            }
        }
    }
}
