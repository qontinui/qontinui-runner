//! The `[canonical]` gate: let a dispatch require the box to be at the
//! canonical configuration for the toolchains it builds with, and let
//! `env_agent`'s existing convergence machinery satisfy it.
//!
//! Plan `2026-08-08-ci-tool-registry-and-canonical-configuration-parity`,
//! Phase 2.
//!
//! # Why this is a bridge and not a second implementation
//!
//! The runner already contained two provisioning systems that did not know each
//! other existed. `ci_node` provisions dispatch-scoped siblings and tools;
//! `env_agent` (2026-07-02) converges the box's global toolchain versions and
//! service topology toward a canonical machine, with a plan-first, dry-run,
//! explicit-confirm safety model. `grep -rn env_agent src-tauri/src/ci_node/`
//! returned nothing but a disclaimer comment before this module.
//!
//! So a CI dispatch could not ask for the box to be at canonical, and canonical
//! convergence had no idea what CI needed — meaning the lane could run a repo's
//! gates under a `node`/`python`/`rustc` the canonical machine never validated
//! against, and report a green that meant less than it appeared to.
//!
//! Nothing is reimplemented here. This module decides **whether** the
//! requirement is met and **whether** convergence is permitted; the measuring
//! is [`env_agent::pull::pull_and_plan`] and the acting is
//! [`env_agent::apply_versions::apply_section`].
//!
//! # The crate boundary runs downhill, and this is the legal direction
//!
//! `ci_node` is `mod ci_node;` in `main.rs` — the **binary** crate.
//! `env_agent` is `pub mod env_agent;` in `lib.rs` — the **library** crate. So
//! `env_agent → ci_node` is structurally impossible (and `apply_repos.rs` says
//! so in a comment, having duplicated a `ci_node` probe rather than call it),
//! while `ci_node → env_agent` is already routine. **Nothing moves crates to
//! make this work.** Hoisting `ci_node` into the library to "fix" the asymmetry
//! would put a dispatch executor on the library's public surface for no gain.
//!
//! # The sync/async boundary is crossed explicitly
//!
//! [`env_agent::apply::run_apply`] and [`apply_versions::apply_section`] are
//! **synchronous** and shell out to rustup/volta/pyenv — a rustup toolchain is
//! hundreds of megabytes. A dispatch runs on a `tokio::spawn`ed task, so
//! calling them inline would block a tokio worker for the length of a toolchain
//! download. The apply therefore goes through [`tokio::task::spawn_blocking`].
//!
//! The *measurement* half does not: [`pull_and_plan`] is `async` and is awaited
//! directly. That is not a stylistic split. `env_agent` also exposes
//! `pull_and_plan_blocking`, which builds its **own** current-thread runtime
//! and `block_on`s it — convenient for the CLI, and a nested-runtime hazard
//! from inside a runtime. Awaiting the async function is the version with no
//! such question to answer.
//!
//! # Two decisions the plan said must not be left implicit
//!
//! **1. A drifted box with no convergence authority is REFUSED, not run
//! anyway.** Both are defensible and the plan says so; leaving it implicit is
//! the only wrong answer. Refusing wins because the alternative makes the
//! declaration unenforceable: a required check could go green on a box that is
//! not at canonical, and "this build requires canonical" would then mean
//! nothing a repo could rely on. It also matches the sibling decision Phase 4
//! already took for services — a database-backed gate without a container
//! runtime fails loudly rather than turning green — and this lane's standing
//! rule that silence is never success.
//!
//! **2. The requirement comes from the repo; the authority comes from the
//! box.** A manifest declaring `[canonical]` never authorises mutating the
//! owner's global toolchain. `CiNodeSettings::canonical_converge` does, it
//! defaults to false, and only the owner-authored coord directive can set it.
//! With it false the dispatch still does not silently build: it refuses, naming
//! the drift.
//!
//! # Agreement is read, never inferred
//!
//! "Is this box at canonical for `rustc`?" is answered from
//! [`SectionPlan::agreed`] — the positive record of keys both captures carry
//! with the same value — and never from `rustc` failing to appear in the change
//! list. An empty change list is silence, and silence also covers the case
//! where NEITHER capture contains `rustc`: a box with no Rust at all, against a
//! canonical machine with no Rust either, produces no diff row. Gating on the
//! absence would pass that box, which is the reports-success-while-measuring-
//! nothing failure this whole plan exists to remove.

use serde_json::{json, Value};

use qontinui_runner_lib::env_agent::apply::{SectionApply, SectionStatus};
use qontinui_runner_lib::env_agent::apply_versions;
use qontinui_runner_lib::env_agent::pull::{self, Change, SectionPlan};

use super::manifest::CiCanonical;

/// The `versions` section is the only one this gate reads.
const VERSIONS_SECTION: &str = "versions";

/// What the gate concluded, and the provenance that goes into the verdict.
///
/// Every dispatch produces one of these — including a dispatch that declared
/// nothing, which produces [`Outcome::not_requested`]. A consumer reading the
/// result should never have to infer a toolchain claim from a missing field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outcome {
    status: Status,
    /// Per-toolchain provenance: what this box actually ran under.
    toolchains: Vec<ToolchainRecord>,
    canonical_machine: Option<String>,
    /// Present only on [`Status::Refused`].
    reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    /// The manifest declared no `[canonical]`.
    NotRequested,
    /// This box IS the canonical machine.
    IsCanonical,
    /// Every declared toolchain already matched canonical.
    Satisfied,
    /// Declared toolchains drifted; convergence ran and closed the drift.
    Converged,
    /// The requirement is not met and the dispatch must not run.
    Refused,
}

impl Status {
    fn wire(self) -> &'static str {
        match self {
            Self::NotRequested => "not_requested",
            Self::IsCanonical => "is_canonical",
            Self::Satisfied => "satisfied",
            Self::Converged => "converged",
            Self::Refused => "refused",
        }
    }
}

/// One declared toolchain's provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolchainRecord {
    key: String,
    /// The value this box reports, once the gate is done with it. `None` when
    /// the box could not be measured — which is itself a refusal, never a pass.
    local: Option<String>,
    canonical: Option<String>,
    /// `agreed` | `converged` | `drifted` | `unmeasured` | `no_canonical_value`
    /// | `absent_both`.
    state: &'static str,
}

impl Outcome {
    /// The dispatch declared nothing. Recorded EXPLICITLY rather than omitted:
    /// an absent field would make "no claim was made" and "a claim was made and
    /// this consumer does not know about it" the same bytes.
    pub(crate) fn not_requested() -> Self {
        Self {
            status: Status::NotRequested,
            toolchains: Vec::new(),
            canonical_machine: None,
            reason: None,
        }
    }

    /// True when the dispatch must not proceed.
    pub(crate) fn is_refusal(&self) -> bool {
        self.status == Status::Refused
    }

    pub(crate) fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// One line for the dispatch log.
    pub(crate) fn summary_line(&self) -> String {
        match self.status {
            Status::NotRequested => {
                "[ci-node] canonical: not required by this manifest".to_string()
            }
            Status::Refused => format!(
                "[ci-node] canonical: REFUSED — {}",
                self.reason.as_deref().unwrap_or("no reason recorded")
            ),
            _ => format!(
                "[ci-node] canonical: {} ({})",
                self.status.wire(),
                self.render_toolchains()
            ),
        }
    }

    fn render_toolchains(&self) -> String {
        self.toolchains
            .iter()
            .map(|t| {
                format!(
                    "{}={} [{}]",
                    t.key,
                    t.local.as_deref().unwrap_or("<unmeasured>"),
                    t.state
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The verdict block. Lands under `summary.canonical` in the result POST,
    /// which is what coord persists with the dispatch — so a green produced
    /// under a converged toolchain is distinguishable from one produced at
    /// canonical, and both from one that never made the claim.
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "status": self.status.wire(),
            "canonical_machine": self.canonical_machine,
            "reason": self.reason,
            "toolchains": self.toolchains.iter().map(|t| json!({
                "key": t.key,
                "local": t.local,
                "canonical": t.canonical,
                "state": t.state,
            })).collect::<Vec<_>>(),
        })
    }

    fn refused(
        reason: String,
        canonical_machine: Option<String>,
        toolchains: Vec<ToolchainRecord>,
    ) -> Self {
        Self {
            status: Status::Refused,
            toolchains,
            canonical_machine,
            reason: Some(reason),
        }
    }
}

/// How one declared toolchain stands against canonical, before any apply.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Standing {
    Agreed {
        value: String,
    },
    Drifted {
        local: Option<String>,
        canonical: String,
    },
    /// The box could not READ this key. Not "you do not have it".
    Unmeasured,
    /// This box has a value; canonical does not. Nothing to converge TO.
    NoCanonicalValue {
        local: String,
    },
    /// Neither capture carries the key at all.
    AbsentBoth,
}

/// Classify one declared toolchain from the pulled plan. **Pure** — this is the
/// function the gate's whole verdict rests on, so it is unit-tested against
/// hand-built plans rather than only through a live pull.
fn classify(section: &SectionPlan, key: &str) -> Standing {
    // Unmeasured FIRST. An unmeasured key is absent locally, so it also shows
    // up as a `Missing` change row; reading the row first would call it drift
    // and hand it to an apply that would install over a version nobody read.
    if section.is_unknown(key) {
        return Standing::Unmeasured;
    }
    if let Some(value) = section.agreed.get(key) {
        return Standing::Agreed {
            value: value.clone(),
        };
    }
    for change in &section.changes {
        if change.key() != key {
            continue;
        }
        return match change {
            Change::Missing { canonical, .. } => Standing::Drifted {
                local: None,
                canonical: canonical.clone(),
            },
            Change::Differs {
                local, canonical, ..
            } => Standing::Drifted {
                local: Some(local.clone()),
                canonical: canonical.clone(),
            },
            Change::Extra { local, .. } => Standing::NoCanonicalValue {
                local: local.clone(),
            },
        };
    }
    Standing::AbsentBoth
}

/// Build the narrowed section handed to the apply.
///
/// Only the DECLARED keys survive, so the blast radius of a convergence is
/// exactly the requirement: a manifest asking for `rustc` must not also
/// rewrite the owner's python. `apply_versions::apply_section` acts on every
/// actionable key in the section it is given, so narrowing the section IS the
/// scoping mechanism — there is no per-key argument to pass instead.
///
/// `derived_keys` and `unknown_keys` are carried through (filtered to the same
/// keys) rather than dropped: they are what `SectionPlan::actionable` uses to
/// refuse to act on a repo-derived or never-measured key, and a narrowed plan
/// that lost them would be a narrowed plan with the safety filters switched
/// off.
fn narrow(section: &SectionPlan, keys: &[String]) -> SectionPlan {
    let wanted = |k: &str| keys.iter().any(|d| d == k);
    SectionPlan {
        section: section.section.clone(),
        policy: section.policy,
        changes: section
            .changes
            .iter()
            .filter(|c| wanted(c.key()))
            .cloned()
            .collect(),
        local_section_absent: section.local_section_absent,
        derived_keys: section
            .derived_keys
            .iter()
            .filter(|k| wanted(k))
            .cloned()
            .collect(),
        unknown_keys: section
            .unknown_keys
            .iter()
            .filter(|k| wanted(k))
            .cloned()
            .collect(),
        agreed: section
            .agreed
            .iter()
            .filter(|(k, _)| wanted(k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    }
}

/// Read the applied report and say, per declared key, whether the drift closed.
///
/// `SectionStatus::Applied` alone is not enough: it is a section-level word,
/// and a section can be `Applied` because ONE key moved while another was
/// skipped. The per-key evidence is `changes` (what moved) and `skipped` (what
/// did not, and why), so both are read.
fn converged_keys(applied: &SectionApply) -> Vec<String> {
    if !matches!(applied.status, SectionStatus::Applied) {
        return Vec::new();
    }
    applied.changes.iter().map(|c| c.key.clone()).collect()
}

/// Why an apply did not close the drift, in the apply's own words.
fn apply_failure_reason(applied: &SectionApply) -> String {
    let mut parts: Vec<String> = vec![format!("apply reported '{}'", applied.status.label())];
    for skip in &applied.skipped {
        parts.push(format!("{}: {}", skip.key, skip.reason));
    }
    parts.join("; ")
}

/// Run the gate.
///
/// Returns `Ok(outcome)` for every reachable conclusion INCLUDING a refusal —
/// the refusal is a verdict with provenance, not an error string, because it
/// has to reach the dispatch result rather than only a log line. `Err` is
/// reserved for "the gate could not run at all" (the canonical pull failed),
/// which is also a refusal but carries no per-toolchain detail.
pub(crate) async fn ensure(
    declared: Option<&CiCanonical>,
    converge_authorized: bool,
    log: &mut (dyn FnMut(String) + Send),
) -> Outcome {
    let Some(declared) = declared else {
        return Outcome::not_requested();
    };
    let keys: Vec<String> = declared.toolchains.clone();

    log(format!(
        "[ci-node] canonical: manifest requires {keys:?}; convergence authority on this box: {}",
        if converge_authorized {
            "GRANTED"
        } else {
            "withheld (ci_node.canonical_converge = false)"
        }
    ));

    // Measurement half — async, awaited directly. See the module docs on why
    // this deliberately does not use `pull_and_plan_blocking`.
    let plan = match pull::pull_and_plan().await {
        Ok(p) => p,
        Err(e) => {
            // A gate that cannot measure must not pass. This is the same
            // `silent-empty-is-unknown` rule the rest of the module runs on:
            // an unreachable canonical is UNKNOWN, and unknown is not "at
            // canonical".
            return Outcome::refused(
                format!(
                    "could not read the canonical configuration ({e}). The manifest requires \
                     this box to be at canonical for {keys:?}, and a requirement that cannot \
                     be checked is not a requirement that passed"
                ),
                None,
                Vec::new(),
            );
        }
    };
    let machine = plan
        .canonical_machine_name
        .clone()
        .or_else(|| Some(plan.canonical_machine_id.clone()))
        .filter(|s| !s.is_empty());

    // NOTE what does NOT happen here: `plan.is_canonical_self` does not
    // short-circuit the check.
    //
    // "This box defines canonical, therefore it is at canonical" is true, and
    // it is also not the question the manifest asked. A canonical machine with
    // no Rust installed still cannot run a build that requires `rustc`, and
    // returning satisfied on the flag alone would pass it — while every OTHER
    // box declaring `rustc` would be refused for canonical carrying no value.
    // One box exempt from the check it defines is exactly the shape of vacuous
    // green this lane keeps rediscovering.
    //
    // What the flag DOES change is the predicate below: a self box is compared
    // against its own last uploaded capture, so a difference means "this box
    // has moved since it last published", not "this box disagrees with
    // canonical". Presence and measurement are still required; agreement with
    // a stale snapshot of itself is not.
    if plan.is_canonical_self {
        log("[ci-node] canonical: this box IS the canonical machine — requiring the declared toolchains to be present and measured, not to match its own last upload".to_string());
    }

    let Some(section) = plan.sections.iter().find(|s| s.section == VERSIONS_SECTION) else {
        return Outcome::refused(
            format!(
                "the canonical configuration carries no '{VERSIONS_SECTION}' section, so there \
                 is nothing to compare {keys:?} against"
            ),
            machine,
            Vec::new(),
        );
    };
    if section.local_section_absent {
        return Outcome::refused(
            format!(
                "this box produced no '{VERSIONS_SECTION}' capture at all, so it cannot be \
                 shown to be at canonical for {keys:?}. Nothing measured is not nothing wrong"
            ),
            machine,
            Vec::new(),
        );
    }

    let standings: Vec<(String, Standing)> = keys
        .iter()
        .map(|k| (k.clone(), classify(section, k)))
        .collect();

    // Refusals that no amount of convergence could fix are decided BEFORE any
    // apply, so a box that cannot satisfy the requirement never pays for a
    // toolchain download first.
    let mut records: Vec<ToolchainRecord> = Vec::new();
    let mut hard: Vec<String> = Vec::new();
    let mut drifted: Vec<String> = Vec::new();
    let self_canonical = plan.is_canonical_self;
    for (key, standing) in &standings {
        match standing {
            Standing::Agreed { value } => records.push(ToolchainRecord {
                key: key.clone(),
                local: Some(value.clone()),
                canonical: Some(value.clone()),
                state: "agreed",
            }),
            // On a NON-canonical box this is drift to close. On the canonical
            // box itself it is only "moved since the last upload", and the
            // requirement is met as long as the toolchain is actually there —
            // converging the canonical machine toward its own stale snapshot
            // would be backwards.
            Standing::Drifted { local, canonical } => {
                if self_canonical && local.is_some() {
                    records.push(ToolchainRecord {
                        key: key.clone(),
                        local: local.clone(),
                        canonical: Some(canonical.clone()),
                        state: "canonical_self",
                    });
                } else if self_canonical {
                    // The stored capture has the key; this box does not report
                    // it NOW. There is nothing to converge toward but itself.
                    hard.push(format!(
                        "{key}: this box defines canonical but does not currently report it"
                    ));
                    records.push(ToolchainRecord {
                        key: key.clone(),
                        local: None,
                        canonical: Some(canonical.clone()),
                        state: "absent_locally",
                    });
                } else {
                    drifted.push(key.clone());
                    records.push(ToolchainRecord {
                        key: key.clone(),
                        local: local.clone(),
                        canonical: Some(canonical.clone()),
                        state: "drifted",
                    });
                }
            }
            Standing::Unmeasured => {
                hard.push(format!(
                    "{key}: this box could not measure it (the capture probe did not answer), \
                     and an unread value is not an agreeing value"
                ));
                records.push(ToolchainRecord {
                    key: key.clone(),
                    local: None,
                    canonical: None,
                    state: "unmeasured",
                });
            }
            Standing::NoCanonicalValue { local } => {
                // Same asymmetry: on the canonical box this only means the key
                // post-dates its last upload. It is measured and present, which
                // is the whole requirement there.
                if self_canonical {
                    records.push(ToolchainRecord {
                        key: key.clone(),
                        local: Some(local.clone()),
                        canonical: None,
                        state: "canonical_self",
                    });
                } else {
                    hard.push(format!(
                        "{key}: the canonical machine reports no value for it, so there is no canonical state to be at"
                    ));
                    records.push(ToolchainRecord {
                        key: key.clone(),
                        local: Some(local.clone()),
                        canonical: None,
                        state: "no_canonical_value",
                    });
                }
            }
            Standing::AbsentBoth => {
                hard.push(format!(
                    "{key}: neither this box nor the canonical machine reports it. An empty \
                     diff between two absences is not agreement"
                ));
                records.push(ToolchainRecord {
                    key: key.clone(),
                    local: None,
                    canonical: None,
                    state: "absent_both",
                });
            }
        }
    }
    if !hard.is_empty() {
        return Outcome::refused(
            format!(
                "this box cannot be shown to be at canonical ({}) — {}",
                machine.as_deref().unwrap_or("unknown machine"),
                hard.join("; ")
            ),
            machine,
            records,
        );
    }
    if drifted.is_empty() {
        log("[ci-node] canonical: every declared toolchain already matches".to_string());
        return Outcome {
            status: if self_canonical {
                Status::IsCanonical
            } else {
                Status::Satisfied
            },
            toolchains: records,
            canonical_machine: machine,
            reason: None,
        };
    }

    if !converge_authorized {
        return Outcome::refused(
            format!(
                "{} drifted from canonical ({}), and this box has not authorised convergence. \
                 Enable ci-node 'canonical_converge' for this device to let a dispatch drive \
                 the version managers, or bring the box to canonical yourself with \
                 `qontinui env apply --confirm`. The dispatch is refused rather than run under \
                 a toolchain the canonical machine never validated",
                drifted.join(", "),
                machine.as_deref().unwrap_or("unknown machine")
            ),
            machine,
            records,
        );
    }

    // Acting half — synchronous, shells out to rustup/volta/pyenv, so it goes
    // on the blocking pool rather than a tokio worker.
    log(format!(
        "[ci-node] canonical: converging {drifted:?} toward {}",
        machine.as_deref().unwrap_or("canonical")
    ));
    let narrowed = narrow(section, &keys);
    let applied =
        match tokio::task::spawn_blocking(move || apply_versions::apply_section(&narrowed, true))
            .await
        {
            Ok(a) => a,
            Err(e) => {
                return Outcome::refused(
                    format!("the convergence task did not complete ({e})"),
                    machine,
                    records,
                );
            }
        };
    for note in &applied.notes {
        log(format!("[ci-node] canonical: {note}"));
    }

    let moved = converged_keys(&applied);
    let still: Vec<&String> = drifted.iter().filter(|k| !moved.contains(k)).collect();
    if !still.is_empty() {
        return Outcome::refused(
            format!(
                "convergence did not bring {} to canonical — {}",
                still
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                apply_failure_reason(&applied)
            ),
            machine,
            records,
        );
    }
    for change in &applied.changes {
        if let Some(record) = records.iter_mut().find(|r| r.key == change.key) {
            record.local = Some(change.to.clone());
            record.state = "converged";
        }
    }
    Outcome {
        status: Status::Converged,
        toolchains: records,
        canonical_machine: machine,
        reason: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_runner_lib::env_agent::apply::{AppliedChange, SkipRecord};
    use qontinui_runner_lib::env_agent::pull::SectionPolicy;
    use std::collections::{BTreeMap, BTreeSet};

    fn section(
        changes: Vec<Change>,
        agreed: &[(&str, &str)],
        unknown: &[&str],
        derived: &[&str],
    ) -> SectionPlan {
        SectionPlan {
            section: VERSIONS_SECTION.to_string(),
            policy: SectionPolicy::Applyable,
            changes,
            local_section_absent: false,
            derived_keys: derived
                .iter()
                .map(|s| s.to_string())
                .collect::<BTreeSet<_>>(),
            unknown_keys: unknown
                .iter()
                .map(|s| s.to_string())
                .collect::<BTreeSet<_>>(),
            agreed: agreed
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    /// THE test for this module. A key that appears in NEITHER capture produces
    /// no change row, so any gate written against "is it in `changes`?" passes
    /// it — reporting a box as at-canonical for a toolchain neither side has.
    #[test]
    fn absence_on_both_sides_is_not_agreement() {
        let s = section(vec![], &[("node", "v22.11.0")], &[], &[]);
        assert_eq!(
            classify(&s, "node"),
            Standing::Agreed {
                value: "v22.11.0".to_string()
            }
        );
        // `rustc` is in no change row and in no agreed entry.
        assert_eq!(classify(&s, "rustc"), Standing::AbsentBoth);
        // And the two must not be confused by the change list alone, which is
        // empty for both.
        assert!(s.changes.is_empty());
    }

    /// An unmeasured key diffs as `Missing`, so the unmeasured check has to run
    /// BEFORE the change scan or it would be classified as ordinary drift and
    /// handed to an apply that installs over a version nobody read.
    #[test]
    fn unmeasured_beats_the_missing_row_it_also_produces() {
        let s = section(
            vec![Change::Missing {
                key: "python".to_string(),
                canonical: "3.12.4".to_string(),
            }],
            &[],
            &["python"],
            &[],
        );
        assert_eq!(classify(&s, "python"), Standing::Unmeasured);
    }

    /// Canonical having no value is unsatisfiable, not drift: there is nothing
    /// to converge toward, and `actionable()` would never act on it either.
    #[test]
    fn an_extra_local_key_is_unsatisfiable_not_drift() {
        let s = section(
            vec![Change::Extra {
                key: "rustc".to_string(),
                local: "1.95.0".to_string(),
            }],
            &[],
            &[],
            &[],
        );
        assert_eq!(
            classify(&s, "rustc"),
            Standing::NoCanonicalValue {
                local: "1.95.0".to_string()
            }
        );
    }

    #[test]
    fn real_drift_is_classified_with_both_values() {
        let s = section(
            vec![Change::Differs {
                key: "node".to_string(),
                local: "v20.9.0".to_string(),
                canonical: "v22.11.0".to_string(),
            }],
            &[],
            &[],
            &[],
        );
        assert_eq!(
            classify(&s, "node"),
            Standing::Drifted {
                local: Some("v20.9.0".to_string()),
                canonical: "v22.11.0".to_string()
            }
        );
    }

    /// Narrowing is the blast-radius control: a manifest requiring `rustc` must
    /// not hand python's drift to the apply.
    #[test]
    fn narrowing_keeps_only_the_declared_keys_and_their_safety_sets() {
        let s = section(
            vec![
                Change::Differs {
                    key: "rustc".to_string(),
                    local: "1.90.0".to_string(),
                    canonical: "1.95.0".to_string(),
                },
                Change::Differs {
                    key: "python".to_string(),
                    local: "3.11.0".to_string(),
                    canonical: "3.12.4".to_string(),
                },
            ],
            &[("node", "v22.11.0")],
            &["python"],
            &["python"],
        );
        let n = narrow(&s, &["rustc".to_string()]);
        assert_eq!(n.changes.len(), 1);
        assert_eq!(n.changes[0].key(), "rustc");
        assert!(n.agreed.is_empty(), "node was not declared");
        assert!(n.unknown_keys.is_empty(), "python was not declared");
        assert!(n.derived_keys.is_empty(), "python was not declared");
        // Policy must survive: `actionable()` returns nothing for a
        // non-applyable section, so dropping it would silently disarm the apply.
        assert_eq!(n.policy, SectionPolicy::Applyable);
    }

    /// The safety filters must survive narrowing. A declared key that is BOTH
    /// drifted and repo-derived stays non-actionable in the narrowed plan.
    #[test]
    fn narrowing_preserves_the_filters_for_declared_keys() {
        let s = section(
            vec![Change::Differs {
                key: "python".to_string(),
                local: "3.11.0".to_string(),
                canonical: "3.12.4".to_string(),
            }],
            &[],
            &[],
            &["python"],
        );
        let n = narrow(&s, &["python".to_string()]);
        assert!(n.derived_keys.contains("python"));
        assert!(
            n.actionable().is_empty(),
            "a repo-derived key must stay non-actionable after narrowing"
        );
    }

    fn applied(
        status: SectionStatus,
        changes: &[(&str, &str)],
        skipped: &[(&str, &str)],
    ) -> SectionApply {
        SectionApply {
            section: VERSIONS_SECTION.to_string(),
            status,
            target: None,
            changes: changes
                .iter()
                .map(|(k, to)| AppliedChange {
                    key: k.to_string(),
                    from: None,
                    to: to.to_string(),
                    detail: None,
                })
                .collect(),
            skipped: skipped
                .iter()
                .map(|(k, r)| SkipRecord {
                    key: k.to_string(),
                    reason: r.to_string(),
                })
                .collect(),
            notes: Vec::new(),
            // `dispatch` is the ONE site that POPULATES `unmeasured_keys`
            // (see the field's doc on `SectionApply`); it overwrites whatever
            // a section module returned. This fixture builds the struct
            // directly, bypassing `dispatch`, so the empty set is the honest
            // value: it says nothing about an unread key. Do NOT "fix" this by
            // populating it — the two functions this file hands the value to
            // (`converged_keys`, `apply_failure_reason`) never read the field,
            // and this module's own unread-key defence is `classify`'s
            // `is_unknown` check, not this vec.
            unmeasured_keys: Vec::new(),
        }
    }

    /// `Applied` is a SECTION-level word. One key moving while another is
    /// skipped is still `Applied`, so the gate reads the per-key change list.
    #[test]
    fn a_partially_applied_section_does_not_count_every_key_as_converged() {
        let a = applied(
            SectionStatus::Applied,
            &[("rustc", "1.95.0")],
            &[("node", "no supported version manager detected")],
        );
        let moved = converged_keys(&a);
        assert_eq!(moved, vec!["rustc".to_string()]);
        assert!(!moved.contains(&"node".to_string()));
        assert!(apply_failure_reason(&a).contains("no supported version manager"));
    }

    /// A section that is not `Applied` converged nothing, whatever it lists.
    #[test]
    fn a_non_applied_section_converged_nothing() {
        let a = applied(
            SectionStatus::blocked_precondition("no supported version manager detected"),
            &[("rustc", "1.95.0")],
            &[],
        );
        assert!(converged_keys(&a).is_empty());
    }

    /// The canonical machine is NOT exempt from its own check.
    ///
    /// `is_canonical_self` used to return satisfied with an empty toolchain
    /// list, which passed a box that had none of the declared toolchains while
    /// every other box declaring the same key would be refused. What the flag
    /// legitimately changes is only WHICH question is asked — presence and
    /// measurement instead of agreement with its own last upload — so these
    /// assertions are about `classify`, the function that answers it, on the
    /// exact standings a self box produces.
    #[test]
    fn the_canonical_box_still_has_to_report_the_toolchain() {
        // Moved since its last upload: measured, present, satisfies a self box.
        let moved = section(
            vec![Change::Differs {
                key: "rustc".to_string(),
                local: "1.95.0".to_string(),
                canonical: "1.90.0".to_string(),
            }],
            &[],
            &[],
            &[],
        );
        assert!(matches!(
            classify(&moved, "rustc"),
            Standing::Drifted { local: Some(_), .. }
        ));

        // Newer than its last upload: also measured and present.
        let newer = section(
            vec![Change::Extra {
                key: "rustc".to_string(),
                local: "1.95.0".to_string(),
            }],
            &[],
            &[],
            &[],
        );
        assert!(matches!(
            classify(&newer, "rustc"),
            Standing::NoCanonicalValue { .. }
        ));

        // Gone from the box entirely: `local` is None, which is what the self
        // arm refuses on. A canonical machine with no rustc cannot run a build
        // that requires rustc.
        let gone = section(
            vec![Change::Missing {
                key: "rustc".to_string(),
                canonical: "1.90.0".to_string(),
            }],
            &[],
            &[],
            &[],
        );
        assert_eq!(
            classify(&gone, "rustc"),
            Standing::Drifted {
                local: None,
                canonical: "1.90.0".to_string()
            }
        );

        // Never installed on either side: still absent, on the canonical box
        // as much as anywhere else.
        let never = section(vec![], &[], &[], &[]);
        assert_eq!(classify(&never, "rustc"), Standing::AbsentBoth);
    }

    /// The verdict must distinguish "no claim" from every claim, and say so in
    /// bytes rather than by an absent field.
    #[test]
    fn not_requested_is_stated_not_omitted() {
        let o = Outcome::not_requested();
        assert!(!o.is_refusal());
        assert_eq!(o.to_json()["status"], "not_requested");
        assert!(o.summary_line().contains("not required"));
    }

    /// A refusal carries its reason into the verdict, because the dispatch
    /// result is where an operator will look — not the runner's local log.
    #[test]
    fn a_refusal_carries_its_reason_into_the_verdict() {
        let o = Outcome::refused(
            "node drifted from canonical".to_string(),
            Some("spaceship".to_string()),
            vec![ToolchainRecord {
                key: "node".to_string(),
                local: Some("v20.9.0".to_string()),
                canonical: Some("v22.11.0".to_string()),
                state: "drifted",
            }],
        );
        assert!(o.is_refusal());
        let v = o.to_json();
        assert_eq!(v["status"], "refused");
        assert_eq!(v["canonical_machine"], "spaceship");
        assert_eq!(v["toolchains"][0]["local"], "v20.9.0");
        assert_eq!(v["toolchains"][0]["canonical"], "v22.11.0");
        assert!(o.reason().unwrap().contains("drifted"));
    }
}
