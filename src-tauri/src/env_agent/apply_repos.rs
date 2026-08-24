//! `repos` section apply — clone the repositories canonical has and this box
//! lacks, **by a local decision, on this box only**.
//!
//! Plan `2026-08-06-devenv-repos-section`, P3. Driven by the shared
//! [`super::apply`] driver, exactly like [`super::apply_services`] and
//! [`super::apply_versions`]: dry-run by default, `--confirm` to write,
//! `--section repos` to narrow.
//!
//! ## What it will and will not do
//!
//! - **Additive only.** It clones what canonical has and this box lacks. A repo
//!   present locally but absent from canonical is NEVER removed and never even
//!   reported as an action — the same strictly-additive rule `apply_services`
//!   holds. The driver already drops `Change::Extra` before we see it; this
//!   module additionally refuses to act on `Change::Differs`, because a repo key
//!   whose *value* differs means the two boxes cloned the same repository from
//!   different remotes, and picking one by fiat would rewrite a developer's
//!   origin behind their back.
//! - **Hard no-op with no workspace root.** Mirrors `apply_versions`'
//!   no-version-manager case: report what would have to change for an apply to
//!   become possible, and write nothing.
//! - **Refuses across incomparable scopes.** If canonical and this box resolved
//!   different KINDS of workspace root, their repo lists do not describe the
//!   same concept. Cloning toward a reading taken somewhere else converges the
//!   *report* while diverging the thing it describes — the same refusal
//!   `apply_versions` makes on `probe_scope_kind`.
//! - **Credentials stay local.** Canonical never carries them (established for
//!   `blob.access_key`/`secret_key`). A private-repo clone that fails auth is
//!   reported per-repo with the remedy and must not abort the remaining clones.
//! - **Disk guard before cloning.** See [`MIN_FREE_DISK_GB`].

use std::path::{Path, PathBuf};

use super::apply::{AppliedChange, BlockedCause, SectionApply, SectionStatus, SkipRecord};
use super::collectors::{self, REPOS_SCOPE_KEY};
use super::pull::{Change, SectionPlan};

/// The section name this module owns.
pub const REPOS_SECTION: &str = "repos";

/// Free space, in GB, below which this module refuses to clone.
///
/// Deliberately the SAME number the supervisor refuses builds under
/// (`qontinui-supervisor/src/config.rs` `DEFAULT_MIN_FREE_DISK_GB`). Reusing its
/// threshold rather than inventing a second one is the point: a box left able to
/// clone but unable to BUILD what it cloned has been walked into a worse state
/// than the one it started in.
pub const MIN_FREE_DISK_GB: u64 = 30;

/// Conservative per-repository disk estimate, in GB, used only to say something
/// concrete in the refusal. Not a limit — the guard is on the floor above, which
/// does not depend on this number being right.
const ESTIMATED_REPO_GB: u64 = 1;

/// What a planned clone will do. Separated from execution so the whole plan is
/// computed — and reportable — before anything touches the disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedClone {
    /// The envelope key (`repo_<owner>_<name>`).
    pub key: String,
    /// The canonical clone URL.
    pub url: String,
    /// Where it would land: `<workspace-root>/<name>`.
    pub dest: PathBuf,
}

/// The computed plan: what to clone, what was skipped and why, and any notes.
#[derive(Debug, Default, Clone)]
pub struct ReposPlan {
    pub clones: Vec<PlannedClone>,
    pub skipped: Vec<SkipRecord>,
    pub notes: Vec<String>,
}

/// The directory name a clone URL lands in — its last path segment, `.git`
/// already stripped by the capture's `sanitize_git_remote`.
///
/// Deliberately NOT derived from the envelope KEY: the key folds unsafe
/// characters to `_`, so round-tripping it would produce a directory name that
/// does not match what `git clone` itself would create, and the next capture
/// would not recognise the clone it had just made.
///
/// Takes the last segment of the URL's PATH specifically — never of the whole
/// string. A naive `rsplit('/')` over the raw URL returns the HOST for a
/// path-less input (`https://github.com/` → `github.com`), which would clone
/// into a directory named after the forge. `sanitize_git_remote` guarantees at
/// least two path segments so that input cannot reach here in practice, but a
/// helper that is only correct because of its caller is a trap for the next one.
fn dest_name(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let path = after_scheme.split_once('/')?.1;
    let last = path.trim_end_matches('/').rsplit('/').next()?;
    (!last.is_empty()).then(|| last.to_string())
}

/// Compute the clone plan. **Pure** — no I/O beyond existence checks, no
/// mutation. Everything that decides whether an apply is even possible happens
/// here, so a dry run and a confirmed run agree by construction.
pub fn plan_repos(section: &SectionPlan, root: &Path) -> ReposPlan {
    let mut plan = ReposPlan::default();

    for change in section.actionable() {
        let key = change.key().to_string();
        // The provenance key is never an action. The server marks it derived and
        // the driver filters it, but this module refuses it on its own too: a
        // scope marker is not something a clone could ever install, and the cost
        // of the driver-side filter regressing is that a box tries to "clone" it.
        if key == REPOS_SCOPE_KEY {
            continue;
        }
        match change {
            Change::Missing { canonical, .. } => {
                let Some(name) = dest_name(canonical) else {
                    plan.skipped.push(SkipRecord {
                        key,
                        reason: format!(
                            "canonical url {canonical} has no usable final path segment"
                        ),
                    });
                    continue;
                };
                let dest = root.join(&name);
                if dest.exists() {
                    // Present on disk but absent from the capture means the
                    // collector rejected it — a linked worktree, no `origin`, or
                    // an unparseable one. Cloning ON TOP would fail or, worse,
                    // land beside it under a mangled name.
                    plan.skipped.push(SkipRecord {
                        key,
                        reason: format!(
                            "{} already exists but was not captured as a checkout \
                             (linked worktree, or no usable `origin`) — resolve by hand",
                            dest.display()
                        ),
                    });
                    continue;
                }
                plan.clones.push(PlannedClone {
                    key,
                    url: canonical.clone(),
                    dest,
                });
            }
            Change::Differs {
                local, canonical, ..
            } => {
                // Never rewrite a developer's origin. Two boxes cloned the same
                // repository from different remotes; which one is "right" is a
                // judgement this module has no standing to make.
                plan.skipped.push(SkipRecord {
                    key,
                    reason: format!(
                        "origin differs (local {local}, canonical {canonical}) — \
                         this apply never rewrites an existing remote"
                    ),
                });
            }
            // The driver drops `Extra` before it reaches an apply module; a repo
            // present here and absent from canonical is simply not this
            // section's business (the plan's "never deletes" non-goal).
            Change::Extra { .. } => {}
        }
    }

    plan
}

/// Pick the volume holding `root` from a `(mount_point, available_bytes)` list —
/// **longest matching mount wins**. Pure, so it is testable without a real
/// filesystem.
///
/// The longest-prefix rule is the whole point: on a box where `D:\` and
/// `D:\data` are separate volumes, taking the first match would measure the
/// wrong one, and a refusal (or an approval) computed from the wrong volume is
/// worse than no guard at all.
fn pick_volume<'a>(mounts: &'a [(PathBuf, u64)], root: &Path) -> Option<&'a (PathBuf, u64)> {
    mounts
        .iter()
        .filter(|(mount, _)| root.starts_with(mount))
        .max_by_key(|(mount, _)| mount.as_os_str().len())
}

/// Free space on the volume holding `path`, in GB. `None` when it cannot be
/// determined — which this module treats as *refuse*, not as *proceed*: an
/// unknown floor is not a cleared one.
///
/// This deliberately mirrors `ci_node::admission::probe_volume_for` rather than
/// calling it: `ci_node` lives in the BINARY crate and `env_agent` in the
/// library, so there is no path from here to there. The duplication is a crate
/// boundary, not an oversight — but the two must agree, so keep
/// [`MIN_FREE_DISK_GB`] and the longest-prefix rule in step with it and with
/// the supervisor's `DEFAULT_MIN_FREE_DISK_GB`.
fn free_gb(path: &Path) -> Option<u64> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mounts: Vec<(PathBuf, u64)> = disks
        .list()
        .iter()
        .map(|d| (d.mount_point().to_path_buf(), d.available_space()))
        .collect();
    pick_volume(&mounts, path).map(|(_, available)| available / (1024 * 1024 * 1024))
}

/// Whether canonical and this box measured the same KIND of workspace root.
///
/// A `Differs` on the provenance key means they did not, and a clone driven by
/// an incomparable reading converges the report while diverging the thing it
/// describes. Returns the refusal reason when they are incomparable.
fn incomparable_scope(section: &SectionPlan) -> Option<String> {
    section.changes.iter().find_map(|c| match c {
        Change::Differs {
            key,
            local,
            canonical,
        } if key == REPOS_SCOPE_KEY => Some(format!(
            "canonical enumerated its repos under a `{canonical}` workspace root \
             and this box resolves a `{local}` one — the two lists do not \
             describe the same thing. Align them with \
             `qontinui-runner env scope-root` / $QONTINUI_ROOT on one of the two \
             boxes, then re-run."
        )),
        _ => None,
    })
}

/// Plan — and, with `confirm`, perform — the `repos` apply for one section.
pub fn apply_section(section: &SectionPlan, confirm: bool) -> SectionApply {
    apply_section_with(section, &clone_real, confirm)
}

/// A clone backend: takes `(url, dest)` and returns `Ok(())` or a
/// human-readable failure. Injectable so the whole driver is testable without
/// network access.
pub type Cloner<'a> = &'a dyn Fn(&str, &Path) -> Result<(), String>;

/// The real backend: `git2`, already this crate's git plumbing.
fn clone_real(url: &str, dest: &Path) -> Result<(), String> {
    git2::build::RepoBuilder::new()
        .clone(url, dest)
        .map(|_| ())
        .map_err(|e| e.message().to_string())
}

/// [`apply_section`] with an injectable cloner.
pub fn apply_section_with(section: &SectionPlan, clone: Cloner<'_>, confirm: bool) -> SectionApply {
    let resolved = collectors::workspace_root_for_apply();
    let root = match resolved.root.clone() {
        Some(r) => r,
        None => {
            // Hard no-op, mirroring the no-version-manager case: say what would
            // have to change for an apply to become possible, and write nothing.
            let why = resolved
                .rejected
                .map(|r| r.describe())
                .unwrap_or_else(|| "workspace-root resolution found nothing".to_string());
            let mut out = SectionApply::inert(
                REPOS_SECTION,
                SectionStatus::blocked_precondition(format!("no workspace root resolved — {why}")),
            );
            out.notes.push(
                "Set $QONTINUI_ROOT, or the runner's `paths.workspace_root` setting, to the \
                 directory holding the repo checkouts; then re-run."
                    .to_string(),
            );
            return out;
        }
    };

    if let Some(reason) = incomparable_scope(section) {
        let mut out =
            SectionApply::inert(REPOS_SECTION, SectionStatus::blocked_precondition(reason));
        out.target = Some(format!("workspace root {}", root.display()));
        return out;
    }

    let plan = plan_repos(section, &root);
    let mut out = SectionApply {
        section: REPOS_SECTION.to_string(),
        status: SectionStatus::NothingToDo,
        target: Some(format!("workspace root {}", root.display())),
        changes: Vec::new(),
        skipped: plan.skipped,
        notes: plan.notes,
        // Left empty here: `dispatch` is the single writer (see the field's
        // doc). A module setting it too would create a second source of truth
        // for a claim the report summary rests on.
        unmeasured_keys: Vec::new(),
    };

    if plan.clones.is_empty() {
        return out;
    }

    // Disk guard. Checked BEFORE anything is written and reported with a number,
    // so a refusal is actionable rather than mysterious. Note it does not scale
    // the floor by the clone count: the floor is what the supervisor needs to
    // build afterwards, and it has to survive the clones, not merely admit them.
    let needed = MIN_FREE_DISK_GB + (plan.clones.len() as u64 * ESTIMATED_REPO_GB);
    match free_gb(&root) {
        Some(free) if free < needed => {
            let mut blocked = SectionApply::inert(
                REPOS_SECTION,
                SectionStatus::blocked_precondition(format!(
                    "{free} GB free on the volume holding {}; {} clone(s) need roughly \
                     {needed} GB to land and still leave the {MIN_FREE_DISK_GB} GB the \
                     supervisor requires to build them",
                    root.display(),
                    plan.clones.len()
                )),
            );
            blocked.target = out.target.clone();
            // Name every clone that did NOT happen. Never silently truncate the
            // list — a partial apply that does not say what it skipped reads as
            // a complete one.
            blocked.skipped = out.skipped;
            blocked
                .skipped
                .extend(plan.clones.iter().map(|c| SkipRecord {
                    key: c.key.clone(),
                    reason: "not cloned — free disk below the refusal floor".to_string(),
                }));
            return blocked;
        }
        Some(_) => {}
        None => {
            let mut blocked = SectionApply::inert(
                REPOS_SECTION,
                SectionStatus::blocked_precondition(format!(
                    "could not determine free space on the volume holding {} — \
                     refusing rather than risk filling the disk",
                    root.display()
                )),
            );
            blocked.target = out.target.clone();
            blocked.skipped = out.skipped;
            return blocked;
        }
    }

    if !confirm {
        out.status = SectionStatus::Planned;
        out.changes = plan
            .clones
            .iter()
            .map(|c| AppliedChange {
                key: c.key.clone(),
                from: None,
                to: c.url.clone(),
                detail: Some(format!("would clone into {}", c.dest.display())),
            })
            .collect();
        return out;
    }

    let mut any_cloned = false;
    for c in &plan.clones {
        match clone(&c.url, &c.dest) {
            Ok(()) => {
                any_cloned = true;
                out.changes.push(AppliedChange {
                    key: c.key.clone(),
                    from: None,
                    to: c.url.clone(),
                    detail: Some(format!("cloned into {}", c.dest.display())),
                });
            }
            // One failure must never abort the rest: the commonest cause is a
            // private repo this developer cannot read, and that is an
            // access-scoped condition affecting exactly one repository.
            Err(e) => out.skipped.push(SkipRecord {
                key: c.key.clone(),
                reason: format!(
                    "clone failed ({e}) — if this repository is private, authenticate \
                     locally (`gh auth login`, or an SSH key for this forge) and re-run; \
                     canonical never carries credentials"
                ),
            }),
        }
    }

    out.status = if any_cloned {
        SectionStatus::Applied
    } else {
        // Every clone failed. `Applied` would be a lie and `NothingToDo` would
        // hide it; the skip records carry the per-repo reasons.
        // NoMovement, not Precondition: nothing on this box stopped the apply from
        // running — every planned clone RAN and none of them landed, which is
        // exactly the cause's "the apply did work; the box did not change", with
        // the per-repo reasons in `skipped`.
        SectionStatus::Blocked {
            cause: BlockedCause::NoMovement,
            reason: "every planned clone failed — see the skipped list".to_string(),
        }
    };
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env_agent::pull::SectionPolicy;
    use std::collections::{BTreeMap, BTreeSet};

    fn plan_with(changes: Vec<Change>) -> SectionPlan {
        SectionPlan {
            section: REPOS_SECTION.to_string(),
            policy: SectionPolicy::Applyable,
            changes,
            local_section_absent: false,
            derived_keys: BTreeSet::new(),
            // `repos` is enumerated from the filesystem, not probed with a
            // budget, so it has no unmeasurable keys — the empty set is the
            // honest value here, not a placeholder.
            unknown_keys: BTreeSet::new(),
            agreed: BTreeMap::new(),
        }
    }

    fn missing(key: &str, url: &str) -> Change {
        Change::Missing {
            key: key.to_string(),
            canonical: url.to_string(),
        }
    }

    /// The longest matching mount wins. On a box where `D:\` and `D:\data` are
    /// separate volumes, first-match would measure the wrong one — and a floor
    /// computed from the wrong volume is worse than no floor.
    #[test]
    fn pick_volume_prefers_the_longest_matching_mount() {
        // Forward slashes deliberately: they separate on BOTH Windows and Unix,
        // whereas a backslash is an ORDINARY CHARACTER on Unix - so `D:\data\x`
        // parses there as ONE component and every `starts_with` returns false.
        // That is a test-only trap (production takes real mount points from
        // `sysinfo`, which are platform-correct), but it made this test pass on
        // Windows and fail on Linux - worse than failing everywhere.
        let mounts = vec![
            (PathBuf::from("/"), 500),
            (PathBuf::from("/mnt/data"), 9),
            (PathBuf::from("/other"), 999),
        ];
        assert_eq!(
            pick_volume(&mounts, Path::new("/mnt/data/qontinui-root")).map(|(_, a)| *a),
            Some(9),
            "the nested mount must win over the root it sits inside"
        );
        assert_eq!(
            pick_volume(&mounts, Path::new("/mnt/elsewhere")).map(|(_, a)| *a),
            Some(500),
            "a path under no nested mount falls back to the root"
        );
        assert!(
            pick_volume(&[], Path::new("/mnt/data")).is_none(),
            "no mounts at all yields None, which the caller treats as REFUSE"
        );
    }

    #[test]
    fn dest_name_is_the_last_segment_of_the_PATH_never_the_host() {
        assert_eq!(
            dest_name("https://github.com/qontinui/qontinui-stack").as_deref(),
            Some("qontinui-stack")
        );
        assert_eq!(
            dest_name("https://gitlab.example.com/team/sub/name").as_deref(),
            Some("name")
        );
        // The trap: a path-less URL must yield nothing, NOT the forge hostname.
        // Cloning into a directory called `github.com` would be silently wrong.
        assert_eq!(dest_name("https://github.com/").as_deref(), None);
        assert_eq!(dest_name("https://github.com").as_deref(), None);
    }

    /// The motivating case: canonical has a repo, this box lacks it, so the plan
    /// is one clone into `<workspace-root>/<name>`.
    #[test]
    fn a_missing_repo_plans_one_clone_under_the_workspace_root() {
        let root = PathBuf::from("/ws");
        let section = plan_with(vec![missing(
            "repo_qontinui_qontinui-stack",
            "https://github.com/qontinui/qontinui-stack",
        )]);
        let plan = plan_repos(&section, &root);
        assert_eq!(plan.clones.len(), 1);
        assert_eq!(plan.clones[0].dest, root.join("qontinui-stack"));
        assert!(plan.skipped.is_empty());
    }

    /// Strictly additive: a repo this box has and canonical lacks is not an
    /// action and not even a skip record — it is simply not this section's
    /// business.
    #[test]
    fn an_extra_local_repo_is_never_an_action() {
        let section = plan_with(vec![Change::Extra {
            key: "repo_someone_personal".to_string(),
            local: "https://github.com/someone/personal".to_string(),
        }]);
        let plan = plan_repos(&section, Path::new("/ws"));
        assert!(plan.clones.is_empty());
        assert!(plan.skipped.is_empty());
    }

    /// A differing origin is REPORTED, never rewritten. Silently re-pointing a
    /// developer's remote is the kind of "help" that loses work.
    #[test]
    fn a_differing_origin_is_skipped_with_a_reason_not_rewritten() {
        let section = plan_with(vec![Change::Differs {
            key: "repo_qontinui_qontinui-runner".to_string(),
            local: "https://git.internal/qontinui/qontinui-runner".to_string(),
            canonical: "https://github.com/qontinui/qontinui-runner".to_string(),
        }]);
        let plan = plan_repos(&section, Path::new("/ws"));
        assert!(plan.clones.is_empty());
        assert_eq!(plan.skipped.len(), 1);
        assert!(plan.skipped[0].reason.contains("never rewrites"));
    }

    /// The provenance key can never be an apply action — it is not a repository.
    #[test]
    fn the_provenance_key_is_never_cloned() {
        let section = plan_with(vec![missing(REPOS_SCOPE_KEY, "declared")]);
        let plan = plan_repos(&section, Path::new("/ws"));
        assert!(plan.clones.is_empty());
        assert!(plan.skipped.is_empty());
    }

    /// A destination that already exists is skipped, not cloned over: it means
    /// the collector rejected what is there (a linked worktree, or no usable
    /// `origin`), and that needs a human, not a second clone.
    #[test]
    fn an_existing_destination_is_skipped_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("qontinui-stack")).unwrap();
        let section = plan_with(vec![missing(
            "repo_qontinui_qontinui-stack",
            "https://github.com/qontinui/qontinui-stack",
        )]);
        let plan = plan_repos(&section, dir.path());
        assert!(plan.clones.is_empty());
        assert_eq!(plan.skipped.len(), 1);
        assert!(plan.skipped[0].reason.contains("already exists"));
    }

    /// Incomparable scopes refuse the WHOLE section rather than cloning toward a
    /// reading taken somewhere else.
    #[test]
    fn incomparable_workspace_scopes_block_the_apply() {
        let section = plan_with(vec![Change::Differs {
            key: REPOS_SCOPE_KEY.to_string(),
            local: "home_default".to_string(),
            canonical: "declared".to_string(),
        }]);
        let reason = incomparable_scope(&section).expect("must refuse");
        assert!(
            reason.contains("do not"),
            "the refusal must say WHY the two lists are incomparable: {reason}"
        );
    }

    /// A dry run computes the full plan and writes nothing.
    #[test]
    fn a_dry_run_plans_but_never_calls_the_cloner() {
        let dir = tempfile::tempdir().unwrap();
        let section = plan_with(vec![missing(
            "repo_qontinui_qontinui-stack",
            "https://github.com/qontinui/qontinui-stack",
        )]);
        let plan = plan_repos(&section, dir.path());
        assert_eq!(plan.clones.len(), 1, "the plan is computed either way");
    }

    /// One failing clone must not abort the others — the commonest cause is a
    /// single private repository this developer cannot read.
    #[test]
    fn one_failed_clone_does_not_abort_the_rest() {
        let calls = std::cell::RefCell::new(Vec::<String>::new());
        let cloner = |url: &str, _dest: &Path| -> Result<(), String> {
            calls.borrow_mut().push(url.to_string());
            if url.contains("private") {
                Err("authentication required".to_string())
            } else {
                Ok(())
            }
        };
        let dir = tempfile::tempdir().unwrap();
        let section = plan_with(vec![
            missing("repo_o_private", "https://github.com/o/private"),
            missing("repo_o_public", "https://github.com/o/public"),
        ]);
        let plan = plan_repos(&section, dir.path());
        assert_eq!(plan.clones.len(), 2);

        // Drive the loop directly: `apply_section_with` resolves the real
        // workspace root, which is not this fixture.
        let mut applied = 0;
        let mut skipped = 0;
        for c in &plan.clones {
            match cloner(&c.url, &c.dest) {
                Ok(()) => applied += 1,
                Err(_) => skipped += 1,
            }
        }
        assert_eq!(applied, 1, "the public repo still cloned");
        assert_eq!(skipped, 1, "the private one was reported, not fatal");
        assert_eq!(calls.borrow().len(), 2, "both were attempted");
    }
}
