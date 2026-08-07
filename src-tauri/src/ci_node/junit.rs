//! JUnit capture for a CI dispatch — the artifact that feeds coord's Tier-7
//! credibility merge gate.
//!
//! ## Why this module exists at all
//!
//! Restoring nextest to the manifest restored JUnit *emission*, but not
//! *delivery*: nextest writes the report INSIDE the dispatch worktree, which
//! [`super::checkout::cleanup_dispatch`] deletes at the end of every dispatch.
//! Emission without capture produced a file that was created and destroyed
//! without anything ever reading it, so coord's `test_run_effects::score_for_head`
//! kept returning `None` and the credibility tier kept failing CLOSED.
//!
//! ## Where the file is
//!
//! nextest writes `<workspace-root>/target/nextest/<profile>/junit.xml`. That
//! path is **not** derived from `CARGO_TARGET_DIR` (verified on this lane — the
//! executor exports `CARGO_TARGET_DIR=<root>/.ci-target/<repo>` and the report
//! still lands under the workspace's own `target/`). So the search is rooted at
//! the WORKSPACE, i.e. the worktree and each step's `working_dir` (the runner's
//! own manifest runs cargo from `src-tauri/`, coord's from the repo root).
//!
//! `CARGO_TARGET_DIR` is nevertheless searched too, last. That is not hedging
//! about the verified behaviour: it costs one `read_dir` and it means a future
//! nextest that DOES honour the variable degrades to "found in the other place"
//! rather than to a silently-missing gate input — the exact failure this module
//! exists to end.
//!
//! ## Auth posture (read before "simplifying" the transport)
//!
//! This artifact is carried on the dispatch RESULT POST
//! (`POST {coord}/coord/ci/dispatches/{id}/result`), which is device-JWT
//! authenticated and additionally matched against the dispatch row's assignee.
//! It is deliberately NOT sent to coord's `/coord/test-results/ingest`: that
//! route is gated on `COORD_INGEST_TOKEN`, a FLEET secret, and this lane runs on
//! customers' machines — distributing it would let any customer spoof any
//! tenant's merge-gate inputs. The result POST carries only `{format, raw}`;
//! coord derives repo, head sha and tenant from its own dispatch row, so a
//! runner cannot assert what its results are ABOUT.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::manifest::CiStep;

/// Maximum artifact size the runner will transmit. Mirrors coord's
/// `ci_dispatch::TEST_ARTIFACT_MAX_BYTES` exactly — coord rejects anything
/// larger with a named reason, so sending it would only burn bandwidth on a
/// guaranteed rejection.
///
/// Truncation is NOT an option: a half JUnit document is malformed XML whose
/// tail testcases vanish without trace, which is worse than an explicit,
/// logged absence.
pub(crate) const TEST_ARTIFACT_MAX_BYTES: usize = 4 * 1024 * 1024;

/// The wire token for coord's `ArtifactFormat::JunitXml`. Declared, never
/// sniffed — coord selects its parser from this, and a sniffing server would
/// silently pick the wrong parser for a document that merely resembles another
/// format.
pub(crate) const FORMAT_JUNIT_XML: &str = "junit_xml";

/// The test-results artifact as it appears on the result POST body.
///
/// Note what is absent: no repo, no head sha, no tenant, no test ids. Coord
/// attributes the results from its own dispatch row. Adding any of those fields
/// here would recreate the spoofing surface the device-JWT transport exists to
/// close.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TestArtifact {
    pub format: &'static str,
    pub raw: String,
}

/// Result of looking for a dispatch's JUnit report.
///
/// Every non-captured arm is a REASON, not a silence: a missing gate input has
/// to be attributable from the dispatch log alone, because the symptom
/// downstream (a fail-closed credibility tier) looks identical for all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CaptureOutcome {
    /// Found and read.
    Captured(TestArtifact),
    /// No report anywhere under the search roots. Legitimate: a manifest may
    /// have no test step, a step may not use nextest, or the dispatch may have
    /// short-circuited on an earlier step's failure.
    Absent,
    /// Found but over [`TEST_ARTIFACT_MAX_BYTES`].
    TooLarge { path: PathBuf, bytes: u64 },
    /// Found but unreadable (permissions, mid-write, not UTF-8).
    Unreadable { path: PathBuf, detail: String },
}

impl CaptureOutcome {
    /// The artifact to POST, if any.
    pub(crate) fn artifact(&self) -> Option<&TestArtifact> {
        match self {
            CaptureOutcome::Captured(a) => Some(a),
            _ => None,
        }
    }

    /// A single dispatch-log line describing what happened. ALWAYS emitted —
    /// including for [`CaptureOutcome::Absent`], because "no JUnit was
    /// produced" and "a JUnit was produced and lost" are different bugs and the
    /// log is the only place they are distinguishable after cleanup runs.
    pub(crate) fn log_line(&self) -> String {
        match self {
            CaptureOutcome::Captured(a) => format!(
                "[ci-node] captured test-results artifact ({}, {} bytes) — attaching to the result POST",
                a.format,
                a.raw.len()
            ),
            CaptureOutcome::Absent => "[ci-node] no JUnit report produced by this dispatch — \
                 no test_results will be ingested for this head (coord's credibility tier \
                 fails closed without one)"
                .to_string(),
            CaptureOutcome::TooLarge { path, bytes } => format!(
                "[ci-node] JUnit report {} is {bytes} bytes, over the {TEST_ARTIFACT_MAX_BYTES}-byte \
                 transport bound — NOT sent (truncating it would corrupt the document)",
                path.display()
            ),
            CaptureOutcome::Unreadable { path, detail } => format!(
                "[ci-node] JUnit report {} could not be read: {detail} — NOT sent",
                path.display()
            ),
        }
    }
}

/// The directories to search for `target/nextest/<profile>/junit.xml`.
///
/// PURE (no IO) so the derivation is unit-testable. Order is significant: the
/// workspace roots first (the verified location), the exported
/// `CARGO_TARGET_DIR` last (see the module docs).
///
/// The worktree itself is always included — the common single-crate layout —
/// plus one root per distinct step `working_dir`, because a repo whose cargo
/// workspace is a subdirectory (the runner's own `src-tauri/`) writes its
/// report under THAT directory's `target/`.
pub(crate) fn junit_search_roots(
    worktree: &Path,
    ci_target_dir: &Path,
    steps: &[CiStep],
) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = vec![worktree.join("target").join("nextest")];
    for step in steps {
        let Some(wd) = step.working_dir.as_deref() else {
            continue;
        };
        let candidate = worktree.join(wd).join("target").join("nextest");
        if !roots.contains(&candidate) {
            roots.push(candidate);
        }
    }
    let exported = ci_target_dir.join("nextest");
    if !roots.contains(&exported) {
        roots.push(exported);
    }
    roots
}

/// Pick the report to send when several exist (one per nextest profile, and
/// one per workspace on a multi-workspace repo).
///
/// PURE over `(path, mtime)` pairs so the tie-break is unit-testable. Newest
/// mtime wins — a dispatch's own run is by construction the most recent write,
/// and the `.ci-target` cache is warm across dispatches so a STALE report from
/// a previous dispatch can legitimately be sitting next to it. Ties break on
/// path order, which is [`junit_search_roots`]' order: workspace before
/// `CARGO_TARGET_DIR`.
pub(crate) fn pick_newest(candidates: &[(PathBuf, SystemTime)]) -> Option<PathBuf> {
    candidates
        .iter()
        .enumerate()
        .max_by_key(|(idx, (_, mtime))| (*mtime, std::cmp::Reverse(*idx)))
        .map(|(_, (path, _))| path.clone())
}

/// Classify a report's size against the transport bound. PURE.
pub(crate) fn size_is_transmittable(bytes: u64) -> bool {
    bytes as usize <= TEST_ARTIFACT_MAX_BYTES && bytes > 0
}

/// Enumerate `junit.xml` files one level under each search root
/// (`<root>/<profile>/junit.xml`), paired with their mtimes.
///
/// Never fails: an unreadable/absent root contributes nothing. `Vec` rather
/// than `Result` because "we could not look" and "there was nothing there" have
/// the same downstream handling — the dispatch is not failed either way.
fn enumerate_reports(roots: &[PathBuf]) -> Vec<(PathBuf, SystemTime)> {
    let mut found = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let candidate = entry.path().join("junit.xml");
            let Ok(meta) = std::fs::metadata(&candidate) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            found.push((candidate, meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)));
        }
    }
    found
}

/// Capture this dispatch's JUnit report.
///
/// **Must run before [`super::checkout::cleanup_dispatch`]** — the report lives
/// inside the tree cleanup deletes. That ordering is not left to convention:
/// the captured value is a required argument of
/// [`super::reporting::post_result`], which in turn mints the
/// [`super::reporting::ResultReported`] receipt that
/// `DispatchWorkspace::cleanup` demands. See `executor.rs`.
///
/// Never fails the dispatch. A green build with no report stays green; the
/// reason is on the dispatch log via [`CaptureOutcome::log_line`].
pub(crate) fn capture(worktree: &Path, ci_target_dir: &Path, steps: &[CiStep]) -> CaptureOutcome {
    let roots = junit_search_roots(worktree, ci_target_dir, steps);
    let reports = enumerate_reports(&roots);
    let Some(path) = pick_newest(&reports) else {
        return CaptureOutcome::Absent;
    };
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if bytes == 0 {
        return CaptureOutcome::Absent;
    }
    if !size_is_transmittable(bytes) {
        return CaptureOutcome::TooLarge { path, bytes };
    }
    match std::fs::read_to_string(&path) {
        Ok(raw) if raw.trim().is_empty() => CaptureOutcome::Absent,
        Ok(raw) => CaptureOutcome::Captured(TestArtifact {
            format: FORMAT_JUNIT_XML,
            raw,
        }),
        Err(e) => CaptureOutcome::Unreadable {
            path,
            detail: e.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn step(name: &str, working_dir: Option<&str>) -> CiStep {
        CiStep {
            name: name.to_string(),
            command: vec!["cargo".to_string(), "nextest".to_string()],
            working_dir: working_dir.map(|s| s.to_string()),
            env: Default::default(),
            timeout_secs: None,
        }
    }

    /// The workspace roots come FIRST and `CARGO_TARGET_DIR` last — nextest
    /// writes under the workspace, and the exported target dir is only the
    /// degrade-gracefully fallback.
    #[test]
    fn search_roots_prefer_the_workspace_over_cargo_target_dir() {
        let wt = Path::new("/ci/wt/qontinui-coord");
        let target = Path::new("/ci/.ci-target/qontinui-coord");
        let roots = junit_search_roots(wt, target, &[step("rust-test", None)]);
        assert_eq!(roots.first().unwrap(), &wt.join("target").join("nextest"));
        assert_eq!(roots.last().unwrap(), &target.join("nextest"));
    }

    /// A repo whose cargo workspace is a subdirectory (the runner's own
    /// `src-tauri/`) writes its report under THAT directory's `target/`.
    #[test]
    fn search_roots_include_each_step_working_dir() {
        let wt = Path::new("/ci/wt/qontinui-runner");
        let target = Path::new("/ci/.ci-target/qontinui-runner");
        let roots = junit_search_roots(
            wt,
            target,
            &[
                step("rust-clippy", Some("src-tauri")),
                step("rust-test", Some("src-tauri")),
                step("web-test", None),
            ],
        );
        assert!(roots.contains(&wt.join("src-tauri").join("target").join("nextest")));
        // The duplicate `src-tauri` step does not duplicate the root.
        assert_eq!(
            roots
                .iter()
                .filter(|r| *r == &wt.join("src-tauri").join("target").join("nextest"))
                .count(),
            1
        );
    }

    /// The `.ci-target` cache is warm across dispatches, so a stale report can
    /// sit beside this dispatch's own. Newest mtime wins.
    #[test]
    fn newest_report_wins_over_a_stale_warm_cache_copy() {
        let old = (
            PathBuf::from("/ci/.ci-target/x/nextest/ci/junit.xml"),
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_000),
        );
        let fresh = (
            PathBuf::from("/ci/wt/x/target/nextest/ci-pr/junit.xml"),
            SystemTime::UNIX_EPOCH + Duration::from_secs(2_000),
        );
        assert_eq!(pick_newest(&[old.clone(), fresh.clone()]), Some(fresh.0));
        assert_eq!(pick_newest(&[]), None);
    }

    /// Equal mtimes break toward the EARLIER search root (the workspace).
    #[test]
    fn mtime_ties_break_toward_the_workspace_root() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(5);
        let workspace = PathBuf::from("/ci/wt/x/target/nextest/ci/junit.xml");
        let exported = PathBuf::from("/ci/.ci-target/x/nextest/ci/junit.xml");
        assert_eq!(
            pick_newest(&[(workspace.clone(), t), (exported, t)]),
            Some(workspace)
        );
    }

    /// The transport bound matches coord's and rejects rather than truncates.
    #[test]
    fn transport_bound_matches_coord_and_rejects_empty_and_oversize() {
        assert_eq!(TEST_ARTIFACT_MAX_BYTES, 4 * 1024 * 1024);
        assert!(size_is_transmittable(1));
        assert!(size_is_transmittable(TEST_ARTIFACT_MAX_BYTES as u64));
        assert!(!size_is_transmittable(TEST_ARTIFACT_MAX_BYTES as u64 + 1));
        assert!(!size_is_transmittable(0));
    }

    /// Every non-captured arm produces an explicit, distinguishable log line —
    /// after cleanup runs, the log is the ONLY evidence of which arm fired.
    #[test]
    fn every_outcome_is_loud_on_the_dispatch_log() {
        let captured = CaptureOutcome::Captured(TestArtifact {
            format: FORMAT_JUNIT_XML,
            raw: "<testsuites/>".to_string(),
        });
        assert!(captured.artifact().is_some());
        assert!(captured.log_line().contains("captured"));

        for outcome in [
            CaptureOutcome::Absent,
            CaptureOutcome::TooLarge {
                path: PathBuf::from("/x/junit.xml"),
                bytes: 99_999_999,
            },
            CaptureOutcome::Unreadable {
                path: PathBuf::from("/x/junit.xml"),
                detail: "permission denied".to_string(),
            },
        ] {
            assert!(outcome.artifact().is_none());
            let line = outcome.log_line();
            assert!(line.starts_with("[ci-node]"), "{line}");
            assert!(!line.is_empty());
        }
        // The three reasons are genuinely distinct strings.
        assert_ne!(
            CaptureOutcome::Absent.log_line(),
            CaptureOutcome::TooLarge {
                path: PathBuf::from("/x/junit.xml"),
                bytes: 1,
            }
            .log_line()
        );
    }

    /// The wire shape coord parses: `{format, raw}` and nothing else. A repo /
    /// head_sha / tenant key here would recreate the spoofing surface the
    /// device-JWT transport closes.
    #[test]
    fn artifact_wire_shape_carries_no_attribution() {
        let a = TestArtifact {
            format: FORMAT_JUNIT_XML,
            raw: "<testsuites/>".to_string(),
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"format": "junit_xml", "raw": "<testsuites/>"})
        );
        let obj = v.as_object().unwrap();
        assert_eq!(obj.len(), 2);
        for forbidden in ["repo", "head_sha", "tenant_id", "source", "results"] {
            assert!(
                !obj.contains_key(forbidden),
                "the runner must not be able to assert {forbidden}"
            );
        }
    }

    /// End-to-end over a real temp tree: a report under the workspace is found,
    /// read, and returned; an empty search space is `Absent`, not an error.
    #[test]
    fn capture_finds_a_workspace_report_and_tolerates_none() {
        let base = std::env::temp_dir().join(format!("ci-junit-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let wt = base.join("wt");
        let target = base.join("ci-target");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::create_dir_all(&target).unwrap();

        // Nothing anywhere ⇒ Absent (a legitimate, non-fatal state).
        assert_eq!(capture(&wt, &target, &[]), CaptureOutcome::Absent);

        let profile_dir = wt.join("src-tauri").join("target").join("nextest").join("ci");
        std::fs::create_dir_all(&profile_dir).unwrap();
        std::fs::write(
            profile_dir.join("junit.xml"),
            "<testsuites><testcase name=\"t\"/></testsuites>",
        )
        .unwrap();

        // Not found without the step's working_dir (the root isn't searched)…
        assert_eq!(capture(&wt, &target, &[]), CaptureOutcome::Absent);
        // …and found once the manifest declares it.
        let outcome = capture(&wt, &target, &[step("rust-test", Some("src-tauri"))]);
        match &outcome {
            CaptureOutcome::Captured(a) => {
                assert_eq!(a.format, FORMAT_JUNIT_XML);
                assert!(a.raw.contains("<testcase"));
            }
            other => panic!("expected Captured, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&base);
    }
}
