//! The external-volume contract: declare a removable volume, and answer
//! "is it *provably* there right now?" with three states, never two.
//!
//! Plan `2026-08-07-external-storage-tiering-for-fleet-disk-pressure`, Phase 3.
//!
//! # Why a sentinel and not `Path::exists`
//!
//! The binding mechanism is an NTFS **volume-GUID mount point**: the external
//! volume is mounted at a fixed directory (e.g. `D:\qontinui-ext\`) via
//! `mountvol <path> \\?\Volume{GUID}\`, so the binding is to *volume identity*
//! and survives reboot, replug and drive-letter reassignment.
//!
//! That buys durability and creates one lethal failure mode. **When the volume
//! is absent, the mount point is still an ordinary empty directory on the
//! internal disk.** `Path::exists` on it returns `true`. cargo will cheerfully
//! write 300 GiB into it — filling the very volume the relocation exists to
//! relieve — while every path still "works" and every check still passes.
//!
//! So presence is decided by a **sentinel file** ([`SENTINEL_FILENAME`]) whose
//! contents are the expected volume GUID:
//!
//! | Sentinel | Meaning | Verdict |
//! |---|---|---|
//! | present, contents match | the declared volume is mounted here | [`ExternalVolumeState::Present`] |
//! | missing (or dir missing) | volume not mounted — we are looking at the stub | [`ExternalVolumeState::Absent`] |
//! | present, contents differ | **a different volume is mounted here** | [`ExternalVolumeState::Mismatched`] |
//!
//! # The third state is the whole point
//!
//! `Mismatched` is *more* dangerous than `Absent`, not less: writing build
//! artifacts onto a volume that is not the one you declared is the silent
//! wrong-place bug that a drive-letter binding would have produced anyway. It
//! must never collapse into "present", and it must never collapse into
//! "absent" either — an absent volume is a clean refusal, a wrong volume is a
//! misconfiguration a human has to look at. That is why this module returns an
//! enum, and why the only way out of it is
//! [`ExternalVolumeState::refusal_reason`] — an `Option<String>` rather than a
//! `bool`, so a caller that wants to know "may I write here?" is forced to hold
//! the reason it may not. No refusal can reach an operator without saying which
//! of the two failure modes it was.
//!
//! # Fail-closed, and only for external paths
//!
//! [`external_state_for`] is the one function the disk gates call. It returns:
//!
//! - `None` — no external volume is declared, **or** this path is not under
//!   the declared mount. The caller keeps its existing behaviour exactly,
//!   fail-open included. On a box with no dock this is every path, which is
//!   how Phase 3 ships with literally no behaviour change.
//! - `Some(state)` — this path IS on the declared external volume, and here is
//!   whether it is provably usable. A caller that cannot resolve free space
//!   for such a path must **refuse**, not proceed: "I cannot see the volume"
//!   is precisely the condition under which a build must not start.
//!
//! The asymmetry with the internal case is deliberate and is argued in the
//! plan's "Disconnect behaviour" section: for internal storage the cost of
//! proceeding on an unknown probe is a failed build, so fail-open is right;
//! for a removable volume the cost is a **partially written artifact tree on a
//! volume that just went away**, or 300 GiB written into a stub. Refusal is
//! strictly recoverable. The alternative is not.

use std::path::{Path, PathBuf};

/// Declared mount point of the external volume (absolute path).
///
/// Unset ⇒ no external volume on this machine ⇒ every code path below is inert.
pub const EXTERNAL_VOLUME_PATH_ENV: &str = "QONTINUI_EXTERNAL_VOLUME_PATH";

/// Expected NTFS volume GUID of the external volume, as written into the
/// sentinel file. Compared case-insensitively and brace-insensitively, because
/// `mountvol`, `Get-Volume` and `wmic` disagree about both.
pub const EXTERNAL_VOLUME_GUID_ENV: &str = "QONTINUI_EXTERNAL_VOLUME_GUID";

/// Name of the sentinel file at the root of the declared mount point.
pub const SENTINEL_FILENAME: &str = ".qontinui-volume-id";

/// Whether the declared external volume is provably mounted right now.
///
/// Deliberately three-valued. See the module docs for why `Mismatched` may not
/// be folded into either neighbour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalVolumeState {
    /// The sentinel is missing: the mount point is the un-mounted stub (or is
    /// not there at all). Writers must refuse; nothing is corrupt.
    Absent,
    /// The sentinel is present and names the declared volume.
    Present,
    /// The sentinel is present and names a DIFFERENT volume. Harder-failed
    /// than [`Absent`](ExternalVolumeState::Absent) — a human must look.
    Mismatched { expected: String, found: String },
}

impl ExternalVolumeState {
    /// Operator-readable reason a gate refuses, or `None` when the volume is
    /// usable — the single sanctioned collapse out of the three states.
    ///
    /// Deliberately an `Option<String>` and not a `bool`. A call site reads as
    /// a policy decision ("may I write here?") rather than an observation ("is
    /// it mounted?") — `Mismatched` is emphatically *mounted* and emphatically
    /// not usable — and, more importantly, a caller cannot refuse without
    /// holding the sentence that tells the operator which failure it was. A
    /// bare presence predicate lived here briefly; nothing used it, and it
    /// offered a way to refuse without being able to explain why.
    pub fn refusal_reason(&self, mount: &Path) -> Option<String> {
        match self {
            ExternalVolumeState::Present => None,
            ExternalVolumeState::Absent => Some(format!(
                "declared external volume at {} is NOT mounted (sentinel {} absent) — \
                 refusing rather than writing into the un-mounted stub",
                mount.display(),
                SENTINEL_FILENAME
            )),
            ExternalVolumeState::Mismatched { expected, found } => Some(format!(
                "WRONG volume mounted at {}: sentinel {} names {found}, expected {expected} — \
                 refusing (this is a misconfiguration, not a disconnect)",
                mount.display(),
                SENTINEL_FILENAME
            )),
        }
    }
}

/// A declared external volume: where it is mounted, and which volume it must be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalVolume {
    pub mount: PathBuf,
    pub expected_guid: String,
}

/// Read the declaration from the environment.
///
/// **Both** variables are required. A half-declaration (a path with no GUID)
/// is treated as no declaration at all rather than as a path to trust blindly:
/// without an expected GUID the sentinel can only be tested for existence, and
/// existence is exactly the test the stub defeats.
pub fn declared() -> Option<ExternalVolume> {
    declared_from(
        std::env::var(EXTERNAL_VOLUME_PATH_ENV).ok().as_deref(),
        std::env::var(EXTERNAL_VOLUME_GUID_ENV).ok().as_deref(),
    )
}

/// Pure core of [`declared`], so the half-declaration rules are testable
/// without touching the process environment (which is global and would race
/// the rest of the suite).
fn declared_from(path: Option<&str>, guid: Option<&str>) -> Option<ExternalVolume> {
    let path = path?.trim();
    let guid = guid?.trim();
    if path.is_empty() || guid.is_empty() {
        return None;
    }
    Some(ExternalVolume {
        mount: PathBuf::from(path),
        expected_guid: guid.to_string(),
    })
}

impl ExternalVolume {
    /// Probe the sentinel. Read-only; never creates or repairs anything.
    pub fn probe(&self) -> ExternalVolumeState {
        let sentinel = self.mount.join(SENTINEL_FILENAME);
        match std::fs::read_to_string(&sentinel) {
            // An unreadable sentinel (missing, permission, I/O error on a
            // half-detached device) is ABSENT, not an error to propagate: the
            // caller's contract is "refuse unless provably present", so every
            // non-proof lands in the same refusing bucket. Distinguishing them
            // would add call-site branches that all do the same thing.
            Err(_) => ExternalVolumeState::Absent,
            Ok(found) => {
                if guid_eq(&found, &self.expected_guid) {
                    ExternalVolumeState::Present
                } else {
                    ExternalVolumeState::Mismatched {
                        expected: self.expected_guid.clone(),
                        found: found.trim().to_string(),
                    }
                }
            }
        }
    }

    /// Is `path` on this external volume?
    ///
    /// Prefix containment, matching [`crate::ci_node::admission::pick_volume`]'s
    /// longest-mount-prefix semantics — which is exactly why the plan mounts at
    /// `D:\qontinui-ext\`, *inside* the workspace root: the longer prefix wins,
    /// so the external volume is selected for external paths and the internal
    /// one for everything else, with no change to that function.
    pub fn contains(&self, path: &Path) -> bool {
        path_starts_with(path, &self.mount)
    }
}

/// The one call the disk gates make.
///
/// `None` means "not an external path — keep doing exactly what you did
/// before", which on a machine with no declaration is every path. `Some` means
/// "this is external; here is whether it is provably usable", and the caller
/// must fail CLOSED on anything that is not
/// [`ExternalVolumeState::Present`].
pub fn external_state_for(path: &Path) -> Option<ExternalVolumeState> {
    let vol = declared()?;
    if !vol.contains(path) {
        return None;
    }
    Some(vol.probe())
}

/// Case- and brace-insensitive volume-GUID comparison.
///
/// `mountvol` prints `\\?\Volume{d913fcde-...}\`, `Get-Volume` reports
/// `{d913fcde-...}` or a bare `d913fcde-...` depending on the property, and
/// operators paste whichever they had. A comparison strict enough to reject a
/// correct GUID for its punctuation would read as `Mismatched` — i.e. it would
/// report "wrong volume mounted" for the right volume, which is the worst
/// possible false positive here.
fn guid_eq(a: &str, b: &str) -> bool {
    normalize_guid(a) == normalize_guid(b)
}

fn normalize_guid(s: &str) -> String {
    s.trim()
        .trim_start_matches("\\\\?\\Volume")
        .trim_matches(|c: char| c == '{' || c == '}' || c == '\\' || c.is_whitespace())
        .to_ascii_lowercase()
}

/// `Path::starts_with`, but case-insensitive on Windows.
///
/// The stdlib compares path components case-SENSITIVELY on every platform.
/// On Windows `D:\qontinui-ext` and `D:\Qontinui-Ext` are the same directory,
/// so a case-sensitive test would answer "not external" for a correctly
/// declared volume — and "not external" routes straight back to the fail-OPEN
/// arm. That is the one direction this module must never fail in, so the
/// comparison is normalized rather than left to the default.
fn path_starts_with(path: &Path, prefix: &Path) -> bool {
    if cfg!(windows) {
        let norm = |p: &Path| {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
                .collect::<Vec<_>>()
        };
        let (p, pre) = (norm(path), norm(prefix));
        pre.len() <= p.len() && p[..pre.len()] == pre[..]
    } else {
        path.starts_with(prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- declaration -----------------------------------------------------

    #[test]
    fn both_vars_required() {
        assert!(declared_from(Some("D:/qontinui-ext"), Some("{abc}")).is_some());
        assert!(declared_from(Some("D:/qontinui-ext"), None).is_none());
        assert!(declared_from(None, Some("{abc}")).is_none());
        assert!(declared_from(None, None).is_none());
    }

    #[test]
    fn blank_is_not_a_declaration() {
        // An exported-but-empty variable is how a shell profile "unsets" a var
        // in practice; treating it as a declaration would arm the fail-closed
        // path with a mount of "" that matches every relative path.
        assert!(declared_from(Some("   "), Some("{abc}")).is_none());
        assert!(declared_from(Some("D:/qontinui-ext"), Some("")).is_none());
    }

    // ---- GUID comparison -------------------------------------------------

    #[test]
    fn guid_compare_ignores_punctuation_and_case() {
        let bare = "d913fcde-1111-2222-3333-444455556666";
        let braced = "{D913FCDE-1111-2222-3333-444455556666}";
        let mountvol = "\\\\?\\Volume{d913fcde-1111-2222-3333-444455556666}\\";
        assert!(guid_eq(bare, braced));
        assert!(guid_eq(braced, mountvol));
        assert!(guid_eq(mountvol, bare));
        // trailing newline is what `echo > sentinel` leaves behind
        assert!(guid_eq(&format!("{bare}\r\n"), braced));
    }

    #[test]
    fn different_guids_do_not_compare_equal() {
        assert!(!guid_eq(
            "{d913fcde-1111-2222-3333-444455556666}",
            "{ffffffff-1111-2222-3333-444455556666}"
        ));
    }

    // ---- containment -----------------------------------------------------

    #[test]
    fn contains_matches_paths_under_the_mount() {
        let v = ExternalVolume {
            mount: PathBuf::from("D:/qontinui-ext"),
            expected_guid: "{abc}".into(),
        };
        assert!(v.contains(Path::new("D:/qontinui-ext")));
        assert!(v.contains(Path::new("D:/qontinui-ext/targets/coord")));
        assert!(!v.contains(Path::new("D:/qontinui-root/qontinui-coord/target")));
        // A sibling whose name merely STARTS with the mount's name is not
        // under it — component-wise comparison, never a string prefix.
        assert!(!v.contains(Path::new("D:/qontinui-external-decoy/x")));
    }

    #[cfg(windows)]
    #[test]
    fn contains_is_case_insensitive_on_windows() {
        let v = ExternalVolume {
            mount: PathBuf::from("D:/qontinui-ext"),
            expected_guid: "{abc}".into(),
        };
        // The failure this pins: a case difference answering "not external"
        // would silently route an external path back to the fail-OPEN arm.
        assert!(v.contains(Path::new("D:/Qontinui-Ext/targets")));
        assert!(v.contains(Path::new("d:/QONTINUI-EXT/targets")));
    }

    // ---- the three sentinel states --------------------------------------

    fn temp_mount(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("qontinui-extvol-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_sentinel_reads_as_absent_not_present() {
        // This is the stub case: the directory EXISTS (as it always does when
        // the volume is detached), and `Path::exists` on it says true. The
        // probe must still say Absent.
        let mount = temp_mount("absent");
        assert!(
            mount.exists(),
            "the stub directory exists — that is the trap"
        );
        let v = ExternalVolume {
            mount: mount.clone(),
            expected_guid: "{d913fcde}".into(),
        };
        assert_eq!(v.probe(), ExternalVolumeState::Absent);
        assert!(v.probe().refusal_reason(&mount).is_some());
        let _ = std::fs::remove_dir_all(&mount);
    }

    #[test]
    fn matching_sentinel_reads_as_present() {
        let mount = temp_mount("present");
        std::fs::write(
            mount.join(SENTINEL_FILENAME),
            "{D913FCDE-1111-2222-3333-444455556666}\n",
        )
        .unwrap();
        let v = ExternalVolume {
            mount: mount.clone(),
            expected_guid: "d913fcde-1111-2222-3333-444455556666".into(),
        };
        assert_eq!(v.probe(), ExternalVolumeState::Present);
        assert!(v.probe().refusal_reason(&mount).is_none());
        let _ = std::fs::remove_dir_all(&mount);
    }

    #[test]
    fn mismatched_sentinel_is_not_present_and_not_absent() {
        // The load-bearing assertion of this module: a WRONG volume must not
        // read as usable, and must stay distinguishable from a missing one.
        let mount = temp_mount("mismatch");
        std::fs::write(mount.join(SENTINEL_FILENAME), "{ffffffff-dead-beef}").unwrap();
        let v = ExternalVolume {
            mount: mount.clone(),
            expected_guid: "{d913fcde-1111-2222-3333-444455556666}".into(),
        };
        let state = v.probe();
        assert!(
            state.refusal_reason(&mount).is_some(),
            "a wrong volume must never be usable"
        );
        assert_ne!(state, ExternalVolumeState::Absent, "must stay distinct");
        assert_ne!(state, ExternalVolumeState::Present);
        match state {
            ExternalVolumeState::Mismatched { expected, found } => {
                assert!(expected.contains("d913fcde"));
                assert!(found.contains("ffffffff"));
            }
            other => panic!("expected Mismatched, got {other:?}"),
        }
        // And the operator gets told which of the two it is.
        let reason = v.probe().refusal_reason(&mount).unwrap();
        assert!(reason.contains("WRONG volume"), "reason was: {reason}");
        let _ = std::fs::remove_dir_all(&mount);
    }

    #[test]
    fn absent_and_mismatched_give_different_refusal_reasons() {
        let mount = PathBuf::from("D:/qontinui-ext");
        let absent = ExternalVolumeState::Absent.refusal_reason(&mount).unwrap();
        let wrong = ExternalVolumeState::Mismatched {
            expected: "{a}".into(),
            found: "{b}".into(),
        }
        .refusal_reason(&mount)
        .unwrap();
        assert_ne!(absent, wrong);
        assert!(absent.contains("NOT mounted"));
        assert!(wrong.contains("WRONG volume"));
    }

    // ---- the no-declaration inertness guarantee -------------------------

    #[test]
    fn undeclared_volume_makes_every_path_non_external() {
        // Phase 3's shipping contract: with nothing declared, behaviour is
        // byte-identical to today. `external_state_for` returning None is the
        // mechanism — every gate keeps its existing fail-open arm.
        //
        // Asserted on the pure core rather than on `external_state_for`, which
        // reads the real process environment and would race a parallel test
        // that sets it.
        assert!(declared_from(None, None).is_none());
    }

    #[test]
    fn a_declared_volume_does_not_capture_unrelated_paths() {
        let v = declared_from(Some("D:/qontinui-ext"), Some("{abc}")).unwrap();
        // The internal workspace root must keep its fail-OPEN behaviour: this
        // plan does not regress the internal-only case.
        assert!(!v.contains(Path::new("D:/qontinui-root/qontinui-runner/target")));
        assert!(!v.contains(Path::new("C:/Users/x/AppData/Local/Temp")));
    }
}
