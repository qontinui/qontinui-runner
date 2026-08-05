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
use super::{apply_services, apply_versions};

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
    /// The section key this change is for (`database_url`, `node`, …).
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
    Blocked(String),
}

impl SectionStatus {
    pub fn label(&self) -> String {
        match self {
            Self::NotApplyable(p) => format!("not applyable ({})", p.label()),
            Self::Unsupported => "no apply support in this runner".to_string(),
            Self::NotSelected => "not selected".to_string(),
            Self::NothingToDo => "nothing to do".to_string(),
            Self::Planned => "would change".to_string(),
            Self::Applied => "APPLIED".to_string(),
            Self::Blocked(reason) => format!("blocked: {reason}"),
        }
    }

    fn wire(&self) -> &'static str {
        match self {
            Self::NotApplyable(_) => "not_applyable",
            Self::Unsupported => "unsupported",
            Self::NotSelected => "not_selected",
            Self::NothingToDo => "nothing_to_do",
            Self::Planned => "planned",
            Self::Applied => "applied",
            Self::Blocked(_) => "blocked",
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
    match name {
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
        _ => SectionApply::inert(name, SectionStatus::Unsupported),
    }
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
    out.push('\n');
    if n == 0 {
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

    fn plan_with(sections: Value, policy: Value, local: Value, local_machine: &str) -> ApplyPlan {
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
            "env-1",
            local_machine,
        )
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
            json!({"services": {"database_url": "postgres://new:5433"}}),
            json!({"services": "applyable"}),
            json!({"services": {"database_url": "postgres://old:5432"}}),
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
                    key: "database_url".to_string(),
                    from: Some("postgres://old:5432".to_string()),
                    to: "postgres://new:5433".to_string(),
                    detail: Some("local credentials and path preserved".to_string()),
                }],
                skipped: vec![SkipRecord {
                    key: "port_backend_8000".to_string(),
                    reason: "a liveness observation".to_string(),
                }],
                notes: Vec::new(),
            }],
        };
        let text = render_report(&report);
        let as_json = report_to_json(&report).to_string();
        for surface in [&text, &as_json] {
            assert!(surface.contains("postgres://new:5433"));
            assert!(!surface.contains('@'), "no userinfo may appear: {surface}");
        }
        assert!(text.contains("would set"));
        assert!(text.contains("DRY RUN"));
    }
}
