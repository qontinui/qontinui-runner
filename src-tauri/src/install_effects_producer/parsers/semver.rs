//! A tiny, pure semver parser — just enough to classify a `from → to` version
//! delta as `major` / `minor` / `patch` / `none`, mirroring coord's
//! `dependency_observer::SemVer` (runner has no `semver` crate dep, and the
//! delta math only needs the three numeric cores).
//!
//! Tolerant by design: a leading `v`/`=`/`^`/`~`/`>=` range operator and any
//! build/prerelease suffix (`-rc.1`, `+build`) are stripped to the numeric
//! core before comparison. Unparseable input ⇒ `None` (the caller degrades to
//! the conservative `unknown` delta), never a panic.

/// The numeric core of a semantic version (`major.minor.patch`). Prerelease /
/// build metadata is intentionally NOT retained — the delta classifier only
/// compares the three cores, and coord recomputes its own `delta` from the
/// `from`/`to` strings we ship (this type is for the runner-side
/// `version_bumped` derivation only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemVer {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// True iff the original string carried a `-prerelease` suffix. Lets the
    /// delta classifier distinguish `1.2.3 → 1.2.3-rc.1` (a prerelease move)
    /// from an identical pin.
    pub prerelease: bool,
}

impl SemVer {
    /// Parse a version string into its numeric core. Strips a single leading
    /// range/equality sigil (`v`, `=`, `^`, `~`, `>`, `<`, `>=`, `<=`),
    /// whitespace, and any `-pre`/`+build` suffix. A missing minor/patch
    /// defaults to 0 (`"1"` ⇒ `1.0.0`, `"1.2"` ⇒ `1.2.0`). Returns `None` on
    /// any non-numeric core component.
    pub fn parse(raw: &str) -> Option<SemVer> {
        let s = raw.trim();
        // Strip a leading range/equality operator (multi-char first).
        let s = s
            .strip_prefix(">=")
            .or_else(|| s.strip_prefix("<="))
            .or_else(|| s.strip_prefix("=="))
            .or_else(|| s.strip_prefix('^'))
            .or_else(|| s.strip_prefix('~'))
            .or_else(|| s.strip_prefix('='))
            .or_else(|| s.strip_prefix('>'))
            .or_else(|| s.strip_prefix('<'))
            .or_else(|| s.strip_prefix('v'))
            .or_else(|| s.strip_prefix('V'))
            .unwrap_or(s)
            .trim();

        if s.is_empty() {
            return None;
        }

        // Split off build metadata (`+…`) then prerelease (`-…`).
        let s = s.split('+').next().unwrap_or(s);
        let (core, prerelease) = match s.split_once('-') {
            Some((c, _pre)) => (c, true),
            None => (s, false),
        };

        let mut it = core.split('.');
        let major = it.next()?.trim().parse::<u64>().ok()?;
        let minor = it
            .next()
            .map(|p| p.trim().parse::<u64>())
            .transpose()
            .ok()?
            .unwrap_or(0);
        let patch = it
            .next()
            .map(|p| p.trim().parse::<u64>())
            .transpose()
            .ok()?
            .unwrap_or(0);
        // Reject trailing garbage components (`1.2.3.4`).
        if it.next().is_some() {
            return None;
        }
        Some(SemVer {
            major,
            minor,
            patch,
            prerelease,
        })
    }
}

/// Classify the semver delta between two version strings as one of
/// `major` / `minor` / `patch` / `prerelease` / `none`, or `unknown` when
/// either side is unparseable. This is the runner-side mirror of coord's
/// `VersionBump::delta_kind` — coord recomputes its own `delta` from the
/// `from`/`to` we ship, but we expose this so the producer can pre-filter /
/// test the classification locally.
///
/// `prerelease` is returned only when the numeric cores are identical but the
/// prerelease flag differs (`1.2.3 → 1.2.3-rc.1`). Coord collapses that to
/// `none`; we keep it distinct for honesty in the producer's own logging.
pub fn delta_kind(from: &str, to: &str) -> &'static str {
    match (SemVer::parse(from), SemVer::parse(to)) {
        (Some(a), Some(b)) => {
            if b.major != a.major {
                "major"
            } else if b.minor != a.minor {
                "minor"
            } else if b.patch != a.patch {
                "patch"
            } else if b.prerelease != a.prerelease {
                "prerelease"
            } else {
                "none"
            }
        }
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_triple() {
        let v = SemVer::parse("1.2.3").unwrap();
        assert_eq!((v.major, v.minor, v.patch, v.prerelease), (1, 2, 3, false));
    }

    #[test]
    fn parses_partial_versions() {
        assert_eq!(
            SemVer::parse("1").unwrap(),
            SemVer {
                major: 1,
                minor: 0,
                patch: 0,
                prerelease: false
            }
        );
        assert_eq!(
            SemVer::parse("2.5").unwrap(),
            SemVer {
                major: 2,
                minor: 5,
                patch: 0,
                prerelease: false
            }
        );
    }

    #[test]
    fn strips_sigils_and_suffixes() {
        assert_eq!(SemVer::parse("^1.2.3").unwrap().major, 1);
        assert_eq!(SemVer::parse(">=4.0.0").unwrap().major, 4);
        assert_eq!(SemVer::parse("v3.1.0").unwrap().minor, 1);
        let pre = SemVer::parse("1.2.3-rc.1").unwrap();
        assert!(pre.prerelease);
        assert_eq!(pre.patch, 3);
        assert_eq!(SemVer::parse("1.2.3+build.99").unwrap().patch, 3);
    }

    #[test]
    fn rejects_garbage() {
        assert!(SemVer::parse("").is_none());
        assert!(SemVer::parse("not-a-version").is_none());
        assert!(SemVer::parse("1.2.3.4").is_none());
        assert!(SemVer::parse("1.x.0").is_none());
    }

    #[test]
    fn delta_classification() {
        assert_eq!(delta_kind("1.0.0", "2.0.0"), "major");
        assert_eq!(delta_kind("1.2.0", "1.3.0"), "minor");
        assert_eq!(delta_kind("1.2.3", "1.2.4"), "patch");
        assert_eq!(delta_kind("1.2.3", "1.2.3"), "none");
        assert_eq!(delta_kind("1.2.3", "1.2.3-rc.1"), "prerelease");
        assert_eq!(delta_kind("garbage", "1.0.0"), "unknown");
        assert_eq!(delta_kind("1.0.0", "garbage"), "unknown");
    }

    #[test]
    fn major_takes_precedence_over_minor_patch() {
        assert_eq!(delta_kind("1.5.9", "2.0.0"), "major");
        assert_eq!(delta_kind("1.1.1", "1.2.0"), "minor");
    }
}
