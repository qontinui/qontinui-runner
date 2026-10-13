//! The selftest's gates against an in-memory desktop (`common`): every check
//! passes when the desktop behaves, and each fault the selftest exists to
//! catch fails exactly the gate that watches for it. The selftest on a real
//! display runs in CI (Xvfb and windows-latest); this proves its verdict logic
//! everywhere, without a display, and proves each gate CAN fail (policy
//! testing/a-test-must-be-able-to-fail).

mod common;

use common::{FakeHost, Faults};
use termhost_probe::records::{CheckRecord, Sink};
use termhost_probe::selftest::{run, SelftestConfig, SelftestOutcome};

/// The default configuration shortened to what the fake needs: two delays,
/// five markers each, five keys; the hang freeze still exceeds the 2 s hang
/// threshold and the occlusion still spans enough 500 ms intervals.
fn quick() -> SelftestConfig {
    SelftestConfig {
        delays_ms: vec![0, 50],
        markers_per_delay: 5,
        keys: 5,
        key_interval_ms: 60,
        hang_freeze_ms: 2600,
        occlusion_ms: 4500,
        os_hung_freeze_ms: 1200,
        ..SelftestConfig::default()
    }
}

fn run_with(faults: Faults) -> SelftestOutcome {
    let mut host = FakeHost::new(faults);
    run(&mut host, &quick(), &mut Sink::discard())
}

fn find<'a>(o: &'a SelftestOutcome, name: &str) -> &'a CheckRecord {
    o.checks
        .iter()
        .find(|c| c.check == name)
        .unwrap_or_else(|| {
            panic!(
                "no check {name}; checks: {:?}",
                o.checks.iter().map(|c| &c.check).collect::<Vec<_>>()
            )
        })
}

fn report(o: &SelftestOutcome) -> String {
    o.checks
        .iter()
        .map(|c| {
            format!(
                "{} gated={} passed={:?}: {}",
                c.check, c.gated, c.passed, c.detail
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The gates a working desktop must pass — one per phase, so a selftest that
/// silently skipped a phase cannot pass this test. (`selftest_window` and
/// `input_listener` are recorded only when they fail, so they are not here.)
const KEY_CHECKS: [&str; 11] = [
    "clock_aligned",
    "process_sampler",
    "selftest_window_foreground",
    "capture_visible.wgc",
    "output_latency_slope.wgc",
    "hang_detected",
    "input_round_trip",
    "untagged_injection_aborts",
    "occlusion_reported",
    "os_not_responding_detected",
    "probe_kept_up",
];

#[test]
fn passes_on_a_well_behaved_desktop() {
    let o = run_with(Faults::default());
    for name in KEY_CHECKS {
        let c = find(&o, name);
        assert!(c.gated, "{name} is not gated:\n{}", report(&o));
        assert_eq!(c.passed, Some(true), "{name} failed:\n{}", report(&o));
    }
    assert_eq!(o.exit_code, 0, "{}\n{}", o.summary, report(&o));
}

/// Run with `faults` and require the selftest to fail, with `gate` among the
/// failed gated checks.
fn must_fail(faults: Faults, gate: &str) {
    let o = run_with(faults);
    let c = find(&o, gate);
    assert!(c.gated, "{gate} is not gated:\n{}", report(&o));
    assert_eq!(
        c.passed,
        Some(false),
        "{gate} passed under {faults:?}:\n{}",
        report(&o)
    );
    assert_eq!(o.exit_code, 1, "{}", o.summary);
}

#[test]
fn fails_when_capture_lags_the_paint() {
    // Every frame stamped 40 ms late: more than one 16.7 ms refresh interval.
    // DDA's absolute check catches it (WGC's absolute check is reported, not
    // gated, and a lag shared by both sources leaves their offset alone).
    let mut host = FakeHost::new(Faults {
        capture_lag_ms: 40,
        ..Faults::default()
    });
    host.dda = true;
    let o = run(&mut host, &quick(), &mut Sink::discard());
    let c = find(&o, "output_latency.dda.d0");
    assert!(c.gated && c.passed == Some(false), "{}", report(&o));
    let w = find(&o, "output_latency.wgc.d0");
    assert!(!w.gated && w.passed == Some(false), "{}", report(&o));
    assert!(w.detail.contains("one machine class"), "{}", w.detail);
    assert_eq!(o.exit_code, 1, "{}", o.summary);
}

#[test]
fn fails_when_the_listener_misses_an_untagged_injection() {
    must_fail(
        Faults {
            deaf_to_foreign: true,
            ..Faults::default()
        },
        "untagged_injection_aborts",
    );
}

#[test]
fn fails_when_occlusion_goes_unreported() {
    must_fail(
        Faults {
            occlusion_blind: true,
            ..Faults::default()
        },
        "occlusion_reported",
    );
}

#[test]
fn fails_when_keys_are_lost() {
    must_fail(
        Faults {
            drop_keys: true,
            ..Faults::default()
        },
        "input_round_trip",
    );
}

fn no_frame() -> termhost_probe::doctor::CaptureFacts {
    termhost_probe::doctor::CaptureFacts::NoFrame("no frame within 2000 ms".into())
}

#[test]
fn an_unknown_capture_mechanism_gates_its_checks_and_fails_when_broken() {
    // The doctor's own capture proof got no frame (UNKNOWN) and capture
    // really is broken: the selftest must go red, not quietly un-gate the
    // checks that would have caught the regression.
    let mut host = FakeHost::new(Faults {
        capture_dead: true,
        ..Faults::default()
    });
    host.wgc_facts = no_frame();
    let o = run(&mut host, &quick(), &mut Sink::discard());
    let c = find(&o, "capture_visible.wgc");
    assert!(c.gated, "an UNKNOWN mechanism must gate:\n{}", report(&o));
    assert_eq!(c.passed, Some(false), "{}", report(&o));
    assert_eq!(o.exit_code, 1, "{}", o.summary);
}

#[test]
fn an_unknown_capture_mechanism_that_works_passes() {
    let mut host = FakeHost::new(Faults::default());
    host.wgc_facts = no_frame();
    let o = run(&mut host, &quick(), &mut Sink::discard());
    let c = find(&o, "capture_visible.wgc");
    assert!(c.gated && c.passed == Some(true), "{}", report(&o));
    assert_eq!(o.exit_code, 0, "{}\n{}", o.summary, report(&o));
    // The Markdown summary names every check and the mechanisms.
    let md = termhost_probe::selftest::render_markdown(&o, "selftest");
    assert!(
        md.contains("| capture_visible.wgc | gated | pass |"),
        "{md}"
    );
    assert!(md.contains("| capture.wgc | UNKNOWN |"), "{md}");
}

#[test]
fn fails_when_the_probe_falls_behind_its_source() {
    // Every frame reaches the probe 80 ms after it was presented: past the
    // 50 ms bound, so the latencies it read include its own backlog.
    must_fail(
        Faults {
            probe_backlog_ms: 80,
            ..Faults::default()
        },
        "probe_kept_up",
    );
}

#[test]
fn a_probe_that_cannot_place_its_frames_in_time_has_not_shown_it_kept_up() {
    let o = run_with(Faults {
        presentation_unknown: true,
        ..Faults::default()
    });
    let c = find(&o, "probe_kept_up");
    assert!(c.gated, "{}", report(&o));
    assert_eq!(c.passed, None, "never established: {}", report(&o));
    assert_eq!(o.exit_code, 1, "{}", o.summary);
}

#[test]
fn an_unknown_the_environment_decides_is_recorded_not_gated() {
    // A GPU-less hosted VM where the PDH query errors: UNKNOWN, and not the
    // probe's fault, so its check is recorded — failing — but not gated.
    let mut host = FakeHost::new(Faults::default());
    host.gpu_facts =
        termhost_probe::doctor::GpuFacts::Failed("PdhCollectQueryData: 0x800007d5".into());
    let o = run(&mut host, &quick(), &mut Sink::discard());
    let c = find(&o, "gpu_counters_sampled");
    assert!(!c.gated, "{}", report(&o));
    assert_eq!(c.passed, Some(false), "it really failed: {}", report(&o));
    assert!(c.detail.starts_with("not gated"), "{}", c.detail);
    assert_eq!(
        o.exit_code,
        0,
        "{}
{}",
        o.summary,
        report(&o)
    );
}

#[test]
fn a_capture_source_that_never_opened_is_recorded_not_gated() {
    // The doctor could not open WGC at all (an HRESULT at CreateForWindow):
    // what the image allows, not a probe regression. Capture really is dead,
    // and the selftest records it without failing the job.
    let mut host = FakeHost::new(Faults {
        capture_dead: true,
        ..Faults::default()
    });
    host.wgc_facts =
        termhost_probe::doctor::CaptureFacts::OpenFailed("CreateForWindow: 0x80070005".into());
    let o = run(&mut host, &quick(), &mut Sink::discard());
    let c = find(&o, "capture_visible.wgc");
    assert!(!c.gated, "{}", report(&o));
    assert_eq!(c.passed, Some(false), "{}", report(&o));
    assert_eq!(
        o.exit_code,
        0,
        "{}
{}",
        o.summary,
        report(&o)
    );
}

#[test]
fn an_assumed_refresh_rate_still_cross_checks_with_the_labelled_tolerance() {
    // Two sources, and a display that reports no refresh rate: the
    // cross-check runs against the assumed interval, says so, and passes.
    let mut host = FakeHost::new(Faults::default());
    host.dda = true;
    host.refresh_assumed = true;
    let o = run(&mut host, &quick(), &mut Sink::discard());
    let c = find(&o, "capture_crosscheck");
    assert!(c.gated, "{}", report(&o));
    assert_eq!(c.passed, Some(true), "{}", report(&o));
    assert!(c.detail.contains("ASSUMED"), "{}", c.detail);
    assert_eq!(
        o.exit_code,
        0,
        "{}
{}",
        o.summary,
        report(&o)
    );
}

#[test]
fn one_late_frame_does_not_fail_the_kept_up_check() {
    // One frame 300 ms late — a hiccup on a loaded CI box — among the run's
    // frames: the check judges a statistic one frame cannot decide, says
    // which, and passes.
    let o = run_with(Faults {
        one_late_frame_ms: 300,
        ..Faults::default()
    });
    let c = find(&o, "probe_kept_up");
    assert!(c.gated, "{}", report(&o));
    assert_eq!(c.passed, Some(true), "{}", report(&o));
    assert!(c.detail.contains("1 over the bound"), "{}", c.detail);
    assert!(
        c.detail.contains("p95") || c.detail.contains("second_largest"),
        "{}",
        c.detail
    );
    assert_eq!(
        o.exit_code,
        0,
        "{}
{}",
        o.summary,
        report(&o)
    );
}

fn with_dda(faults: Faults) -> SelftestOutcome {
    let mut host = FakeHost::new(faults);
    host.dda = true;
    run(&mut host, &quick(), &mut Sink::discard())
}

#[test]
fn wgc_is_held_to_dda_by_its_render_offset() {
    // WGC and DDA see the same frames: within the offset, gated, passing.
    let o = with_dda(Faults::default());
    for d in [0, 50] {
        let c = find(&o, &format!("wgc_render_offset.d{d}"));
        assert!(c.gated && c.passed == Some(true), "{}", report(&o));
    }
    assert!(!find(&o, "output_latency.wgc.d0").gated, "{}", report(&o));
    assert_eq!(o.exit_code, 0, "{}\n{}", o.summary, report(&o));
    // WGC gains a frame (20 ms) on DDA: the offset check fails, gated.
    let o = with_dda(Faults {
        wgc_extra_ms: 20,
        ..Faults::default()
    });
    let c = find(&o, "wgc_render_offset.d0");
    assert!(c.gated && c.passed == Some(false), "{}", report(&o));
    assert_eq!(o.exit_code, 1, "{}", o.summary);
}

#[test]
fn without_dda_wgc_is_held_to_its_own_slope() {
    // From the eighth frame on (into the 50 ms phase) WGC frames are 30 ms
    // later: median(d) - d moves by 30 ms across the delays.
    let o = run_with(Faults {
        wgc_extra_from_frame: 8,
        wgc_extra_ms: 30,
        ..Faults::default()
    });
    let c = find(&o, "output_latency_slope.wgc");
    assert!(c.gated, "{}", report(&o));
    assert_eq!(c.passed, Some(false), "{}", report(&o));
    assert_eq!(o.exit_code, 1, "{}", o.summary);
}

#[test]
fn outside_the_measured_refresh_class_the_offset_band_is_reported_not_gated() {
    // 144 Hz: the band's milliseconds are not known to mean the same stage.
    let mut host = FakeHost::new(Faults::default());
    host.dda = true;
    host.refresh_ns = 6_944_444;
    let o = run(&mut host, &quick(), &mut Sink::discard());
    let c = find(&o, "wgc_render_offset.d0");
    assert!(!c.gated, "{}", report(&o));
    assert!(c.detail.contains("outside the class"), "{}", c.detail);
    assert_eq!(c.data["in_measured_class"], false);
    // At the default 60 Hz it gates (and passes: the fake's WGC frames sit
    // at the declared offset).
    let o = with_dda(Faults::default());
    let c = find(&o, "wgc_render_offset.d0");
    assert!(c.gated && c.passed == Some(true), "{}", report(&o));
    assert_eq!(c.data["in_measured_class"], true);
}

#[test]
fn without_dda_a_constant_wgc_regression_is_reported_not_gated() {
    // WGC gains a constant 40 ms on every frame: its slope is unchanged, so
    // nothing gated sees it — the absolute checks report it, ungated.
    let o = run_with(Faults {
        wgc_extra_ms: 40,
        ..Faults::default()
    });
    let slope = find(&o, "output_latency_slope.wgc");
    assert!(slope.gated && slope.passed == Some(true), "{}", report(&o));
    assert!(slope.detail.contains("CONSTANT"), "{}", slope.detail);
    let abs = find(&o, "output_latency.wgc.d0");
    assert!(!abs.gated && abs.passed == Some(false), "{}", report(&o));
    assert_eq!(o.exit_code, 0, "{}\n{}", o.summary, report(&o));
}
