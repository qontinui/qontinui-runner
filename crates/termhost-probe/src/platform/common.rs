//! Platform-independent helpers the Windows and X11 layers share: occlusion
//! geometry, target selection by title, X11's foreground rule, the GPU counter
//! instance-name parser, and the doctor's capture probe. All pure or
//! trait-generic, so they are tested on every platform.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::clock::now_ns;
use crate::codec::Marker;
use crate::doctor::CaptureFacts;
use crate::raster::{scan, Rect, ScanOptions};

use super::{CaptureSource, CaptureStatus, RawFrame};

/// An overlap thinner than this (px) in either dimension cannot hide a marker
/// (markers are 20 cells of >= 6 px), so it does not count as occlusion — an
/// auto-hidden taskbar's 2 px strip along a maximised window's edge is the
/// case this exists for.
pub const SLIVER_PX: u32 = 8;

/// The area of the union of `rects` (coordinate compression; exact).
pub fn union_area(rects: &[Rect]) -> u64 {
    if rects.is_empty() {
        return 0;
    }
    let mut xs: Vec<i32> = rects.iter().flat_map(|r| [r.x, r.right()]).collect();
    let mut ys: Vec<i32> = rects.iter().flat_map(|r| [r.y, r.bottom()]).collect();
    xs.sort_unstable();
    xs.dedup();
    ys.sort_unstable();
    ys.dedup();
    let mut area = 0u64;
    for xi in xs.windows(2) {
        for yi in ys.windows(2) {
            let (cx, cy) = (xi[0], yi[0]);
            if rects
                .iter()
                .any(|r| r.x <= cx && cx < r.right() && r.y <= cy && cy < r.bottom())
            {
                area += u64::from((xi[1] - xi[0]) as u32) * u64::from((yi[1] - yi[0]) as u32);
            }
        }
    }
    area
}

/// The share of `target` covered by `above` (windows higher in the stacking
/// order), ignoring slivers. Returns (fraction, indices of the windows that
/// counted).
pub fn occlusion(target: &Rect, above: &[Rect]) -> (f64, Vec<usize>) {
    if target.area() == 0 {
        return (0.0, Vec::new());
    }
    let mut parts = Vec::new();
    let mut who = Vec::new();
    for (i, r) in above.iter().enumerate() {
        if let Some(x) = target.intersect(r) {
            if x.w >= SLIVER_PX && x.h >= SLIVER_PX {
                parts.push(x);
                who.push(i);
            }
        }
    }
    (union_area(&parts) as f64 / target.area() as f64, who)
}

/// Pick the target among the windows whose title contains `text` (with
/// their visible areas): the largest — or, with `unique`, the only one. An
/// empty title is refused: it would match every window, the operator's
/// editor included.
pub fn pick_by_title<H: Copy>(matches: &[(H, u64)], text: &str, unique: bool) -> Result<H, String> {
    if text.trim().is_empty() {
        return Err("an empty title would match every window".into());
    }
    if unique && matches.len() > 1 {
        return Err(format!(
            "{} windows' titles contain {text:?}; name the window with --hwnd or --pid",
            matches.len()
        ));
    }
    matches
        .iter()
        .max_by_key(|(_, area)| *area)
        .map(|(h, _)| *h)
        .ok_or_else(|| format!("no visible top-level window title contains {text:?}"))
}

/// X11's foreground rule. The keyboard focus is the authority — XTEST
/// delivers keys to it — so it must be the target or a descendant of the
/// target (`focus_chain`: the focused window, then its ancestors below the
/// root). A window manager's `_NET_ACTIVE_WINDOW`, when it names a window,
/// must agree as well (`active_top` against `target_top`, both top-level
/// windows); it is never enough on its own, because it goes stale when focus
/// moves to an override-redirect popup or a client sets the focus itself.
pub fn x11_focus_verdict(
    focus_chain: &[u32],
    target: u32,
    active_top: Option<u32>,
    target_top: u32,
) -> bool {
    // PointerRoot (1) and None (0) name no window.
    let Some(&focus) = focus_chain.first() else {
        return false;
    };
    if focus <= 1 || !focus_chain.contains(&target) {
        return false;
    }
    active_top.is_none_or(|a| a == target_top)
}

/// Parse a `\GPU Engine(*)` instance name —
/// `pid_1234_luid_0x00000000_0x0000D1E4_phys_0_eng_3_engtype_VideoDecode` —
/// into (pid, engine type).
pub fn parse_gpu_instance(name: &str) -> Option<(u32, String)> {
    let mut parts = name.split('_');
    let mut pid = None;
    let mut engtype = None;
    while let Some(p) = parts.next() {
        match p {
            "pid" => pid = parts.next().and_then(|v| v.parse().ok()),
            "engtype" => {
                let rest: Vec<&str> = parts.by_ref().collect();
                if !rest.is_empty() {
                    engtype = Some(rest.join("_"));
                }
            }
            _ => {}
        }
    }
    Some((pid?, engtype.unwrap_or_else(|| "unknown".into())))
}

/// Sum utilisation per (pid, engine type).
pub fn fold_gpu(
    items: impl IntoIterator<Item = (String, f64)>,
) -> std::collections::HashMap<u32, BTreeMap<String, f64>> {
    let mut out: std::collections::HashMap<u32, BTreeMap<String, f64>> =
        std::collections::HashMap::new();
    for (name, v) in items {
        if let Some((pid, engine)) = parse_gpu_instance(&name) {
            if pid != 0 && v.is_finite() {
                *out.entry(pid).or_default().entry(engine).or_default() += v.max(0.0);
            }
        }
    }
    out
}

/// `/proc/<pid>/stat` -> utime + stime in clock ticks. The command name (field
/// 2) is parenthesised and may hold spaces or parentheses, so fields are
/// counted after the LAST `)`.
pub fn parse_proc_stat_ticks(stat: &str) -> Option<u64> {
    let rest = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // After ')': state is field 3, so utime (14) is index 11, stime (15) index 12.
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// The value in kB of a `Key:   123 kB` line (`/proc/<pid>/smaps_rollup`,
/// `/proc/<pid>/status`), in bytes.
pub fn parse_kb_line(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|l| {
        let v = l.strip_prefix(key)?.strip_prefix(':')?;
        let kb: u64 = v.split_whitespace().next()?.parse().ok()?;
        Some(kb * 1024)
    })
}

/// The doctor's capture proof: pump `src` until a frame carries `expect`
/// decoded, or `timeout`.
pub fn probe_capture(
    src: &mut dyn CaptureSource,
    expect: Marker,
    timeout: Duration,
) -> CaptureFacts {
    let deadline = now_ns() + timeout.as_nanos() as u64;
    let mut frames = 0u32;
    let mut found = false;
    let mut rejected: Option<String> = None;
    let mut lost: Option<String> = None;
    let opts = ScanOptions::default();
    while now_ns() < deadline && !found {
        let status = src.next_frame(Duration::from_millis(100), &mut |raw: RawFrame<'_>| {
            frames += 1;
            let r = scan(&raw.view, &opts);
            if r.markers.iter().any(|f| f.marker == expect) {
                found = true;
            } else if let Some(rej) = r.rejected.first() {
                rejected = Some(rej.reason.as_str().to_string());
            }
        });
        match status {
            CaptureStatus::Ended(why) => {
                return CaptureFacts::Failed(format!("the probe window's capture ended: {why}"))
            }
            CaptureStatus::Lost(why) => lost = Some(why),
            CaptureStatus::Frame | CaptureStatus::Timeout => {}
        }
    }
    if frames == 0 {
        return CaptureFacts::NoFrame(format!(
            "no frame within {} ms{}",
            timeout.as_millis(),
            lost.map_or(String::new(), |l| format!(" (lost: {l})"))
        ));
    }
    CaptureFacts::Frame {
        decoded: if found {
            Ok(expect)
        } else {
            Err(rejected.unwrap_or_else(|| "no marker start found".into()))
        },
        degraded: None,
        detail: format!("{frames} frame(s), {}", src.describe()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn a_title_target_is_never_empty_and_inject_needs_a_unique_match() {
        let two = [(1u32, 100u64), (2, 900)];
        assert_eq!(pick_by_title(&two, "Terminal", false), Ok(2), "the largest");
        assert!(
            pick_by_title(&two, "Terminal", true).is_err(),
            "ambiguous for inject"
        );
        assert_eq!(pick_by_title(&two[..1], "Terminal", true), Ok(1));
        assert!(pick_by_title(&two, "", false).is_err());
        assert!(pick_by_title(&two, "  ", false).is_err());
        assert!(pick_by_title::<u32>(&[], "x", false).is_err());
    }

    #[test]
    fn x11_focus_is_the_authority_and_the_wm_hint_only_a_constraint() {
        // Focus on the target's own child widget, the WM agreeing: foreground.
        assert!(x11_focus_verdict(
            &[0x51, 0x50, 0x40],
            0x50,
            Some(0x40),
            0x40
        ));
        // Bare server (no WM hint): the focus alone decides.
        assert!(x11_focus_verdict(&[0x50], 0x50, None, 0x50));
        // The WM's hint is stale — it names the target — but the focus moved
        // to a popup: keys would go to the popup.
        assert!(!x11_focus_verdict(&[0x90], 0x50, Some(0x40), 0x40));
        // Focus inside the target, but the WM says another window is active.
        assert!(!x11_focus_verdict(&[0x50, 0x40], 0x50, Some(0x70), 0x40));
        // PointerRoot / None, or no focus reply at all.
        assert!(!x11_focus_verdict(&[1], 0x50, None, 0x50));
        assert!(!x11_focus_verdict(&[], 0x50, None, 0x50));
    }

    #[test]
    fn union_area_counts_overlaps_once() {
        assert_eq!(union_area(&[r(0, 0, 10, 10), r(5, 5, 10, 10)]), 175);
        assert_eq!(union_area(&[r(0, 0, 10, 10), r(0, 0, 10, 10)]), 100);
        assert_eq!(union_area(&[r(0, 0, 10, 10), r(20, 0, 5, 5)]), 125);
        assert_eq!(union_area(&[]), 0);
    }

    #[test]
    fn occlusion_ignores_slivers_and_reports_who() {
        let target = r(0, 0, 100, 100);
        let (f, who) = occlusion(
            &target,
            &[r(50, 0, 100, 100), r(0, 98, 100, 40), r(-50, -50, 20, 20)],
        );
        assert!((f - 0.5).abs() < 1e-9, "{f}");
        assert_eq!(who, vec![0]);
        let (f, _) = occlusion(&target, &[r(-10, -10, 200, 200)]);
        assert_eq!(f, 1.0);
    }

    #[test]
    fn proc_stat_fields_are_counted_after_the_last_paren() {
        let stat = "4242 (node (gen) x) S 1 4242 4242 0 -1 4194560 100 0 0 0 250 37 0 0 20 0 11 0 777 1000 50";
        assert_eq!(parse_proc_stat_ticks(stat), Some(287));
        assert_eq!(parse_proc_stat_ticks("garbage"), None);
    }

    #[test]
    fn kb_lines_parse() {
        let rollup = "55d0-7ffd ---p 00000000 00:00 0  [rollup]\nRss:              120416 kB\nPss:               80211 kB\nPss_Anon: 1 kB\n";
        assert_eq!(parse_kb_line(rollup, "Pss"), Some(80_211 * 1024));
        assert_eq!(parse_kb_line(rollup, "Rss"), Some(120_416 * 1024));
        assert_eq!(parse_kb_line(rollup, "Swap"), None);
    }

    #[test]
    fn gpu_instance_names_parse() {
        assert_eq!(
            parse_gpu_instance(
                "pid_1234_luid_0x00000000_0x0000D1E4_phys_0_eng_3_engtype_VideoDecode"
            ),
            Some((1234, "VideoDecode".to_string()))
        );
        assert_eq!(
            parse_gpu_instance("pid_9_luid_0x0_0x1_phys_0_eng_0_engtype_3D"),
            Some((9, "3D".to_string()))
        );
        assert_eq!(parse_gpu_instance("_Total"), None);
        let folded = fold_gpu([
            (
                "pid_9_luid_0x0_0x1_phys_0_eng_0_engtype_3D".to_string(),
                10.0,
            ),
            (
                "pid_9_luid_0x0_0x1_phys_0_eng_1_engtype_3D".to_string(),
                5.0,
            ),
            (
                "pid_9_luid_0x0_0x1_phys_0_eng_2_engtype_Copy".to_string(),
                1.0,
            ),
        ]);
        assert_eq!(folded[&9]["3D"], 15.0);
        assert_eq!(folded[&9]["Copy"], 1.0);
    }
}
