//! Custody (K) over git outputs, and G7's predicate: "would closing this session
//! LOSE anything?" — a port of the TREE rules of ccfg's
//! `scripts/session-closeable.sh` (plan
//! `2026-10-18-a-finished-session-is-safe-to-close-when-nothing-lives-only-in-it-
//! and-coord-owns-whether-its-watchers-work`, Phase 1), coordinated with its own
//! lane, never re-landed here. The claims ledger and the owed-item reads of that
//! script are G4 and G8's inputs, not this module's.
//!
//! The kernel does no git: the runner runs the probes and hands the outputs in
//! ([`TreeProbe`]). What this module owns is the judgement:
//!
//! - Definite reasons (`uncommitted`, `unpushed`) come ONLY from the direct git
//!   probes. The WIP custody record is a cross-check that must POSITIVELY read
//!   `clean` / `nothing_to_stash`; any other word (`captured`, `deferred`,
//!   `disabled`, ...) is UNKNOWN, never a definite reason — the Stop hook writes
//!   the slot at the END of a turn, so a session that commits, pushes and
//!   finishes in one turn still carries a stale word.
//! - A primary checkout mixes peers' state: UNKNOWN `shared_checkout`.
//! - A tree that is gone is `Absent`, which is not `Clean` (T14, P-3).
//! - Any definite reason makes the tree [`Custody::Dirty`], listing every reason
//!   (C18); else any UNKNOWN reason makes it [`Custody::Unknown`]; else `Clean`.

use crate::state::{Custody, CustodyAxis, Unk};

/// A probe that may have failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe<T> {
    Ok(T),
    /// The probe could not be made or its output not trusted (`probe_failed`).
    Failed(String),
}

/// The tree's own custody slot (`$GIT_DIR/qontinui-custody.d/<id>.json`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotRead {
    Missing,
    Unreadable,
    /// A slot whose `session_id` is another session's.
    OtherSession,
    Record {
        wip_state: String,
    },
}

/// `git status --porcelain --untracked-files=normal` and its stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusProbe {
    /// Tracked changes (any `M`, `A`, `D`, `R`, `U`... entry).
    pub tracked_changes: Vec<String>,
    /// Untracked paths outside the served-corpus roots.
    pub untracked: Vec<String>,
    /// Untracked files under `.claude/{commands,skills,agents}`, each judged by
    /// the served-corpus residue rule.
    pub served_corpus: Vec<(String, ResidueVerdict)>,
    /// The listing was non-empty (so an empty verdict means unjudged).
    pub listing_nonempty: bool,
    /// git wrote stderr (a warning on a directory it could not open): the
    /// listing may be incomplete.
    pub stderr_nonempty: bool,
}

/// The served-corpus residue rule's verdict for one untracked file (plan
/// `2026-11-03-the-runners-served-claude-corpus-makes-every-no-op-closeout-not-
/// closeable`, Phase 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResidueVerdict {
    /// These exact bytes are the runner's own and exist outside this tree.
    Residue,
    /// Not the runner's bytes: counts as uncommitted.
    Definite(String),
    /// Cannot be checked (`served_corpus_unverifiable`).
    Unknown(String),
}

/// The branch's upstream, for the unpushed rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamProbe {
    /// No upstream configured.
    None,
    /// A LOCAL upstream (`--track main`) proves nothing about a remote.
    Local,
    /// A remote-tracking upstream that resolves; `ahead` = `<upstream>..HEAD`.
    Remote { ahead: Probe<u32> },
    /// Configured, but its remote branch is gone (pushed-then-deleted: the
    /// signature of a land or a prune).
    Gone,
}

/// Whether every commit missing from every remote is patch-equal to one on a
/// remote's default branch (`git cherry`: a rebase land whose source was pruned).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchLanded {
    AllLanded,
    NotAll,
    /// No remote at all: nothing could have landed.
    NoRemotes,
    /// Remotes, but none with a default branch / the probe failed.
    ProbeFailed(String),
}

/// One dirty path, for attribution in a tree several sessions hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtyPath {
    pub path: String,
    pub tracked: bool,
    /// lstat mtime, epoch seconds.
    pub mtime: Option<u64>,
    /// The latest time P's current state could have been produced (its mtime /
    /// ctime, its directories', the index and HEAD-move floors): an UPPER bound
    /// erring late, sound only for "it predates S". `None` = no time (never 0).
    pub produced_by: Option<u64>,
}

/// One session's custody slot, as seen from another session's judgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerSlot {
    pub session_id: String,
    pub wip_state: String,
    pub snapshot_resolves: bool,
    pub first_seen: u64,
    pub last_seen_epoch: u64,
    pub wip_captured_epoch: Option<u64>,
    /// Tracked paths dirty at that session's last stop.
    pub dirty_tracked_at_stop: Vec<String>,
    /// The session's start (the earliest transcript timestamp); `None` = no bound.
    pub started: Option<u64>,
    /// The slot could not be read.
    pub unreadable: bool,
}

/// The slack on rule (b): `QONTINUI_CUSTODY_MIN_INTERVAL_SEC` (60) + 5 s.
pub const SLACK_SECS: u64 = 65;
/// Rule (c)'s margin before a session's start.
pub const SESSION_START_MARGIN_SECS: u64 = 300;

/// Who a dirty path is attributed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    /// No peer remains a candidate: definite `uncommitted`.
    Own,
    /// This session is excluded and a peer is not: UNKNOWN.
    Peer,
    /// Neither is excluded: UNKNOWN.
    Ambiguous,
    /// Any doubt about the attribution itself: UNKNOWN `attribution_unreadable`.
    Unreadable,
}

/// Whether the slot `x` is excluded from path `p` by rule (a), (b) or (c).
fn excluded(x: &PeerSlot, p: &DirtyPath, others: &[&PeerSlot], windows_ok: bool) -> bool {
    // (c) any P: produced before X's start minus the margin.
    if let (Some(prod), Some(start)) = (p.produced_by, x.started) {
        if prod < start.saturating_sub(SESSION_START_MARGIN_SECS) {
            return true;
        }
    }
    if !windows_ok {
        return false;
    }
    // (a) a TRACKED P: X's record is current at its stop and P was not dirty then.
    if p.tracked {
        let current = match x.wip_state.as_str() {
            "clean" => true,
            "captured" => x.snapshot_resolves,
            "unchanged" => {
                // The shared per-gitdir stamp may have been rewritten by a peer
                // whose [first_seen, last_seen] window overlaps
                // (wip_captured_epoch, last_seen_epoch].
                let from = x.wip_captured_epoch.unwrap_or(x.last_seen_epoch);
                x.snapshot_resolves
                    && !others.iter().any(|o| {
                        o.session_id != x.session_id
                            && o.first_seen <= x.last_seen_epoch
                            && o.last_seen_epoch > from
                    })
            }
            _ => false,
        };
        return current && !x.dirty_tracked_at_stop.iter().any(|d| d == &p.path);
    }
    // (b) an UNTRACKED P (never judged for a tracked path: a later write over a
    // path a peer left dirty is MIXED work): P is newer than X's last stop.
    p.mtime.is_some_and(|m| m > x.last_seen_epoch + SLACK_SECS)
}

/// Attribute one dirty path of a tree several sessions hold.
///
/// `me` is this session's own slot (None when it has none); `known_other_judge`
/// is whether the judge is provably another session (rules (a) and (b) are
/// applied to THIS session only then — otherwise only the start bound (c) is).
pub fn attribute_dirty_path(
    p: &DirtyPath,
    me: Option<&PeerSlot>,
    peers: &[PeerSlot],
    known_other_judge: bool,
) -> Attribution {
    if peers.iter().any(|s| s.unreadable) || me.is_some_and(|m| m.unreadable) {
        return Attribution::Unreadable;
    }
    let mut all: Vec<&PeerSlot> = peers.iter().collect();
    if let Some(m) = me {
        all.push(m);
    }
    let peer_candidate = peers.iter().any(|x| !excluded(x, p, &all, true));
    let me_excluded = me.is_some_and(|m| excluded(m, p, &all, known_other_judge));
    if !peer_candidate {
        Attribution::Own
    } else if me_excluded {
        Attribution::Peer
    } else {
        Attribution::Ambiguous
    }
}

/// One touched tree's probes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeProbe {
    pub path: String,
    pub exists: bool,
    /// `rev-parse --git-dir` == `--git-common-dir` (a primary checkout, a
    /// `--separate-git-dir` primary and a submodule included).
    pub is_primary: Probe<bool>,
    pub slot: SlotRead,
    pub status: Probe<StatusProbe>,
    /// `rev-list --count HEAD --not --remotes`.
    pub unpushed: Probe<u32>,
    pub upstream: UpstreamProbe,
    pub patch_landed: PatchLanded,
    /// Dirty paths with attribution inputs, when the tree holds peers' slots.
    pub dirty: Vec<DirtyPath>,
    pub me: Option<PeerSlot>,
    pub peers: Vec<PeerSlot>,
    pub judge_is_known_other: bool,
}

fn evidence(probe: &TreeProbe) -> String {
    format!("{}: slot clean, status empty, nothing unpushed", probe.path)
}

/// Judge one tree.
pub fn custody_of_tree(probe: &TreeProbe) -> Custody {
    if !probe.exists {
        return Custody::Absent {
            evidence: format!("{}: worktree_missing", probe.path),
        };
    }
    match &probe.is_primary {
        Probe::Failed(why) => {
            return Custody::Unknown {
                reason: format!("probe_failed: rev-parse: {why}"),
            }
        }
        // Peers' WIP in a shared tree is not this session's owed work.
        Probe::Ok(true) => {
            return Custody::Unknown {
                reason: "shared_checkout".to_string(),
            }
        }
        Probe::Ok(false) => {}
    }
    let mut definite: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();

    // The custody slot: a cross-check that must POSITIVELY read clean.
    match &probe.slot {
        SlotRead::Missing => unknown.push("custody_record_missing".into()),
        SlotRead::Unreadable | SlotRead::OtherSession => {
            unknown.push("custody_record_unreadable".into())
        }
        SlotRead::Record { wip_state } => {
            if wip_state != "clean" && wip_state != "nothing_to_stash" {
                unknown.push(format!("custody_record_not_clean ({wip_state})"));
            }
        }
    }

    // The direct status probe.
    match &probe.status {
        Probe::Failed(why) => unknown.push(format!("probe_failed: status: {why}")),
        Probe::Ok(st) => {
            if st.stderr_nonempty {
                // What it did list is still judged, and a definite reason there
                // outranks this.
                unknown.push("probe_failed: git status wrote stderr".into());
            }
            let mut residue = 0usize;
            let mut served_unknown: Vec<String> = Vec::new();
            let mut served_definite: Vec<String> = Vec::new();
            for (path, v) in &st.served_corpus {
                match v {
                    ResidueVerdict::Residue => residue += 1,
                    ResidueVerdict::Definite(why) => {
                        served_definite.push(format!("{path} ({why})"))
                    }
                    ResidueVerdict::Unknown(why) => served_unknown.push(format!("{path} ({why})")),
                }
            }
            let mut dirty_paths: Vec<&String> = Vec::new();
            dirty_paths.extend(st.tracked_changes.iter());
            dirty_paths.extend(st.untracked.iter());
            // Attribution applies when peers hold the tree.
            let attributed = !probe.peers.is_empty();
            let mut own: Vec<String> = Vec::new();
            let mut peer: Vec<String> = Vec::new();
            let mut ambig: Vec<String> = Vec::new();
            let mut unread: Vec<String> = Vec::new();
            for path in &dirty_paths {
                if !attributed {
                    own.push((*path).clone());
                    continue;
                }
                let dp = probe
                    .dirty
                    .iter()
                    .find(|d| &d.path == *path)
                    .cloned()
                    .unwrap_or(DirtyPath {
                        path: (*path).clone(),
                        tracked: st.tracked_changes.contains(*path),
                        mtime: None,
                        produced_by: None,
                    });
                match attribute_dirty_path(
                    &dp,
                    probe.me.as_ref(),
                    &probe.peers,
                    probe.judge_is_known_other,
                ) {
                    Attribution::Own => own.push((*path).clone()),
                    Attribution::Peer => peer.push((*path).clone()),
                    Attribution::Ambiguous => ambig.push((*path).clone()),
                    Attribution::Unreadable => unread.push((*path).clone()),
                }
            }
            own.extend(served_definite);
            if !own.is_empty() {
                definite.push(format!("uncommitted: {}", own.join(", ")));
            }
            if !peer.is_empty() {
                unknown.push(format!(
                    "uncommitted_attributed_to_peer: {}",
                    peer.join(", ")
                ));
            }
            if !ambig.is_empty() {
                unknown.push(format!("uncommitted_unattributable: {}", ambig.join(", ")));
            }
            if !unread.is_empty() {
                unknown.push(format!("attribution_unreadable: {}", unread.join(", ")));
            }
            if !served_unknown.is_empty() {
                unknown.push(format!(
                    "served_corpus_unverifiable: {}",
                    served_unknown.join(", ")
                ));
            }
            // A non-empty listing that yields no reason, no residue verdict and
            // no left-out cache went unjudged.
            if st.listing_nonempty
                && dirty_paths.is_empty()
                && st.served_corpus.is_empty()
                && !st.stderr_nonempty
            {
                unknown.push("status_unjudged".into());
            }
            let _ = residue;
        }
    }

    // Unpushed commits: only a non-zero `rev-list --not --remotes` is definite.
    match &probe.unpushed {
        Probe::Failed(why) => unknown.push(format!("probe_failed: rev-list: {why}")),
        Probe::Ok(0) => {
            if let UpstreamProbe::Remote { ahead } = &probe.upstream {
                match ahead {
                    Probe::Ok(n) if *n > 0 => unknown.push("ahead_of_upstream".into()),
                    Probe::Ok(_) => {}
                    Probe::Failed(why) => unknown.push(format!("probe_failed: upstream: {why}")),
                }
            }
        }
        Probe::Ok(n) => match (&probe.patch_landed, &probe.upstream) {
            (PatchLanded::ProbeFailed(why), _) => {
                unknown.push(format!("probe_failed: patch-landed: {why}"))
            }
            (PatchLanded::AllLanded, _) => unknown.push("unpushed_but_patch_landed".into()),
            (_, UpstreamProbe::Gone) => unknown.push("upstream_gone".into()),
            (PatchLanded::NotAll | PatchLanded::NoRemotes, _) => {
                definite.push(format!("unpushed: {n} commit(s) on no remote"))
            }
        },
    }

    if !definite.is_empty() {
        return Custody::Dirty {
            reason: definite.join("; "),
        };
    }
    if !unknown.is_empty() {
        return Custody::Unknown {
            reason: unknown.join("; "),
        };
    }
    Custody::Clean {
        evidence: evidence(probe),
    }
}

/// A worktree coord names for this session (`worktree_path`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordTreeRef {
    pub path: String,
    /// The path exists on this box.
    pub exists_here: bool,
}

/// K over the touched set. Every coord-named worktree that exists on this box
/// must be a tree the run judged, else UNKNOWN `coord_names_unjudged_tree`
/// (one that is not on this box is a note, not a fault). No tree at all is
/// `Unknown(worktrees_unknown)`: absence of a discovered tree is not cleanliness.
pub fn custody_axis(
    trees: &[TreeProbe],
    coord: &[CoordTreeRef],
    discovery_ran: bool,
) -> CustodyAxis {
    if !discovery_ran {
        return CustodyAxis::Unknown(Unk::new(
            "custody",
            "worktrees_unknown: discovery did not run",
        ));
    }
    let mut out: Vec<(String, Custody)> = trees
        .iter()
        .map(|t| (t.path.clone(), custody_of_tree(t)))
        .collect();
    for c in coord {
        if c.exists_here && !trees.iter().any(|t| t.path == c.path) {
            out.push((
                c.path.clone(),
                Custody::Unknown {
                    reason: "coord_names_unjudged_tree".to_string(),
                },
            ));
        }
    }
    CustodyAxis::Known(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean_tree() -> TreeProbe {
        TreeProbe {
            path: "/w/t".into(),
            exists: true,
            is_primary: Probe::Ok(false),
            slot: SlotRead::Record {
                wip_state: "clean".into(),
            },
            status: Probe::Ok(StatusProbe {
                tracked_changes: vec![],
                untracked: vec![],
                served_corpus: vec![],
                listing_nonempty: false,
                stderr_nonempty: false,
            }),
            unpushed: Probe::Ok(0),
            upstream: UpstreamProbe::Remote {
                ahead: Probe::Ok(0),
            },
            patch_landed: PatchLanded::NotAll,
            dirty: vec![],
            me: None,
            peers: vec![],
            judge_is_known_other: false,
        }
    }

    fn dirty(c: &Custody) -> bool {
        matches!(c, Custody::Dirty { .. })
    }
    fn unknown(c: &Custody) -> bool {
        matches!(c, Custody::Unknown { .. })
    }

    #[test]
    fn c1_a_clean_pushed_linked_tree_with_a_clean_slot_is_clean() {
        assert!(matches!(
            custody_of_tree(&clean_tree()),
            Custody::Clean { .. }
        ));
        let mut t = clean_tree();
        t.slot = SlotRead::Record {
            wip_state: "nothing_to_stash".into(),
        };
        assert!(matches!(custody_of_tree(&t), Custody::Clean { .. }));
    }

    #[test]
    fn c2_c3_a_modified_or_untracked_file_is_definite() {
        let mut t = clean_tree();
        t.status = Probe::Ok(StatusProbe {
            tracked_changes: vec!["a.rs".into()],
            listing_nonempty: true,
            ..match clean_tree().status {
                Probe::Ok(s) => s,
                _ => unreachable!(),
            }
        });
        assert!(dirty(&custody_of_tree(&t)));
        let mut t = clean_tree();
        t.status = Probe::Ok(StatusProbe {
            untracked: vec!["new.txt".into()],
            listing_nonempty: true,
            ..match clean_tree().status {
                Probe::Ok(s) => s,
                _ => unreachable!(),
            }
        });
        assert!(dirty(&custody_of_tree(&t)));
    }

    #[test]
    fn c4_c5_unpushed_commits_are_definite_only_when_on_no_remote() {
        let mut t = clean_tree();
        t.unpushed = Probe::Ok(2);
        assert!(dirty(&custody_of_tree(&t)));
        // C35: every patch landed on a remote's default branch (rebase + prune).
        t.patch_landed = PatchLanded::AllLanded;
        assert!(unknown(&custody_of_tree(&t)));
        // C46: the configured upstream's remote branch is gone.
        t.patch_landed = PatchLanded::NotAll;
        t.upstream = UpstreamProbe::Gone;
        assert!(unknown(&custody_of_tree(&t)));
        // C39 / C47 / C51: the patch-landed probe fails, never definite.
        t.upstream = UpstreamProbe::None;
        t.patch_landed = PatchLanded::ProbeFailed("no default branch".into());
        assert!(unknown(&custody_of_tree(&t)));
        // A repo with no remote at all: definite.
        t.patch_landed = PatchLanded::NoRemotes;
        assert!(dirty(&custody_of_tree(&t)));
        // C5c: a LOCAL upstream proves nothing (the count is from --remotes).
        t.upstream = UpstreamProbe::Local;
        t.patch_landed = PatchLanded::NotAll;
        assert!(dirty(&custody_of_tree(&t)));
    }

    #[test]
    fn c29_ahead_of_upstream_with_every_commit_on_some_remote_is_unknown() {
        let mut t = clean_tree();
        t.upstream = UpstreamProbe::Remote {
            ahead: Probe::Ok(3),
        };
        assert!(unknown(&custody_of_tree(&t)));
    }

    #[test]
    fn c6_c8_the_slot_alone_is_never_a_definite_reason() {
        for w in [
            "captured",
            "deferred",
            "unchanged",
            "disabled",
            "stash_create_failed",
        ] {
            let mut t = clean_tree();
            t.slot = SlotRead::Record {
                wip_state: w.into(),
            };
            assert!(unknown(&custody_of_tree(&t)), "{w}");
        }
        for s in [
            SlotRead::Missing,
            SlotRead::Unreadable,
            SlotRead::OtherSession,
        ] {
            let mut t = clean_tree();
            t.slot = s;
            assert!(unknown(&custody_of_tree(&t)));
        }
        // A `captured` slot on a really dirty tree: the git probe decides.
        let mut t = clean_tree();
        t.slot = SlotRead::Record {
            wip_state: "captured".into(),
        };
        t.unpushed = Probe::Ok(1);
        assert!(dirty(&custody_of_tree(&t)));
    }

    #[test]
    fn c15_c16_a_primary_is_shared_and_a_missing_tree_is_absent_t14() {
        let mut t = clean_tree();
        t.is_primary = Probe::Ok(true);
        t.unpushed = Probe::Ok(5); // peers' work: still never definite
        assert!(unknown(&custody_of_tree(&t)));
        let mut t = clean_tree();
        t.exists = false;
        assert!(matches!(custody_of_tree(&t), Custody::Absent { .. }));
        let mut t = clean_tree();
        t.is_primary = Probe::Failed("x".into());
        assert!(unknown(&custody_of_tree(&t)));
    }

    #[test]
    fn c18_a_definite_reason_outranks_an_unknown_one_and_both_are_listed() {
        let mut t = clean_tree();
        t.slot = SlotRead::Missing;
        t.unpushed = Probe::Ok(1);
        let Custody::Dirty { reason } = custody_of_tree(&t) else {
            panic!()
        };
        assert!(reason.contains("unpushed"));
        // Unknown reasons are not mixed into Dirty (a Dirty tree is already refused).
        assert!(!reason.contains("custody_record"));
    }

    #[test]
    fn stderr_from_status_is_probe_failed_and_an_unjudged_listing_is_unknown() {
        let mut t = clean_tree();
        t.status = Probe::Ok(StatusProbe {
            stderr_nonempty: true,
            ..match clean_tree().status {
                Probe::Ok(s) => s,
                _ => unreachable!(),
            }
        });
        assert!(unknown(&custody_of_tree(&t)));
        let mut t = clean_tree();
        t.status = Probe::Ok(StatusProbe {
            listing_nonempty: true,
            ..match clean_tree().status {
                Probe::Ok(s) => s,
                _ => unreachable!(),
            }
        });
        assert!(unknown(&custody_of_tree(&t)));
        let mut t = clean_tree();
        t.status = Probe::Failed("timeout".into());
        assert!(unknown(&custody_of_tree(&t)));
    }

    #[test]
    fn served_corpus_residue_is_not_dirt_but_a_foreign_file_is() {
        let mk = |v: ResidueVerdict| {
            let mut t = clean_tree();
            t.status = Probe::Ok(StatusProbe {
                served_corpus: vec![(".claude/skills/x/SKILL.md".into(), v)],
                listing_nonempty: true,
                ..match clean_tree().status {
                    Probe::Ok(s) => s,
                    _ => unreachable!(),
                }
            });
            custody_of_tree(&t)
        };
        assert!(matches!(mk(ResidueVerdict::Residue), Custody::Clean { .. }));
        assert!(dirty(&mk(ResidueVerdict::Definite("edited".into()))));
        assert!(unknown(&mk(ResidueVerdict::Unknown("no checkout".into()))));
    }

    fn slot(id: &str, state: &str, last: u64, start: Option<u64>) -> PeerSlot {
        PeerSlot {
            session_id: id.into(),
            wip_state: state.into(),
            snapshot_resolves: true,
            first_seen: 0,
            last_seen_epoch: last,
            wip_captured_epoch: None,
            dirty_tracked_at_stop: vec![],
            started: start,
            unreadable: false,
        }
    }

    #[test]
    fn attribution_own_peer_ambiguous() {
        let path = |tracked: bool, mtime: u64, prod: Option<u64>| DirtyPath {
            path: "p".into(),
            tracked,
            mtime: Some(mtime),
            produced_by: prod,
        };
        let me = slot("me", "clean", 1_000, Some(500));
        let peer = slot("peer", "clean", 2_000, Some(100));
        // (a) a tracked path the peer's current record says was clean at its stop:
        // the peer is excluded, so with no other candidate the dirt is OWN.
        assert_eq!(
            attribute_dirty_path(
                &path(true, 3_000, None),
                Some(&me),
                std::slice::from_ref(&peer),
                true
            ),
            Attribution::Own
        );
        // (b) an untracked path newer than the peer's last stop + slack excludes the peer.
        assert_eq!(
            attribute_dirty_path(
                &path(false, 9_000, None),
                Some(&me),
                std::slice::from_ref(&peer),
                true
            ),
            Attribution::Own
        );
        // An untracked path within the peer's window leaves it a candidate; this
        // session excluded by its own earlier stop: PEER's.
        assert_eq!(
            attribute_dirty_path(
                &path(false, 1_900, None),
                Some(&me),
                std::slice::from_ref(&peer),
                true
            ),
            Attribution::Peer
        );
        // Judge not provably another session: rules (a)/(b) are not applied to me.
        assert_eq!(
            attribute_dirty_path(
                &path(false, 1_900, None),
                Some(&me),
                std::slice::from_ref(&peer),
                false
            ),
            Attribution::Ambiguous
        );
        // (c) a path produced before the peer began excludes the peer.
        assert_eq!(
            attribute_dirty_path(
                &path(false, 1_900, Some(10)),
                Some(&me),
                &[slot("peer", "clean", 2_000, Some(5_000))],
                false
            ),
            Attribution::Own
        );
        // An unreadable peer slot is never a definite reason.
        let mut bad = peer;
        bad.unreadable = true;
        assert_eq!(
            attribute_dirty_path(&path(true, 3_000, None), Some(&me), &[bad], true),
            Attribution::Unreadable
        );
    }

    #[test]
    fn a_peer_dirtied_tree_reads_unknown_not_dirty() {
        let mut t = clean_tree();
        t.status = Probe::Ok(StatusProbe {
            untracked: vec!["p".into()],
            listing_nonempty: true,
            ..match clean_tree().status {
                Probe::Ok(s) => s,
                _ => unreachable!(),
            }
        });
        t.dirty = vec![DirtyPath {
            path: "p".into(),
            tracked: false,
            mtime: Some(1_900),
            produced_by: None,
        }];
        t.me = Some(slot("me", "clean", 10_000, Some(500)));
        t.peers = vec![slot("peer", "clean", 2_000, Some(100))];
        t.judge_is_known_other = true;
        assert!(unknown(&custody_of_tree(&t)));
    }

    #[test]
    fn the_axis_requires_discovery_and_judges_every_coord_tree() {
        assert!(matches!(
            custody_axis(&[], &[], false),
            CustodyAxis::Unknown(_)
        ));
        let CustodyAxis::Known(v) = custody_axis(
            &[clean_tree()],
            &[
                CoordTreeRef {
                    path: "/w/other".into(),
                    exists_here: true,
                },
                CoordTreeRef {
                    path: "/w/elsewhere".into(),
                    exists_here: false,
                },
                CoordTreeRef {
                    path: "/w/t".into(),
                    exists_here: true,
                },
            ],
            true,
        ) else {
            panic!()
        };
        assert_eq!(v.len(), 2);
        assert!(v.iter().any(|(p, c)| p == "/w/other" && unknown(c)));
    }
}
