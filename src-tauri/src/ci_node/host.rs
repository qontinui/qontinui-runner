//! The runner's implementations of the executor's host traits
//! (`qontinui_ci_exec::host`), and the seam that runs one admitted dispatch.
//!
//! Each impl is a thin adapter over machinery the runner already has: the
//! CI-node settings, the process-wide GitHub budget and ETag cache, the
//! process helpers (console suppression, git prompt posture, the child-tree
//! reaper, the global and per-dispatch Job Objects), the external-volume
//! declaration, the workspace root, and `env_agent`'s canonical-configuration
//! pull and apply.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use qontinui_ci_exec::canonical::VERSIONS_SECTION;
use qontinui_ci_exec::dispatch::DispatchPayload;
use qontinui_ci_exec::host::{
    BoxFuture, CanonicalConvergence, CanonicalReading, CiSettings, ConvergeReport, GithubAccess,
    Host, HostIdentity, ProcessSpawn, Standing, StepContainment, TreeGuard, VersionsReading,
    VolumeState,
};
use qontinui_ci_exec::host_sizing::HostCapacity;
use qontinui_runner_lib::env_agent::apply::{SectionApply, SectionStatus};
use qontinui_runner_lib::env_agent::apply_versions;
use qontinui_runner_lib::env_agent::pull::{self, Change, SectionPlan};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

use super::reporting::CoordReporter;
use crate::process_helpers::ChildTreeGuard;

/// Run one dispatch that admission let through: resolve where it reports,
/// assemble the runner host, and hand both to the executor.
///
/// `capacity` and `max_concurrent` are the probe and resolved capacity
/// `admission::submit` admitted against, threaded through so the executor's
/// per-dispatch host share uses the same N.
pub(crate) async fn run(
    payload: DispatchPayload,
    cancel: CancellationToken,
    capacity: HostCapacity,
    max_concurrent: u32,
) {
    let pinned = payload.coord_http_url.trim().trim_end_matches('/');
    let base = if pinned.is_empty() {
        match qontinui_runner_lib::profiles::connected_coord_base() {
            Some(b) => b,
            None => {
                warn!(
                    "ci_node: dispatch {} has no coord_http_url and no profile coord base — \
                     cannot report; dropping",
                    payload.dispatch_id
                );
                return;
            }
        }
    } else {
        pinned.to_string()
    };
    let reporter = CoordReporter::start(base, payload.dispatch_id.clone(), cancel.clone());
    qontinui_ci_exec::executor::run_dispatch(
        &runner_host(),
        Box::new(reporter),
        payload,
        cancel,
        capacity,
        max_concurrent,
    )
    .await;
}

/// The runner, as an executor host.
fn runner_host() -> Host {
    Host {
        settings: Arc::new(RunnerSettings),
        github: Arc::new(RunnerGithub),
        process: Arc::new(RunnerProcess),
        volume: Arc::new(RunnerVolume),
        identity: Arc::new(RunnerIdentity),
        canonical: Arc::new(EnvAgentCanonical),
    }
}

/// `ci_node.*` from the runner's settings, read fresh per call.
struct RunnerSettings;

impl CiSettings for RunnerSettings {
    fn canonical_converge(&self) -> bool {
        crate::settings::get_ci_node_settings().canonical_converge
    }
}

/// The budget-histogram label for every GitHub read the executor makes here.
const BUDGET_CONSUMER: &str = "ci_node_sibling";

/// The runner's GitHub token resolver, and its process-wide ETag cache and
/// request-budget histogram — so a sibling probe lands in the same books as
/// every other GitHub read this runner makes.
struct RunnerGithub;

impl GithubAccess for RunnerGithub {
    fn token(&self) -> BoxFuture<'_, Option<String>> {
        Box::pin(crate::session_pr_reconciler::resolve_github_token())
    }
    fn cached_etag(&self, url: &str) -> Option<String> {
        crate::github_budget::etag_for(url)
    }
    fn replay(&self, url: &str) -> Option<Vec<u8>> {
        crate::github_budget::replay(url).map(|b| b.to_vec())
    }
    fn store(&self, url: &str, etag: &str, body: &[u8]) {
        crate::github_budget::store(url, etag, Bytes::copy_from_slice(body));
    }
    fn invalidate(&self, url: &str) {
        crate::github_budget::invalidate(url);
    }
    fn record_response(&self, url: &str, status: u16, headers: &reqwest::header::HeaderMap) {
        crate::github_budget::record_response(
            BUDGET_CONSUMER,
            url,
            crate::github_budget::CacheMode::Cached,
            status,
            headers,
        );
    }
    fn record_transport_error(&self, url: &str) {
        crate::github_budget::record_transport_error(
            BUDGET_CONSUMER,
            url,
            crate::github_budget::CacheMode::Cached,
        );
    }
}

/// The runner's process posture: `CREATE_NO_WINDOW`, the prompt-proof git
/// environment, the [`ChildTreeGuard`] reaper, the Job Objects, and the
/// tracked blocking pool.
pub(crate) struct RunnerProcess;

struct RunnerTree(ChildTreeGuard);

impl TreeGuard for RunnerTree {
    fn disarm(self: Box<Self>) {
        self.0.disarm();
    }
}

impl ProcessSpawn for RunnerProcess {
    fn command(&self, program: &str) -> tokio::process::Command {
        crate::process_helpers::tokio_no_window(program)
    }
    fn std_command(&self, program: &str) -> std::process::Command {
        crate::process_helpers::no_window(program)
    }
    fn arm_tree(&self, cmd: &mut tokio::process::Command) {
        ChildTreeGuard::arm_tokio(cmd);
    }
    fn attach_tree(&self, child: &tokio::process::Child) -> Box<dyn TreeGuard> {
        Box::new(RunnerTree(ChildTreeGuard::attach_armed_tokio(child)))
    }
    fn step_containment(&self) -> Box<dyn StepContainment> {
        Box::new(DispatchJob::create())
    }
    /// Through the runner's tracked blocking lanes, so the executor's archive
    /// extraction is counted with every other blocking body this runner runs.
    fn run_blocking(
        &self,
        job: Box<dyn FnOnce() + Send>,
    ) -> BoxFuture<'static, Result<(), String>> {
        let handle = spawn_blocking_tracked(job);
        Box::pin(async move { handle.await.map_err(|e| e.to_string()) })
    }
}

/// Job-wide committed-memory ceiling for one dispatch's process tree
/// (Windows). Deliberately generous — rustc at `CARGO_BUILD_JOBS=1` on this
/// workspace legitimately commits >10 GiB — because this is a runaway
/// backstop, not a tuning knob; the real throttle is the jobs cap.
#[cfg(windows)]
const CI_JOB_MEMORY_LIMIT_BYTES: usize = 32 * 1024 * 1024 * 1024;

/// One dispatch's step containment: every step child joins the runner's
/// global kill-on-close Job Object (like every other runner child) AND a
/// per-dispatch Job Object carrying the memory backstop (plan §4.6), whose
/// kill-on-close reaps strays when the dispatch drops it. A no-op on
/// non-Windows, so the executor stays platform-uniform.
struct DispatchJob {
    #[cfg(windows)]
    inner: Option<qontinui_runner_win32::ScopedKillOnCloseJob>,
}

impl DispatchJob {
    fn create() -> Self {
        #[cfg(windows)]
        {
            let inner = qontinui_runner_win32::ScopedKillOnCloseJob::create(Some(
                CI_JOB_MEMORY_LIMIT_BYTES,
            ));
            if inner.is_none() {
                warn!(
                    "ci_node: per-dispatch memory-limit job unavailable — \
                     builds run with the global kill-on-close job only"
                );
            }
            Self { inner }
        }
        #[cfg(not(windows))]
        {
            Self {}
        }
    }
}

impl StepContainment for DispatchJob {
    #[cfg_attr(not(windows), allow(unused_variables))]
    fn adopt(&self, child: &tokio::process::Child) {
        #[cfg(windows)]
        if let Some(raw) = child.raw_handle() {
            // SAFETY: `raw` came from the live `child` the caller still owns.
            unsafe {
                qontinui_runner_win32::assign_process_to_job(
                    raw as windows_sys::Win32::Foundation::HANDLE,
                );
                if let Some(job) = self.inner.as_ref() {
                    job.assign(raw as windows_sys::Win32::Foundation::HANDLE);
                }
            }
        }
    }
}

/// The external-volume declaration (`external_volume`).
struct RunnerVolume;

impl VolumeState for RunnerVolume {
    fn refusal_reason(&self, root: &Path) -> Option<String> {
        crate::external_volume::external_state_for(root).and_then(|s| s.refusal_reason(root))
    }
}

/// `QONTINUI_ROOT` and this machine's host name.
struct RunnerIdentity;

impl HostIdentity for RunnerIdentity {
    fn ci_root(&self) -> Result<PathBuf, String> {
        crate::agent_runtime::qontinui_root_dir()
            .ok_or_else(|| "QONTINUI_ROOT not resolvable on this device".to_string())
    }
    fn label(&self) -> String {
        format!(
            "{} (runner ci-node)",
            sysinfo::System::host_name().unwrap_or_else(|| "this runner".to_string())
        )
    }
}

/// `env_agent`'s canonical-configuration machinery: the async pull-and-plan
/// measures, the synchronous rustup/volta/pyenv apply converges.
///
/// The measurement awaits `pull::pull_and_plan` directly rather than
/// `pull_and_plan_blocking`, which builds its OWN current-thread runtime and
/// `block_on`s it — a nested-runtime hazard from inside one. The apply shells
/// out and downloads toolchains (hundreds of megabytes), so it runs on the
/// blocking pool, never a tokio worker.
///
/// Stateless: `converge` pulls again rather than reusing `measure`'s plan, so
/// it narrows and applies against the box as it stands at that moment.
struct EnvAgentCanonical;

fn versions_section(plan: &pull::ApplyPlan) -> Option<&SectionPlan> {
    plan.sections.iter().find(|s| s.section == VERSIONS_SECTION)
}

impl CanonicalConvergence for EnvAgentCanonical {
    fn measure<'a>(
        &'a self,
        keys: &'a [String],
    ) -> BoxFuture<'a, Result<CanonicalReading, String>> {
        Box::pin(async move {
            let plan = pull::pull_and_plan().await?;
            let versions = match versions_section(&plan) {
                None => VersionsReading::NoCanonicalSection,
                Some(section) if section.local_section_absent => {
                    VersionsReading::LocalSectionAbsent
                }
                Some(section) => VersionsReading::Standings(
                    keys.iter()
                        .map(|k| (k.clone(), classify(section, k)))
                        .collect(),
                ),
            };
            Ok(CanonicalReading {
                canonical_machine: plan
                    .canonical_machine_name
                    .clone()
                    .or_else(|| Some(plan.canonical_machine_id.clone())),
                is_canonical_self: plan.is_canonical_self,
                versions,
            })
        })
    }

    fn converge<'a>(&'a self, keys: &'a [String]) -> BoxFuture<'a, Result<ConvergeReport, String>> {
        Box::pin(async move {
            let plan = pull::pull_and_plan().await?;
            let section = versions_section(&plan).ok_or_else(|| {
                format!(
                    "the canonical configuration no longer carries a '{VERSIONS_SECTION}' section"
                )
            })?;
            let narrowed = narrow(section, keys);
            let applied =
                spawn_blocking_tracked(move || apply_versions::apply_section(&narrowed, true))
                    .await
                    .map_err(|e| e.to_string())?;
            Ok(ConvergeReport {
                moved: converged(&applied),
                notes: applied.notes.clone(),
                failure_reason: apply_failure_reason(&applied),
            })
        })
    }
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

/// Read the applied report and say, per key, what the drift closed TO.
///
/// `SectionStatus::Applied` alone is not enough: it is a section-level word,
/// and a section can be `Applied` because ONE key moved while another was
/// skipped. The per-key evidence is `changes` (what moved) and `skipped` (what
/// did not, and why), so both are read.
fn converged(applied: &SectionApply) -> Vec<(String, String)> {
    if !matches!(applied.status, SectionStatus::Applied) {
        return Vec::new();
    }
    applied
        .changes
        .iter()
        .map(|c| (c.key.clone(), c.to.clone()))
        .collect()
}

/// Why an apply did not close the drift, in the apply's own words.
fn apply_failure_reason(applied: &SectionApply) -> String {
    let mut parts: Vec<String> = vec![format!("apply reported '{}'", applied.status.label())];
    for skip in &applied.skipped {
        parts.push(format!("{}: {}", skip.key, skip.reason));
    }
    parts.join("; ")
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
            // (`converged`, `apply_failure_reason`) never read the field,
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
        let moved = converged(&a);
        assert_eq!(moved, vec![("rustc".to_string(), "1.95.0".to_string())]);
        assert!(!moved.iter().any(|(k, _)| k == "node"));
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
        assert!(converged(&a).is_empty());
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

    /// Cancellation of a fetch that is IN FLIGHT, and what it must kill. The
    /// mirror is a local TCP listener that accepts the connection and never
    /// answers, so the fetch would hang. The token is cancelled the moment the
    /// connection arrives. Asserted:
    ///
    /// - the checkout returns `Cancelled` at once;
    /// - the MIRROR CONNECTION CLOSES. That socket is held by git's transport
    ///   helper (`git-remote-http`), a CHILD of `git` — so its close is the
    ///   portable, observable proof that the whole process tree was killed,
    ///   not just `git`. With `kill_on_drop` alone the helper survives and the
    ///   connection stays open until `http.lowSpeedTime` (60 s) — longer than
    ///   this test waits.
    ///
    /// Lives here, not in the executor crate, because the tree reaper is the
    /// HOST's: this pins the runner's [`RunnerProcess`] (a kill-on-close Job
    /// Object on Windows, a process group + `killpg` on Unix) through the
    /// executor's public checkout entry point.
    #[tokio::test]
    async fn cancel_during_a_hung_fetch_kills_the_whole_fetch_tree() {
        use std::time::Duration;
        use tokio::io::AsyncReadExt;
        use tokio_util::sync::CancellationToken;
        // `<tmp>/root/qontinui-runner`: an empty primary checkout, so the
        // dispatched head is not warm and the checkout must fetch it.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let primary = root.join("qontinui-runner");
        std::fs::create_dir_all(&primary).unwrap();
        let init = std::process::Command::new("git")
            .current_dir(&primary)
            .args(["init", "-q"])
            .status()
            .unwrap();
        assert!(init.success());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mirror.git", listener.local_addr().unwrap());
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
        let server = tokio::spawn(async move {
            let Ok(Ok((mut socket, _))) =
                tokio::time::timeout(Duration::from_secs(30), listener.accept()).await
            else {
                let _ = closed_tx.send(Err("git never connected to the listener".to_string()));
                return;
            };
            trigger.cancel();
            // Drain the request, then wait for the peer to go away.
            let closed = tokio::time::timeout(Duration::from_secs(20), async {
                let mut buf = [0u8; 4096];
                loop {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
            })
            .await
            .map_err(|_| "the mirror connection outlived the cancel by 20 s".to_string());
            let _ = closed_tx.send(closed);
        });
        let started = std::time::Instant::now();
        let err = qontinui_ci_exec::checkout::prepare_worktree(
            &RunnerProcess,
            &root,
            "qontinui/qontinui-runner",
            "d-1",
            &url,
            "refs/heads/merge-candidate/1",
            "0123456789abcdef0123456789abcdef01234567",
            &cancel,
            &mut |_| {},
        )
        .await
        .expect_err("cancelled");
        assert_eq!(err, qontinui_ci_exec::checkout::CheckoutError::Cancelled);
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "a hung fetch must not be waited out, took {:?}",
            started.elapsed()
        );
        // Bounded, so an environment where git never reaches the listener
        // (an HTTP proxy, say) fails this test instead of hanging it.
        let closed = tokio::time::timeout(Duration::from_secs(30), closed_rx)
            .await
            .expect("the listener never reported the connection closing")
            .expect("the listener saw the connection");
        assert_eq!(
            closed,
            Ok(()),
            "the fetch's transport helper must be killed"
        );
        server.abort();
    }
}
