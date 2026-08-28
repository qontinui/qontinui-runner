//! The local apply driver — P2b of plan
//! `2026-07-02-devenv-copy-canonical-config-phase2-agent-apply`.
//!
//! P2a shipped `env pull`: fetch the canonical config, diff this box against it,
//! print the plan, change nothing. This module is the other half — it takes that
//! same [`ApplyPlan`] and reconciles **this box, by a decision taken on this
//! box**. Nothing is ever pushed onto a machine from the server; the runner that
//! owns the machine decides.
//!
//! ## Guardrails, all enforced here rather than per-section
//!
//! - **Dry-run by default.** [`ApplyOptions::confirm`] is opt-in; without it the
//!   report is computed and printed and no file is touched, no process is
//!   spawned. This is the "local confirm" the plan's P2b bullet requires.
//! - **Server-authoritative policy.** Only sections the server marked
//!   `applyable` are eligible, and only via [`super::pull::SectionPlan::actionable`],
//!   which also excludes `Extra` local keys and repo-derived keys. An
//!   unrecognized policy string already degrades to `report_only` upstream, so a
//!   newer backend can never be read as permission to mutate.
//! - **Fixed key sets, per section.** Eligibility is not "everything the server
//!   sent" but an explicit allowlist inside each section's module, so a backend
//!   that grows a section cannot widen what this runner writes.
//! - **Report-and-change-nothing beats a partial write.** Every blocker (a
//!   `legacy-env` box, an unparseable local value, no detected version manager)
//!   is surfaced as a reason, never worked around.
//! - **The report holds only redacted values.** Each section module converts its
//!   own raw plan into [`AppliedChange`]s whose `from`/`to` have already been
//!   through the capture's sanitizer, so neither the text nor the `--json`
//!   surface has a channel for a DSN password.
//! - **Audited locally.** A real apply appends one JSONL record per change to
//!   `~/.qontinui/env-apply-log.jsonl`.

use std::path::PathBuf;

use serde_json::{json, Value};

use super::pull::{ApplyPlan, SectionPlan, SectionPolicy};
use super::{apply_repos, apply_services, apply_versions};

/// What the caller asked for.
#[derive(Debug, Clone, Default)]
pub struct ApplyOptions {
    /// Actually mutate this box. Without it this is a preview — the safe
    /// default.
    pub confirm: bool,
    /// Restrict the apply to these section names. Empty = every supported
    /// section.
    pub sections: Vec<String>,
    /// Explicit target profile for the `services` apply. `None` resolves the
    /// active profile the same way `profiles::load` does.
    pub profile: Option<String>,
}

impl ApplyOptions {
    fn wants(&self, section: &str) -> bool {
        self.sections.is_empty() || self.sections.iter().any(|s| s == section)
    }
}

/// One change, in the **report's** vocabulary: already redacted, safe to print,
/// log or serialize. Section modules build these from their own raw plans, which
/// is what keeps a secret out of every downstream surface structurally rather
/// than by remembering to sanitize at each one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedChange {
    /// The section key this change is for (`redis_url`, `node`, …).
    pub key: String,
    /// The current value, redacted. `None` when the box had nothing here.
    pub from: Option<String>,
    /// The value written (or that would be written), redacted.
    pub to: String,
    /// One-line note about HOW — "local credentials and path preserved",
    /// "via rustup", … Rendered under the change.
    pub detail: Option<String>,
}

/// A drift this runner deliberately did not act on, and why. Surfaced rather
/// than dropped: a silently-ignored change reads as "nothing to do".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkipRecord {
    pub key: String,
    pub reason: String,
}

/// Outcome for one section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionStatus {
    /// The server did not mark this section applyable — it is reported by
    /// `env pull` and never written.
    NotApplyable(SectionPolicy),
    /// Applyable, but this runner has no apply implementation for it.
    Unsupported,
    /// The caller narrowed the apply with `--section` and excluded this one.
    NotSelected,
    /// Nothing actionable drifts here.
    NothingToDo,
    /// Changes computed but NOT performed (no `--confirm`).
    Planned,
    /// Changes performed.
    Applied,
    /// Something on this box prevents the apply. Nothing was changed.
    ///
    /// Carries a [`BlockedCause`] as well as prose: three different situations
    /// used to flatten into one wire word, leaving a JSON consumer to tell them
    /// apart by PARSING THE SENTENCE. They are not the same event and they are
    /// not the same operator action.
    Blocked { cause: BlockedCause, reason: String },
}

/// Why a section is [`SectionStatus::Blocked`]. The machine-readable half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedCause {
    /// A key in this section was never MEASURED, so nothing here can be called
    /// settled — the apply is refusing on a measurement gap, not on the box.
    /// Report-only: there is nothing to retry until the probe answers.
    Unmeasured,
    /// Every detected manager ran and NOTHING moved the observed version. The
    /// apply did work; the box did not change. The reasons are per-key, in
    /// `skipped`.
    NoMovement,
    /// Something on this box prevents the apply from running at all (a missing
    /// precondition, an unreadable or unwritable target).
    Precondition,
}

impl BlockedCause {
    fn wire(self) -> &'static str {
        match self {
            Self::Unmeasured => "blocked_unmeasured",
            Self::NoMovement => "blocked_no_movement",
            Self::Precondition => "blocked_precondition",
        }
    }
}

impl SectionStatus {
    /// A [`SectionStatus::Blocked`] on a precondition — the default cause for
    /// every "this box will not let the apply run" case.
    pub fn blocked_precondition(reason: impl Into<String>) -> Self {
        Self::Blocked {
            cause: BlockedCause::Precondition,
            reason: reason.into(),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::NotApplyable(p) => format!("not applyable ({})", p.label()),
            Self::Unsupported => "no apply support in this runner".to_string(),
            Self::NotSelected => "not selected".to_string(),
            Self::NothingToDo => "nothing to do".to_string(),
            Self::Planned => "would change".to_string(),
            Self::Applied => "APPLIED".to_string(),
            Self::Blocked { reason, .. } => format!("blocked: {reason}"),
        }
    }

    /// The wire word. The three `Blocked` causes get three DIFFERENT words —
    /// all prefixed `blocked_`, so a consumer that only cares whether the
    /// section is blocked can still match on the prefix, while one that needs
    /// to know "un-measurable" from "ran and nothing moved" no longer has to
    /// regex an English sentence to find out.
    fn wire(&self) -> &'static str {
        match self {
            Self::NotApplyable(_) => "not_applyable",
            Self::Unsupported => "unsupported",
            Self::NotSelected => "not_selected",
            Self::NothingToDo => "nothing_to_do",
            Self::Planned => "planned",
            Self::Applied => "applied",
            Self::Blocked { cause, .. } => cause.wire(),
        }
    }
}

/// One section's contribution to the report.
#[derive(Debug, Clone)]
pub struct SectionApply {
    pub section: String,
    pub status: SectionStatus,
    /// What the writes land in — the profile name for `services`, the declared
    /// scope root for `versions`.
    pub target: Option<String>,
    pub changes: Vec<AppliedChange>,
    pub skipped: Vec<SkipRecord>,
    pub notes: Vec<String>,
    /// Keys in this section THIS BOX could not MEASURE, straight off
    /// [`SectionPlan::unknown_keys`](super::pull::SectionPlan::unknown_keys).
    ///
    /// **Written in exactly one place — [`dispatch`] — and never by a section
    /// module.** `apply_versions` consumes the same set for its own purposes (a
    /// refusal and a per-key note); `apply_repos` and `apply_services` do not
    /// touch it at all, which is exactly why the report-level summary cannot be
    /// built out of what the modules happen to have surfaced. A single writer is
    /// what stops a module that never learned about the set from silently
    /// restoring a completeness claim the report has not earned.
    ///
    /// Only ever populated for a section that was both SELECTED and `Applyable`
    /// — including one this runner has no module for, since a key it could not
    /// read is a key it could not read either way. The two guards in [`dispatch`]
    /// are what keep it out of `NotSelected` and `NotApplyable`, where a zero
    /// change count is a statement about the operator's own `--sections` choice
    /// or the server's policy rather than about this box.
    pub unmeasured_keys: Vec<String>,
}

impl SectionApply {
    /// A section with a status and nothing else — every "not applied" case.
    pub fn inert(section: &str, status: SectionStatus) -> Self {
        Self {
            section: section.to_string(),
            status,
            target: None,
            changes: Vec::new(),
            skipped: Vec::new(),
            notes: Vec::new(),
            unmeasured_keys: Vec::new(),
        }
    }
}

/// The whole apply outcome.
#[derive(Debug, Clone)]
pub struct ApplyReport {
    pub environment_id: String,
    pub canonical_machine_name: Option<String>,
    /// True when `--confirm` was given, i.e. mutation was permitted.
    pub confirmed: bool,
    /// True when this box IS canonical — there is nothing to reconcile toward.
    pub is_canonical_self: bool,
    pub sections: Vec<SectionApply>,
}

impl ApplyReport {
    /// Total changes made (`confirmed`) or that would be made (dry run).
    pub fn change_count(&self) -> usize {
        self.sections.iter().map(|s| s.changes.len()).sum()
    }

    /// Total keys across the report that this box could not MEASURE.
    ///
    /// Load-bearing beside [`change_count`](Self::change_count): a zero change
    /// count is a POSITIVE claim about the box ("nothing needs doing here"), and
    /// that claim is unsound over a key nobody read. `env pull` already refuses
    /// to make the equivalent claim — [`ApplyPlan::is_in_sync`](super::pull::ApplyPlan::is_in_sync)
    /// is false whenever a key went unmeasured — and the apply side is the
    /// surface that actually MUTATES, so it is the last place that should be
    /// allowed to say "nothing to do" over an unread key.
    pub fn unmeasured_key_count(&self) -> usize {
        self.sections.iter().map(|s| s.unmeasured_keys.len()).sum()
    }

    /// True when at least one section was actually applied.
    pub fn changed_anything(&self) -> bool {
        self.sections
            .iter()
            .any(|s| s.status == SectionStatus::Applied)
    }
}

/// Compute — and, with `opts.confirm`, perform — the local apply.
///
/// Every section of the pull plan appears in the report, so "this section was
/// not applied" always carries a reason rather than being an absence.
pub fn run_apply(plan: &ApplyPlan, opts: &ApplyOptions) -> ApplyReport {
    let mut sections: Vec<SectionApply> = Vec::new();

    if !plan.is_canonical_self {
        for section in &plan.sections {
            sections.push(dispatch(section, opts));
        }
    }

    ApplyReport {
        environment_id: plan.environment_id.clone(),
        canonical_machine_name: plan.canonical_machine_name.clone(),
        confirmed: opts.confirm,
        is_canonical_self: plan.is_canonical_self,
        sections,
    }
}

/// Gate one section through the shared guardrails, then hand it to its section
/// module. A section with no module is [`SectionStatus::Unsupported`] even when
/// the server marks it applyable — the server grants permission, it does not
/// supply an implementation.
fn dispatch(section: &SectionPlan, opts: &ApplyOptions) -> SectionApply {
    let name = section.section.as_str();
    if !opts.wants(name) {
        return SectionApply::inert(name, SectionStatus::NotSelected);
    }
    if section.policy != SectionPolicy::Applyable {
        return SectionApply::inert(name, SectionStatus::NotApplyable(section.policy));
    }
    let mut out = match name {
        apply_services::SERVICES_SECTION => {
            let out = apply_services::apply_section(section, opts.profile.as_deref(), opts.confirm);
            audit_if_applied(&out);
            out
        }
        apply_versions::VERSIONS_SECTION => {
            let out = apply_versions::apply_section(section, opts.confirm);
            audit_if_applied(&out);
            out
        }
        apply_repos::REPOS_SECTION => {
            let out = apply_repos::apply_section(section, opts.confirm);
            audit_if_applied(&out);
            out
        }
        _ => SectionApply::inert(name, SectionStatus::Unsupported),
    };
    // The ONE writer of `unmeasured_keys` (see the field's doc). Stamped here
    // rather than in each section module for two reasons: the two guards above
    // have already established the only case in which the set is meaningful to
    // the report summary (selected AND applyable), and a module that never
    // learned about the field cannot make the summary claim completeness it has
    // not earned. `Unsupported` is stamped too — the operator asked for a
    // section this runner cannot apply, and the keys it could not read are still
    // keys it could not read.
    out.unmeasured_keys = section.unknown_keys.iter().cloned().collect();
    out
}

// ============================================================================
// Local audit
// ============================================================================

/// Path of the local apply audit log (`~/.qontinui/env-apply-log.jsonl`).
pub fn audit_log_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join("env-apply-log.jsonl"))
}

/// Append one JSONL record per performed change. Values are already redacted by
/// construction ([`AppliedChange`]), so the log can never hold a credential.
///
/// Best-effort: failing to write the audit does not fail the apply — the change
/// already happened, and pretending otherwise would be a worse lie than a
/// missing log line — but it does WARN.
fn audit_if_applied(section: &SectionApply) {
    if section.status != SectionStatus::Applied || section.changes.is_empty() {
        return;
    }
    let Some(path) = audit_log_path() else {
        return;
    };
    let at = chrono::Utc::now().to_rfc3339();
    let mut buf = String::new();
    for change in &section.changes {
        let record = json!({
            "at": at,
            "section": section.section,
            "target": section.target,
            "key": change.key,
            "from": change.from,
            "to": change.to,
        });
        buf.push_str(&record.to_string());
        buf.push('\n');
    }
    let write = (|| -> std::io::Result<()> {
        use std::io::Write;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?
            .write_all(buf.as_bytes())
    })();
    if let Err(e) = write {
        tracing::warn!("env apply: could not append {}: {e}", path.display());
    }
}

// ============================================================================
// Rendering
// ============================================================================

/// Render the report as human-readable text. **Pure — unit-tested.**
pub fn render_report(report: &ApplyReport) -> String {
    let mut out = String::new();

    if report.is_canonical_self {
        out.push_str("This machine IS the canonical environment. Nothing to apply.\n");
        return out;
    }

    let who = report
        .canonical_machine_name
        .clone()
        .unwrap_or_else(|| "canonical".to_string());
    out.push_str(&format!("Reconciling toward: {who}\n"));
    out.push_str(&format!(
        "Environment:        {}\n\n",
        report.environment_id
    ));

    for section in &report.sections {
        // Even a section that could never contribute gets a line, so "why wasn't
        // this applied" is always answered rather than being an absence.
        out.push_str(&format!(
            "  {} [{}]",
            section.section,
            section.status.label()
        ));
        if let Some(target) = &section.target {
            out.push_str(&format!(" -> {target}"));
        }
        out.push('\n');

        let verb = if section.status == SectionStatus::Applied {
            "set"
        } else {
            "would set"
        };
        for change in &section.changes {
            match &change.from {
                Some(from) => out.push_str(&format!(
                    "    {verb} {}: {from} -> {}\n",
                    change.key, change.to
                )),
                None => out.push_str(&format!(
                    "    {verb} {}: (unset) -> {}\n",
                    change.key, change.to
                )),
            }
            if let Some(detail) = &change.detail {
                out.push_str(&format!("      ({detail})\n"));
            }
        }
        for skip in &section.skipped {
            out.push_str(&format!(
                "    - {}: not applied — {}\n",
                skip.key, skip.reason
            ));
        }
        for note in &section.notes {
            out.push_str(&format!("    note: {note}\n"));
        }
    }

    let n = report.change_count();
    let unmeasured = report.unmeasured_key_count();
    out.push('\n');
    // Printed BEFORE the count sentence, and qualifying every arm of it: a box
    // that could not read a key is partly blind whether the apply changed
    // nothing, planned three changes, or made them.
    //
    // It NAMES the keys rather than only counting them, and that is the whole
    // reason it is not redundant with what the sections already print. Only
    // `apply_versions` emits a per-key note today; a section whose module never
    // learned about the set (or that has no module at all) would otherwise leave
    // the operator a bare count with no way to learn WHICH key, with the names
    // reachable only from `--json`.
    //
    // No cause is stated. `unknown_keys` is a generic `section -> keys` map, and
    // "the probe exceeded its capture budget" is true of `versions` and asserted
    // of nothing else — the section's own note is where a section-specific
    // explanation and its remediation belong.
    if unmeasured > 0 {
        let named: Vec<String> = report
            .sections
            .iter()
            .flat_map(|s| {
                s.unmeasured_keys
                    .iter()
                    .map(move |k| format!("{}.{k}", s.section))
            })
            .collect();
        out.push_str(&format!(
            "{unmeasured} key(s) could NOT be measured on this box and are reported rather than \
             acted on — nothing below rests on a value for them: {}\n",
            named.join(", "),
        ));
    }
    if n == 0 && unmeasured > 0 {
        // "Nothing to apply on this machine." is a POSITIVE claim about the box,
        // and it is unsound over a key nobody read — the same reason
        // `apply_versions` stopped letting such a section report `nothing to do`,
        // and the same reason `ApplyPlan::is_in_sync` is false in the pull. Without
        // this arm the section line reads `versions [blocked: … could not be
        // measured …]` while the summary three lines below says the opposite, and
        // the summary is the line an operator actually acts on.
        //
        // Worded as "not a clean bill", NOT as "the zero is caused by the unread
        // keys". The zero can have another cause entirely — a section this runner
        // has no module for is `Unsupported` whether or not anything went
        // unmeasured — and naming the wrong cause would be its own small lie.
        out.push_str(
            "No changes to apply on this machine — but that is NOT a clean bill of health: \
             the key(s) above were never read, so nothing here rests on a measurement of \
             them.\n",
        );
    } else if n == 0 {
        out.push_str("Nothing to apply on this machine.\n");
    } else if report.confirmed {
        out.push_str(&format!(
            "{n} change(s) applied. Run `env capture` so the twin's drift re-evaluates — that \
             is the oracle, not this report.\n"
        ));
    } else {
        out.push_str(&format!(
            "{n} change(s) would be made. This was a DRY RUN — re-run with `--confirm` to apply \
             them to this machine.\n"
        ));
    }
    out
}

/// Render the report as JSON. **Pure — unit-tested.**
pub fn report_to_json(report: &ApplyReport) -> Value {
    let sections: Vec<Value> = report
        .sections
        .iter()
        .map(|s| {
            json!({
                "section": s.section,
                "status": s.status.wire(),
                "status_detail": s.status.label(),
                "target": s.target,
                "changes": s.changes.iter().map(|c| json!({
                    "key": c.key,
                    "from": c.from,
                    "to": c.to,
                    "detail": c.detail,
                })).collect::<Vec<_>>(),
                "skipped": s.skipped.iter().map(|k| json!({
                    "key": k.key, "reason": k.reason,
                })).collect::<Vec<_>>(),
                "notes": s.notes,
                // Mirrors `plan_to_json`'s per-section `unknown_keys`. A `--json`
                // consumer must not have to prefix-match `status` against
                // `blocked_unmeasured` — and could not learn the KEY NAMES from
                // that word anyway, since they live only in an English sentence.
                "unmeasured_keys": s.unmeasured_keys,
            })
        })
        .collect();

    json!({
        "environment_id": report.environment_id,
        "canonical_machine_name": report.canonical_machine_name,
        "is_canonical_self": report.is_canonical_self,
        "confirmed": report.confirmed,
        "dry_run": !report.confirmed,
        "change_count": report.change_count(),
        // The qualifier on `change_count`, exactly as `unmeasured_key_count`
        // qualifies `in_sync` in `plan_to_json`: `change_count: 0` alone does NOT
        // mean this box needs nothing, and a consumer reading only that would
        // draw the conclusion this whole path exists to forbid.
        //
        // `null` — not `0` — on a canonical-self report. `run_apply` dispatches
        // NOTHING there, so the sum is taken over zero sections; emitting `0`
        // would turn "this surface did no measuring" into the positive claim
        // "every key was measured", and would contradict `env pull --json` on the
        // same box, which reports the real number. Absence-is-not-zero, on the
        // field whose entire job is to say so.
        "unmeasured_key_count": if report.is_canonical_self {
            Value::Null
        } else {
            json!(report.unmeasured_key_count())
        },
        "changed_anything": report.changed_anything(),
        "sections": sections,
    })
}

/// Pull the canonical config, compute the apply, and (with `--confirm`) perform
/// it. Blocking, for the CLI.
pub fn apply_blocking(opts: &ApplyOptions) -> Result<ApplyReport, String> {
    let plan = super::pull::pull_and_plan_blocking()?;
    Ok(run_apply(&plan, opts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env_agent::pull::{compute_plan, CanonicalConfig};
    use serde_json::Map;

    /// Nothing unmeasured: these driver tests are about the apply dispatch, not
    /// about capture-probe outcomes.
    fn plan_with(sections: Value, policy: Value, local: Value, local_machine: &str) -> ApplyPlan {
        plan_with_unknown(sections, policy, local, json!({}), local_machine)
    }

    /// [`plan_with`], plus a local `unknown_keys` map — `section -> [key, ...]`,
    /// the shape [`super::super::ConfigEnvelope::unknown_keys`] puts on the wire.
    fn plan_with_unknown(
        sections: Value,
        policy: Value,
        local: Value,
        unknown: Value,
        local_machine: &str,
    ) -> ApplyPlan {
        let canonical = CanonicalConfig {
            canonical_machine_id: Some("canon".to_string()),
            canonical_machine_name: Some("spaceship".to_string()),
            schema_version: None,
            captured_at: None,
            sections: sections.as_object().unwrap().clone(),
            section_policy: policy.as_object().unwrap().clone(),
            derived_keys: Map::new(),
        };
        compute_plan(
            &canonical,
            local.as_object().unwrap(),
            unknown.as_object().unwrap(),
            "env-1",
            local_machine,
        )
    }

    /// Every stamped `(section, key)` pair, so a test can assert WHICH section
    /// contributed rather than only how many keys did.
    fn unmeasured_by_section(report: &ApplyReport) -> Vec<(&str, &str)> {
        report
            .sections
            .iter()
            .flat_map(|s| {
                s.unmeasured_keys
                    .iter()
                    .map(move |k| (s.section.as_str(), k.as_str()))
            })
            .collect()
    }

    /// `change_count == 0` is a POSITIVE claim about the box, and it is unsound
    /// over a key nobody read.
    ///
    /// The pull side already refuses to make the equivalent claim — an unmeasured
    /// key makes `is_in_sync()` false — and `apply_versions` already refuses to
    /// let such a SECTION report `nothing to do`. The report SUMMARY was the last
    /// surface still saying it, and it is the line an operator actually acts on:
    /// the per-section line would read `mystery [unsupported]` / `versions
    /// [blocked: … could not be measured …]` while three lines below the report
    /// concluded "Nothing to apply on this machine."
    ///
    /// Uses the unimplemented `mystery` section for the same reason the other
    /// driver tests do — these must stay hermetic; the real modules probe the
    /// host. What is under test is the DISPATCH stamp and the summary, neither
    /// of which is section-specific.
    #[test]
    fn an_unmeasured_key_stops_the_summary_claiming_nothing_to_apply() {
        let plan = plan_with_unknown(
            json!({"mystery": {"k": "new"}}),
            json!({"mystery": "applyable"}),
            // `k` is ABSENT locally — the only shape an unmeasured key can have,
            // and what `compute_plan` intersects the claimed set against.
            json!({"mystery": {}}),
            json!({"mystery": ["k"]}),
            "this-box",
        );
        let report = run_apply(&plan, &ApplyOptions::default());

        assert_eq!(report.change_count(), 0);
        assert_eq!(report.unmeasured_key_count(), 1);

        let text = render_report(&report);
        assert!(
            !text.contains("Nothing to apply on this machine."),
            "the unqualified claim must be gone: {text}"
        );
        assert!(
            text.contains("could NOT be measured"),
            "the operator is told WHY the zero is not a clean bill: {text}"
        );
        assert!(
            text.contains("NOT a clean bill of health"),
            "…and told what the zero does not mean: {text}"
        );
        // The key is NAMED in the text, not only counted — for a section whose
        // module emits no note, this is the only place outside `--json` it appears.
        assert!(text.contains("mystery.k"), "{text}");

        // The `--json` consumer gets the same truth without parsing English, and
        // gets the key NAMES, which no status word could carry.
        let v = report_to_json(&report);
        assert_eq!(v["change_count"], 0);
        assert_eq!(v["unmeasured_key_count"], 1);
        assert_eq!(v["sections"][0]["unmeasured_keys"][0], "k");
    }

    /// …and the stamp is scoped to sections that were SELECTED and `Applyable`.
    ///
    /// That scope is the whole reason the summary line is honest rather than
    /// noisy: for a section the operator excluded with `--sections`, or one the
    /// server marked report-only, "nothing to apply" is a statement about
    /// selection or policy — not about what this box managed to read — and
    /// counting its unread keys would attach a measurement warning to a decision
    /// that had nothing to do with measurement.
    #[test]
    fn unmeasured_keys_are_counted_only_for_selected_applyable_sections() {
        let unknown = json!({"mystery": ["k"], "db_schema": ["alembic_head"]});
        let sections = json!({"mystery": {"k": "new"}, "db_schema": {"alembic_head": "abc"}});
        let local = json!({"mystery": {}, "db_schema": {}});

        // Report-only policy: never counted, whatever the operator selects.
        let report = run_apply(
            &plan_with_unknown(
                sections.clone(),
                json!({"mystery": "applyable", "db_schema": "destructive_confirm"}),
                local.clone(),
                unknown.clone(),
                "this-box",
            ),
            &ApplyOptions::default(),
        );
        // The COUNT alone cannot tell "stamped the right section" from "stamped
        // the wrong one" — the key universe is 2 either way — so assert WHICH.
        assert_eq!(unmeasured_by_section(&report), vec![("mystery", "k")]);

        // Applyable but not selected: also never counted.
        let report = run_apply(
            &plan_with_unknown(
                sections,
                json!({"mystery": "applyable", "db_schema": "applyable"}),
                local,
                unknown,
                "this-box",
            ),
            &ApplyOptions {
                sections: vec!["db_schema".to_string()],
                ..Default::default()
            },
        );
        assert_eq!(
            unmeasured_by_section(&report),
            vec![("db_schema", "alembic_head")],
            "the deselected section's unread keys are not this run's business"
        );
        // …and the text names the key rather than only counting it, which for a
        // section whose module emits no note is the only place the name appears
        // outside `--json`.
        let text = render_report(&report);
        assert!(text.contains("db_schema.alembic_head"), "{text}");
        assert!(!text.contains("mystery.k"), "{text}");
    }

    /// A canonical-self report dispatches nothing, so its unmeasured count is a
    /// sum over zero sections — and that must read as `null`, never `0`.
    ///
    /// `0` there would turn "this surface did no measuring" into the positive
    /// claim "every key was measured", and would contradict `env pull --json` on
    /// the very same box, which reports the real number from the same plan. That
    /// is the two-surfaces-disagree defect the pull side of this change removes,
    /// and it must not be reintroduced here.
    #[test]
    fn a_canonical_self_report_reports_null_not_zero_unmeasured() {
        let plan = plan_with_unknown(
            json!({"mystery": {"k": "new"}}),
            json!({"mystery": "applyable"}),
            json!({"mystery": {}}),
            json!({"mystery": ["k"]}),
            // Matches `canonical_machine_id` in the helper — this box IS canonical.
            "canon",
        );
        assert!(plan.is_canonical_self);
        assert_eq!(plan.unmeasured_key_count(), 1, "the PLAN still knows");

        let report = run_apply(&plan, &ApplyOptions::default());
        assert!(report.sections.is_empty(), "nothing is dispatched");
        assert_eq!(report_to_json(&report)["unmeasured_key_count"], Value::Null);
    }

    /// A dry run must never mutate, and must say so in both surfaces.
    ///
    /// Uses an unimplemented section on purpose: these driver tests must stay
    /// hermetic, and the real section modules probe the host (spawning a version
    /// manager, reading `profiles.json`). Each module owns its own tests.
    #[test]
    fn dry_run_is_the_default() {
        let plan = plan_with(
            json!({"mystery": {"k": "new"}}),
            json!({"mystery": "applyable"}),
            json!({"mystery": {"k": "old"}}),
            "this-box",
        );
        let report = run_apply(&plan, &ApplyOptions::default());
        assert!(!report.confirmed);
        assert!(!report.changed_anything());
        assert_eq!(report_to_json(&report)["dry_run"], true);
    }

    /// A section the server did not mark applyable is reported with its policy,
    /// never applied.
    #[test]
    fn non_applyable_sections_are_reported_with_their_policy() {
        let plan = plan_with(
            json!({
                "env_contract": {"QONTINUI_TOKEN": "present"},
                "db_schema": {"alembic_head": "abc"},
            }),
            json!({"env_contract": "secret_report_only", "db_schema": "destructive_confirm"}),
            json!({"env_contract": {}, "db_schema": {"alembic_head": "old"}}),
            "this-box",
        );
        let report = run_apply(
            &plan,
            &ApplyOptions {
                confirm: true,
                ..Default::default()
            },
        );
        let statuses: Vec<&SectionStatus> = report.sections.iter().map(|s| &s.status).collect();
        assert_eq!(
            statuses,
            vec![
                &SectionStatus::NotApplyable(SectionPolicy::DestructiveConfirm),
                &SectionStatus::NotApplyable(SectionPolicy::SecretReportOnly),
            ]
        );
        assert_eq!(report.change_count(), 0);
        let text = render_report(&report);
        assert!(text.contains("secrets: report-only"));
        assert!(text.contains("needs human confirm"));
    }

    /// A section the server marks applyable but this runner has no module for
    /// must read as "unsupported", not as "nothing to do".
    #[test]
    fn applyable_section_without_an_implementation_is_unsupported() {
        let plan = plan_with(
            json!({"mystery": {"k": "new"}}),
            json!({"mystery": "applyable"}),
            json!({"mystery": {"k": "old"}}),
            "this-box",
        );
        let report = run_apply(&plan, &ApplyOptions::default());
        assert_eq!(report.sections[0].status, SectionStatus::Unsupported);
        assert_eq!(report.change_count(), 0);
    }

    /// `--section` narrows the apply; everything else is explicitly
    /// not-selected rather than silently absent.
    #[test]
    fn section_filter_marks_the_rest_not_selected() {
        let plan = plan_with(
            json!({"mystery": {"k": "new"}, "db_schema": {"alembic_head": "a"}}),
            json!({"mystery": "applyable", "db_schema": "destructive_confirm"}),
            json!({"mystery": {"k": "old"}, "db_schema": {"alembic_head": "b"}}),
            "this-box",
        );
        let report = run_apply(
            &plan,
            &ApplyOptions {
                sections: vec!["mystery".to_string()],
                ..Default::default()
            },
        );
        let db = report
            .sections
            .iter()
            .find(|s| s.section == "db_schema")
            .unwrap();
        assert_eq!(db.status, SectionStatus::NotSelected);
    }

    /// The canonical box has nothing to reconcile toward, and must not even
    /// consider a change — `--confirm` included.
    #[test]
    fn canonical_self_applies_nothing() {
        let plan = plan_with(
            json!({"services": {"redis_url": "redis://new:6380"}}),
            json!({"services": "applyable"}),
            json!({"services": {"redis_url": "redis://old:6379"}}),
            // local machine id == canonical machine id
            "canon",
        );
        let report = run_apply(
            &plan,
            &ApplyOptions {
                confirm: true,
                ..Default::default()
            },
        );
        assert!(report.is_canonical_self);
        assert!(report.sections.is_empty());
        assert_eq!(report.change_count(), 0);
        assert!(render_report(&report).contains("IS the canonical environment"));
    }

    /// Both output surfaces carry only what the section module put in
    /// [`AppliedChange`], which is redacted by construction. This pins that
    /// neither surface adds a channel of its own.
    #[test]
    fn secret_safety_both_surfaces_only_carry_redacted_values() {
        let report = ApplyReport {
            environment_id: "env-1".to_string(),
            canonical_machine_name: Some("spaceship".to_string()),
            confirmed: false,
            is_canonical_self: false,
            sections: vec![SectionApply {
                section: "services".to_string(),
                status: SectionStatus::Planned,
                target: Some("profile 'dev'".to_string()),
                changes: vec![AppliedChange {
                    key: "redis_url".to_string(),
                    from: Some("redis://old:6379".to_string()),
                    to: "redis://new:6380".to_string(),
                    detail: Some("local credentials and path preserved".to_string()),
                }],
                skipped: vec![SkipRecord {
                    key: "port_backend_8000".to_string(),
                    reason: "a liveness observation".to_string(),
                }],
                notes: Vec::new(),
                unmeasured_keys: Vec::new(),
            }],
        };
        let text = render_report(&report);
        let as_json = report_to_json(&report).to_string();
        for surface in [&text, &as_json] {
            assert!(surface.contains("redis://new:6380"));
            assert!(!surface.contains('@'), "no userinfo may appear: {surface}");
        }
        assert!(text.contains("would set"));
        assert!(text.contains("DRY RUN"));
    }

    /// Three different situations used to flatten into ONE wire word
    /// (`"blocked"`), leaving a JSON consumer to tell "a key could not be
    /// measured" from "every manager ran and nothing moved" by parsing the
    /// prose. They are different events with different operator actions, so
    /// they get different words — all still prefixed `blocked_`, so a consumer
    /// that only asks "is this section blocked?" keeps working.
    #[test]
    fn each_blocked_cause_gets_its_own_wire_word() {
        let word = |cause: BlockedCause| {
            SectionStatus::Blocked {
                cause,
                reason: "why".to_string(),
            }
            .wire()
        };
        assert_eq!(word(BlockedCause::Unmeasured), "blocked_unmeasured");
        assert_eq!(word(BlockedCause::NoMovement), "blocked_no_movement");
        assert_eq!(word(BlockedCause::Precondition), "blocked_precondition");

        let words = [
            word(BlockedCause::Unmeasured),
            word(BlockedCause::NoMovement),
            word(BlockedCause::Precondition),
        ];
        assert_eq!(
            words
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            3,
            "a consumer must be able to separate them without reading English"
        );
        assert!(
            words.iter().all(|w| w.starts_with("blocked")),
            "the prefix is the compatibility surface for 'is it blocked?'"
        );

        // The human half is unchanged: the label still carries the reason.
        assert_eq!(
            SectionStatus::blocked_precondition("no profiles file").label(),
            "blocked: no profiles file"
        );
    }
}
