//! The CI reservation (plan D6): memory the CI slices may claim, which agent
//! builds must leave alone.
//!
//! ```text
//! ci_reservation = Σ over top-level CI slices of reserve(slice)
//! reserve(slice) = max(memory.current, min(memory.max, memory.high))   when bounded
//! ```
//!
//! **An unlimited top-level slice is bounded by its children.** Measured on
//! merytshost 2026-10-06 (plan Progress): `ci.slice` has `MemoryHigh`/`MemoryMax`
//! = `max`, and its only child `ci-runners.slice` carries the real limits
//! (148.5 GB high / 198 GB max). Reading the top level alone would make the
//! reservation infinite (or, clamped, the whole machine). So an unbounded slice
//! reserves the sum of its children's reservations plus whatever it holds
//! outside them; an unbounded LEAF reserves only what it currently uses.
//!
//! Every read is fallible: an unreadable `memory.current` anywhere on the path
//! makes the whole reservation `Unknown`, never 0. No CI slice at all is a
//! measured 0, labelled `no_ci_slice`.

use std::path::Path;

use qontinui_types::build_admission::Fact;
use serde::Serialize;

/// One cgroup memory limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "bytes")]
pub enum Limit {
    Unlimited,
    Bytes(u64),
    Unreadable,
}

/// A slice (or a service/scope inside one) and the cgroups nested in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SliceNode {
    pub name: String,
    /// `None` = unreadable.
    pub current: Option<u64>,
    /// `anon` from `memory.stat`: what the work actually holds, without
    /// reclaimable page cache. `None` = unreadable.
    pub anon: Option<u64>,
    pub high: Limit,
    pub max: Limit,
    pub children: Vec<SliceNode>,
}

/// Where the reservation came from, rendered beside the number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CiSource {
    /// Read from the CI slices.
    Measured,
    /// The host has no top-level `ci*.slice`.
    NoCiSlice,
    /// cgroupfs is not available (not Linux, or not cgroup v2).
    NotSupported,
    /// A slice exists but a read failed.
    Unknown,
}

/// The tightest of a slice's own limits, or `None` when it has none.
/// `Err(())` when a limit could not be read.
fn own_bound(node: &SliceNode) -> Result<Option<u64>, ()> {
    let mut bound: Option<u64> = None;
    for l in [node.high, node.max] {
        match l {
            Limit::Unreadable => return Err(()),
            Limit::Unlimited => {}
            Limit::Bytes(b) => bound = Some(bound.map_or(b, |x: u64| x.min(b))),
        }
    }
    Ok(bound)
}

/// `reserve(slice)`; `None` when any input on the path is unreadable.
pub fn reserve(node: &SliceNode) -> Option<u64> {
    let current = node.current?;
    match own_bound(node).ok()? {
        Some(bound) => Some(current.max(bound)),
        None if node.children.is_empty() => Some(current),
        None => {
            let mut sum = 0u64;
            let mut children_current = 0u64;
            for c in &node.children {
                sum = sum.saturating_add(reserve(c)?);
                children_current = children_current.saturating_add(c.current?);
            }
            // What the slice holds outside its children (it can hold none in
            // cgroup v2, but a read race can show a few pages).
            Some(sum.saturating_add(current.saturating_sub(children_current)))
        }
    }
}

/// What the CI slices reserve and what they hold right now.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CiMeasure {
    /// D6's reservation: what agent builds must leave to CI.
    pub reservation: Fact<u64>,
    /// The anonymous memory the CI slices hold NOW (sum of top-level `anon`).
    /// This, never the reservation, is what comes off used memory to give
    /// non-build use (D3: non-build is used memory OUTSIDE the CI slices).
    pub usage: Fact<u64>,
    pub source: CiSource,
}

impl CiMeasure {
    pub fn not_supported() -> Self {
        Self::uniform(Fact::NotSupported, CiSource::NotSupported)
    }

    fn uniform(f: Fact<u64>, source: CiSource) -> Self {
        CiMeasure {
            reservation: f,
            usage: f,
            source,
        }
    }
}

/// The host's CI reservation and usage over `tops` (the top-level CI slices).
pub fn reservation(tops: &[SliceNode]) -> CiMeasure {
    if tops.is_empty() {
        return CiMeasure::uniform(Fact::Measured(0), CiSource::NoCiSlice);
    }
    let mut total = 0u64;
    let mut usage = Some(0u64);
    for t in tops {
        let Some(r) = reserve(t) else {
            return CiMeasure::uniform(Fact::Unknown, CiSource::Unknown);
        };
        total = total.saturating_add(r);
        usage = usage.and_then(|u| t.anon.map(|a| u.saturating_add(a)));
    }
    CiMeasure {
        reservation: Fact::Measured(total),
        usage: usage.map_or(Fact::Unknown, Fact::Measured),
        source: CiSource::Measured,
    }
}

fn read_limit(dir: &Path, file: &str) -> Limit {
    match std::fs::read_to_string(dir.join(file)) {
        Ok(s) => {
            let s = s.trim();
            if s == "max" {
                Limit::Unlimited
            } else {
                s.parse().map(Limit::Bytes).unwrap_or(Limit::Unreadable)
            }
        }
        Err(_) => Limit::Unreadable,
    }
}

fn read_anon(dir: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(dir.join("memory.stat")).ok()?;
    text.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        if it.next() == Some("anon") {
            it.next()?.parse().ok()
        } else {
            None
        }
    })
}

fn read_node(dir: &Path, name: String) -> SliceNode {
    let current = std::fs::read_to_string(dir.join("memory.current"))
        .ok()
        .and_then(|s| s.trim().parse().ok());
    let mut children: Vec<SliceNode> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    // A limited service or scope directly under an unlimited
                    // slice bounds what it may take just as a child slice does.
                    let cgroup =
                        n.ends_with(".slice") || n.ends_with(".service") || n.ends_with(".scope");
                    cgroup.then(|| read_node(&e.path(), n))
                })
                .collect()
        })
        .unwrap_or_default();
    children.sort_by(|a, b| a.name.cmp(&b.name));
    SliceNode {
        name,
        current,
        anon: read_anon(dir),
        high: read_limit(dir, "memory.high"),
        max: read_limit(dir, "memory.max"),
        children,
    }
}

/// `ci.slice` or `ci-<anything>.slice` — not merely a name starting "ci".
pub fn is_ci_slice(name: &str) -> bool {
    name == "ci.slice" || (name.starts_with("ci-") && name.ends_with(".slice"))
}

/// Read the top-level CI slice trees under a cgroup v2 root. `None` when the
/// root is not a readable cgroup v2 hierarchy (not supported here).
pub fn read_tops(cgroup_root: &Path) -> Option<Vec<SliceNode>> {
    if !cgroup_root.join("cgroup.controllers").exists() {
        return None;
    }
    let mut tops: Vec<SliceNode> = std::fs::read_dir(cgroup_root)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            is_ci_slice(&n).then(|| read_node(&e.path(), n))
        })
        .collect();
    tops.sort_by(|a, b| a.name.cmp(&b.name));
    Some(tops)
}

/// [`reservation`] read from cgroupfs.
pub fn measure(cgroup_root: &Path) -> CiMeasure {
    match read_tops(cgroup_root) {
        None => CiMeasure::uniform(Fact::NotSupported, CiSource::NotSupported),
        Some(tops) => reservation(&tops),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: u64 = 1 << 30;

    fn node(
        name: &str,
        current: Option<u64>,
        high: Limit,
        max: Limit,
        children: Vec<SliceNode>,
    ) -> SliceNode {
        SliceNode {
            name: name.into(),
            current,
            anon: current,
            high,
            max,
            children,
        }
    }

    /// The merytshost shape: an unlimited `ci.slice` whose only child carries
    /// the limits. The reservation is the child's bound, not infinity; the
    /// usage is what it holds now.
    #[test]
    fn unlimited_top_takes_its_limited_child() {
        let runners = node(
            "ci-runners.slice",
            Some(43 * G),
            Limit::Bytes(148_512_964_608),
            Limit::Bytes(198_017_286_144),
            vec![],
        );
        let ci = node(
            "ci.slice",
            Some(43 * G),
            Limit::Unlimited,
            Limit::Unlimited,
            vec![runners],
        );
        let m = reservation(&[ci]);
        assert_eq!(
            (m.reservation, m.source),
            (Fact::Measured(148_512_964_608), CiSource::Measured)
        );
        assert_eq!(m.usage, Fact::Measured(43 * G));
    }

    #[test]
    fn a_bounded_slice_over_its_bound_reserves_its_current() {
        let s = node(
            "ci.slice",
            Some(160 * G),
            Limit::Bytes(150 * G),
            Limit::Unlimited,
            vec![],
        );
        assert_eq!(reserve(&s), Some(160 * G));
    }

    #[test]
    fn an_unlimited_leaf_reserves_what_it_uses_and_an_unlimited_tree_sums() {
        let a = node(
            "a.slice",
            Some(10 * G),
            Limit::Unlimited,
            Limit::Unlimited,
            vec![],
        );
        let b = node(
            "b.slice",
            Some(5 * G),
            Limit::Bytes(20 * G),
            Limit::Unlimited,
            vec![],
        );
        // 1 GiB held outside the children.
        let top = node(
            "ci.slice",
            Some(16 * G),
            Limit::Unlimited,
            Limit::Unlimited,
            vec![a, b],
        );
        assert_eq!(reserve(&top), Some(10 * G + 20 * G + G));
    }

    #[test]
    fn unreadable_anywhere_is_unknown_never_zero() {
        let child = node(
            "ci-runners.slice",
            None,
            Limit::Bytes(G),
            Limit::Bytes(G),
            vec![],
        );
        let ci = node(
            "ci.slice",
            Some(G),
            Limit::Unlimited,
            Limit::Unlimited,
            vec![child],
        );
        let m = reservation(&[ci]);
        assert_eq!(
            (m.reservation, m.usage, m.source),
            (Fact::Unknown, Fact::Unknown, CiSource::Unknown)
        );
        let bad = node(
            "ci.slice",
            Some(G),
            Limit::Unreadable,
            Limit::Unlimited,
            vec![],
        );
        assert_eq!(reservation(&[bad]).reservation, Fact::Unknown);
        let mut no_stat = node(
            "ci.slice",
            Some(G),
            Limit::Bytes(G),
            Limit::Unlimited,
            vec![],
        );
        no_stat.anon = None;
        let m = reservation(&[no_stat]);
        assert_eq!((m.reservation, m.usage), (Fact::Measured(G), Fact::Unknown));
    }

    #[test]
    fn no_ci_slice_is_a_measured_zero_with_its_label_and_names_match_exactly() {
        let m = reservation(&[]);
        assert_eq!(
            (m.reservation, m.usage, m.source),
            (Fact::Measured(0), Fact::Measured(0), CiSource::NoCiSlice)
        );
        assert!(is_ci_slice("ci.slice") && is_ci_slice("ci-runners.slice"));
        assert!(!is_ci_slice("circus.slice") && !is_ci_slice("ci.service"));
    }

    fn cg(dir: &Path, cur: &str, high: &str, max: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("memory.current"), cur).unwrap();
        std::fs::write(dir.join("memory.high"), high).unwrap();
        std::fs::write(dir.join("memory.max"), max).unwrap();
        std::fs::write(dir.join("memory.stat"), format!("anon {cur}\nfile 5\n")).unwrap();
    }

    #[test]
    fn reads_a_cgroupfs_tree() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::write(root.join("cgroup.controllers"), "cpu memory\n").unwrap();
        let ci = root.join("ci.slice");
        let runners = ci.join("ci-runners.slice");
        cg(&ci, "100", "max", "max");
        cg(&runners, "90", "4096", "8192");
        cg(
            &runners.join("actions.runner.x.service"),
            "80",
            "max",
            "max",
        );
        std::fs::create_dir_all(root.join("user.slice")).unwrap();
        cg(&root.join("circus.slice"), "999999", "max", "max");
        // 4096 from the bounded child + 10 bytes held directly by ci.slice.
        let m = measure(root);
        assert_eq!(
            (m.reservation, m.usage, m.source),
            (
                Fact::Measured(4106),
                Fact::Measured(100),
                CiSource::Measured
            )
        );
        assert_eq!(measure(&root.join("nope")).source, CiSource::NotSupported);
    }

    #[test]
    fn a_limited_service_bounds_an_unlimited_top() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::write(root.join("cgroup.controllers"), "memory\n").unwrap();
        let top = root.join("ci-x.slice");
        cg(&top, "10", "max", "max");
        cg(&top.join("a.service"), "10", "1000", "max");
        assert_eq!(measure(root).reservation, Fact::Measured(1000));
    }
}
