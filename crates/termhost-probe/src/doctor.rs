//! `doctor`: which measurement mechanisms work on THIS machine, in the
//! vocabulary of qontinui-claude-config `scripts/capability-doctor.sh`:
//!
//! - OPERATIVE — every dependency is here AND a real probe proved it (a frame
//!   captured and its marker decoded; the clock compared through a pipe);
//! - DEGRADED — it runs, but something is missing (the reason says what);
//! - INOPERATIVE-ON-THIS-MACHINE — a hard dependency is absent;
//! - UNKNOWN — the probe that would decide could not run or got no readable
//!   answer. An unprobed mechanism is neither operative nor inoperative.
//!
//! The platform layer gathers [`Facts`] (it creates a small non-activating
//! probe window, paints a calibration marker, and captures it with each
//! source); the mapping from facts to verdicts below is pure and tested.
//! Whether a hosted CI session can capture at all is therefore MEASURED, never
//! assumed (plan Phase 2, CI).

use serde::Serialize;

use crate::clock;
use crate::codec::Marker;
use crate::records::{DoctorRecord, DoctorSummary, MechanismVerdict};
use crate::spec::spec;

/// A mechanism's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum State {
    #[serde(rename = "OPERATIVE")]
    Operative,
    #[serde(rename = "DEGRADED")]
    Degraded,
    #[serde(rename = "INOPERATIVE-ON-THIS-MACHINE")]
    Inoperative,
    #[serde(rename = "UNKNOWN")]
    Unknown,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Operative => "OPERATIVE",
            State::Degraded => "DEGRADED",
            State::Inoperative => "INOPERATIVE-ON-THIS-MACHINE",
            State::Unknown => "UNKNOWN",
        }
    }
}

pub const CAPTURE_WGC: &str = "capture.wgc";
pub const CAPTURE_DDA: &str = "capture.dda";
pub const CAPTURE_X11: &str = "capture.x11";
pub const INPUT_SAFE: &str = "input.safe";
pub const CLOCK_ALIGNED: &str = "clock.aligned";
pub const COUNTERS_GPU: &str = "counters.gpu";
pub const SESSION_INTERACTIVE: &str = "session.interactive";

/// Every mechanism, in report order, with what it depends on.
pub const MECHANISMS: [(&str, &str); 7] = [
    (
        SESSION_INTERACTIVE,
        "an interactive, unlocked desktop session (WinSta0\\Default input desktop) / a reachable X display",
    ),
    (
        CAPTURE_WGC,
        "Windows 10 1803+ Windows.Graphics.Capture, a D3D11 device, session.interactive; proven by capturing and decoding the doctor's own probe window",
    ),
    (
        CAPTURE_DDA,
        "DXGI 1.2 Desktop Duplication of the output holding the window, session.interactive; proven by duplicating and decoding the probe window",
    ),
    (
        CAPTURE_X11,
        "an X11 display ($DISPLAY) with DAMAGE + XFIXES (MIT-SHM >= 1.2 for shared-memory reads); proven by capturing and decoding the probe window",
    ),
    (
        INPUT_SAFE,
        "session.interactive plus a non-blocking real-input listener (Raw Input on Windows; XInput2 raw events on X11) and an injector whose events are distinguishable (SendInput dwExtraInfo tag / XTEST with a per-run ledger)",
    ),
    (
        CLOCK_ALIGNED,
        "node on PATH; process.hrtime on the probe clock (QPC / CLOCK_MONOTONIC), measured by a pipe round trip",
    ),
    (
        COUNTERS_GPU,
        "PDH \\GPU Engine(*)\\Utilization Percentage instances (Windows with a WDDM 2.0+ GPU)",
    ),
];

/// What the platform found out about the session.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionFacts {
    /// `Err` = this platform has no such concept / support.
    pub supported: Result<(), String>,
    /// Interactive and unlocked: `Ok(detail)`; locked or a secure desktop:
    /// `Err(Degraded(detail))`; non-interactive: `Err(Inoperative(detail))`.
    pub state: Result<String, SessionProblem>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SessionProblem {
    /// Interactive but currently unusable (locked, UAC prompt, display asleep).
    Unavailable(String),
    /// Not an interactive session at all (service session, no display).
    NotInteractive(String),
    /// The query failed.
    Unknown(String),
}

/// What capturing the probe window with one source showed.
#[derive(Clone, Debug, PartialEq)]
pub enum CaptureFacts {
    /// Not this platform's mechanism.
    NotApplicable(String),
    /// Skipped because the session cannot display a window.
    NoSession(String),
    /// The API is missing or refused (e.g. `IsSupported` false, duplication
    /// unsupported in this session).
    Unavailable(String),
    /// The API failed to START (an HRESULT or X error opening the source), or
    /// the probe window it would capture could not be shown — no source ever
    /// opened. What the environment allows decides this, so the selftest
    /// records it and does not gate on it.
    OpenFailed(String),
    /// The source OPENED, then failed before a frame.
    Failed(String),
    /// The source OPENED, but no frame within the probe's timeout.
    NoFrame(String),
    /// A frame arrived; the decode of the probe window's calibration marker.
    Frame {
        decoded: Result<Marker, String>,
        /// Something that still works but is reduced (e.g. no MIT-SHM).
        degraded: Option<String>,
        detail: String,
    },
}

/// The input safeguard's prerequisites.
#[derive(Clone, Debug, PartialEq)]
pub struct InputFacts {
    pub supported: Result<(), String>,
    /// The real-input listener registered and running.
    pub listener: Result<String, String>,
    /// The injector is available and its events are distinguishable.
    pub injector: Result<String, String>,
}

/// The clock round trip with node.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ClockAlignment {
    /// node's hrtime minus the probe clock's midpoint estimate.
    pub offset_ns: i64,
    /// The best (shortest) round trip.
    pub rtt_ns: u64,
    pub samples: u32,
    pub aligned: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ClockFacts {
    /// node is not on PATH (or would not start).
    NoNode(String),
    Failed(String),
    Measured(ClockAlignment),
}

#[derive(Clone, Debug, PartialEq)]
pub enum GpuFacts {
    NotApplicable(String),
    /// The counter set does not exist here (no WDDM 2.0+ GPU).
    Absent(String),
    Failed(String),
    Instances(usize),
}

/// Everything the doctor's verdicts are computed from.
#[derive(Clone, Debug, PartialEq)]
pub struct Facts {
    pub host: String,
    pub os: String,
    pub session: SessionFacts,
    pub wgc: CaptureFacts,
    pub dda: CaptureFacts,
    pub x11: CaptureFacts,
    pub input: InputFacts,
    pub clock: ClockFacts,
    pub gpu: GpuFacts,
    /// D9: the foreground window changed between just before the doctor's
    /// probe window was created and just after it closed (Windows) — the
    /// probe must never take the operator's focus. `None` = unchanged, or
    /// not measured on this platform.
    pub foreground_violation: Option<String>,
}

fn v(id: &'static str, state: State, reason: impl Into<String>, at: &str) -> MechanismVerdict {
    let depends_on = MECHANISMS
        .iter()
        .find(|(m, _)| *m == id)
        .map(|(_, d)| *d)
        .unwrap_or("");
    MechanismVerdict {
        id,
        state,
        reason: reason.into(),
        checked_at: at.to_string(),
        depends_on,
    }
}

pub fn session_verdict(f: &SessionFacts, at: &str) -> MechanismVerdict {
    match (&f.supported, &f.state) {
        (Err(why), _) => v(SESSION_INTERACTIVE, State::Inoperative, why.clone(), at),
        (Ok(()), Ok(detail)) => v(SESSION_INTERACTIVE, State::Operative, detail.clone(), at),
        (Ok(()), Err(SessionProblem::Unavailable(d))) => {
            v(SESSION_INTERACTIVE, State::Degraded, d.clone(), at)
        }
        (Ok(()), Err(SessionProblem::NotInteractive(d))) => {
            v(SESSION_INTERACTIVE, State::Inoperative, d.clone(), at)
        }
        (Ok(()), Err(SessionProblem::Unknown(d))) => {
            v(SESSION_INTERACTIVE, State::Unknown, d.clone(), at)
        }
    }
}

pub fn capture_verdict(id: &'static str, f: &CaptureFacts, at: &str) -> MechanismVerdict {
    match f {
        CaptureFacts::NotApplicable(d) => v(id, State::Inoperative, d.clone(), at),
        CaptureFacts::NoSession(d) => v(id, State::Inoperative, format!("no interactive session to capture: {d}"), at),
        CaptureFacts::Unavailable(d) => v(id, State::Inoperative, d.clone(), at),
        // A probe that got no readable answer decides nothing: the mechanism
        // may be broken here, or the probe may be. Which of these the
        // selftest gates on is `gates`' call, not the state's.
        CaptureFacts::OpenFailed(d) => v(id, State::Unknown, format!("the capture source did not open: {d}"), at),
        CaptureFacts::Failed(d) => v(id, State::Unknown, format!("capture opened but failed before a frame: {d}"), at),
        CaptureFacts::NoFrame(d) => v(id, State::Unknown, format!("capture opened but delivered no frame: {d}"), at),
        CaptureFacts::Frame {
            decoded: Ok(m),
            degraded: None,
            detail,
        } => v(
            id,
            State::Operative,
            format!("captured the probe window and decoded its calibration marker (session {}): {detail}", m.session),
            at,
        ),
        CaptureFacts::Frame {
            decoded: Ok(_),
            degraded: Some(d),
            detail,
        } => v(id, State::Degraded, format!("{d}; {detail}"), at),
        CaptureFacts::Frame {
            decoded: Err(why),
            detail,
            ..
        } => v(
            id,
            State::Degraded,
            format!("frames arrive but the probe window's calibration marker did not decode ({why}): colour management, scaling or overlap is altering pixels; {detail}"),
            at,
        ),
    }
}

pub fn input_verdict(f: &InputFacts, session: &MechanismVerdict, at: &str) -> MechanismVerdict {
    if let Err(why) = &f.supported {
        return v(INPUT_SAFE, State::Inoperative, why.clone(), at);
    }
    if session.state != State::Operative {
        let state = match session.state {
            State::Degraded => State::Degraded,
            State::Unknown => State::Unknown,
            _ => State::Inoperative,
        };
        return v(
            INPUT_SAFE,
            state,
            format!(
                "the input desktop is not established ({}): {}",
                session.state.as_str(),
                session.reason
            ),
            at,
        );
    }
    match (&f.listener, &f.injector) {
        (Ok(l), Ok(i)) => v(
            INPUT_SAFE,
            State::Operative,
            format!("{l}; {i}; the per-run key round trip is proven by `selftest`"),
            at,
        ),
        (Err(e), _) => v(
            INPUT_SAFE,
            State::Inoperative,
            format!("the real-input listener cannot run, so real input could not be told from the probe's: {e}"),
            at,
        ),
        (Ok(_), Err(e)) => v(INPUT_SAFE, State::Inoperative, format!("no injector: {e}"), at),
    }
}

pub fn clock_verdict(f: &ClockFacts, at: &str) -> MechanismVerdict {
    match f {
        ClockFacts::NoNode(d) => v(
            CLOCK_ALIGNED,
            State::Unknown,
            format!("node could not be run, so the generator clock cannot be compared: {d}"),
            at,
        ),
        ClockFacts::Failed(d) => v(CLOCK_ALIGNED, State::Unknown, format!("the round trip failed: {d}"), at),
        ClockFacts::Measured(c) if c.aligned => v(
            CLOCK_ALIGNED,
            State::Operative,
            format!(
                "process.hrtime and the {} clock share a timebase: offset {:.3} ms within a {:.3} ms round trip ({} samples)",
                clock::clock_name(),
                c.offset_ns as f64 / 1e6,
                c.rtt_ns as f64 / 1e6,
                c.samples
            ),
            at,
        ),
        ClockFacts::Measured(c) => v(
            CLOCK_ALIGNED,
            State::Inoperative,
            format!(
                "process.hrtime is NOT on the {} timebase: offset {:.3} ms exceeds the {:.3} ms round trip — every latency would be wrong",
                clock::clock_name(),
                c.offset_ns as f64 / 1e6,
                c.rtt_ns as f64 / 1e6
            ),
            at,
        ),
    }
}

pub fn gpu_verdict(f: &GpuFacts, at: &str) -> MechanismVerdict {
    match f {
        GpuFacts::NotApplicable(d) => v(COUNTERS_GPU, State::Inoperative, d.clone(), at),
        GpuFacts::Absent(d) => v(COUNTERS_GPU, State::Inoperative, d.clone(), at),
        GpuFacts::Failed(d) => v(COUNTERS_GPU, State::Unknown, d.clone(), at),
        GpuFacts::Instances(0) => v(
            COUNTERS_GPU,
            State::Degraded,
            "the GPU Engine counter set exists but lists no instances",
            at,
        ),
        GpuFacts::Instances(n) => v(
            COUNTERS_GPU,
            State::Operative,
            format!("{n} GPU engine counter instances"),
            at,
        ),
    }
}

/// Is the node round trip consistent with one timebase? The offset of node's
/// reading from the midpoint of the probe's send/receive times can be at most
/// half the round trip if both read the same clock; 1 ms of slack absorbs
/// scheduling noise.
pub fn alignment(offset_ns: i64, rtt_ns: u64, samples: u32) -> ClockAlignment {
    let bound = rtt_ns as i64 / 2 + 1_000_000;
    ClockAlignment {
        offset_ns,
        rtt_ns,
        samples,
        aligned: offset_ns.abs() <= bound,
    }
}

/// Every verdict, in report order.
pub fn verdicts(f: &Facts, at: &str) -> Vec<MechanismVerdict> {
    let session = session_verdict(&f.session, at);
    let input = input_verdict(&f.input, &session, at);
    vec![
        session,
        capture_verdict(CAPTURE_WGC, &f.wgc, at),
        capture_verdict(CAPTURE_DDA, &f.dda, at),
        capture_verdict(CAPTURE_X11, &f.x11, at),
        input,
        clock_verdict(&f.clock, at),
        gpu_verdict(&f.gpu, at),
    ]
}

/// The one-line `doctor` record.
pub fn report(f: &Facts) -> DoctorRecord {
    let at = clock::wall_rfc3339();
    let mechanisms = verdicts(f, &at);
    let mut summary = DoctorSummary::default();
    for m in &mechanisms {
        match m.state {
            State::Operative => summary.operative += 1,
            State::Degraded => summary.degraded += 1,
            State::Inoperative => summary.inoperative += 1,
            State::Unknown => summary.unknown += 1,
        }
    }
    DoctorRecord {
        host: f.host.clone(),
        os: f.os.clone(),
        checked_at: at,
        mode: "full",
        codec: spec().clone(),
        mechanisms,
        summary,
        violations: f
            .foreground_violation
            .iter()
            .map(|v| format!("D9: {v}"))
            .collect(),
    }
}

/// The state of one mechanism in a report.
pub fn state_of(r: &DoctorRecord, id: &str) -> State {
    r.mechanisms
        .iter()
        .find(|m| m.id == id)
        .map_or(State::Unknown, |m| m.state)
}

/// Whether the selftest GATES its checks on mechanism `id`: whether such a
/// check failing, or being unable to run, fails the selftest.
///
/// - OPERATIVE and DEGRADED gate. The doctor saw the mechanism work here, or
///   work reduced — and a capture whose frames arrive but whose marker does
///   not decode is exactly the decoder regression a selftest exists to catch.
/// - INOPERATIVE-ON-THIS-MACHINE never gates: a hard dependency is absent.
/// - UNKNOWN gates in exactly one case: a capture source that OPENED in an
///   OPERATIVE interactive session and then produced no frame, or failed
///   before one. The environment demonstrably offers that API there, so the
///   likeliest cause is the probe, and a probe regression must not pass as an
///   environment limit. Every other UNKNOWN — a capture source that never
///   opened, a GPU counter query that errored, a session or clock probe that
///   could not run — is the environment's to decide: its checks are recorded
///   with the reason, ungated, and `--require` makes a mechanism mandatory
///   where a run must have it.
pub fn gates(f: &Facts, r: &DoctorRecord, id: &str) -> bool {
    match state_of(r, id) {
        State::Operative | State::Degraded => true,
        State::Inoperative => false,
        State::Unknown => {
            let capture = match id {
                CAPTURE_WGC => &f.wgc,
                CAPTURE_DDA => &f.dda,
                CAPTURE_X11 => &f.x11,
                _ => return false,
            };
            matches!(capture, CaptureFacts::Failed(_) | CaptureFacts::NoFrame(_))
                && state_of(r, SESSION_INTERACTIVE) == State::Operative
        }
    }
}

/// A human rendering (without `--json`).
pub fn render_human(r: &DoctorRecord) -> String {
    let mut s = format!(
        "termhost-probe doctor: {} ({}) at {}\n\nMECHANISMS:\n",
        r.host, r.os, r.checked_at
    );
    for m in &r.mechanisms {
        s.push_str(&format!(
            "  {:<22} {:<28} {}\n",
            m.id,
            m.state.as_str(),
            m.reason
        ));
    }
    s.push_str(&format!(
        "\nSUMMARY: {} INOPERATIVE-ON-THIS-MACHINE, {} DEGRADED, {} OPERATIVE, {} UNKNOWN (of {} mechanisms)\n",
        r.summary.inoperative,
        r.summary.degraded,
        r.summary.operative,
        r.summary.unknown,
        r.mechanisms.len()
    ));
    s.push_str(&format!(
        "Marker decoder: codec v{} pixel rules from marker-golden.json (see `codec` in --json).\n",
        spec().version
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::MarkerKind;

    const AT: &str = "2026-10-11T00:00:00.000Z";

    fn ok_session() -> SessionFacts {
        SessionFacts {
            supported: Ok(()),
            state: Ok("WinSta0\\Default, session 1".into()),
        }
    }

    #[test]
    fn session_states_map_to_the_vocabulary() {
        assert_eq!(session_verdict(&ok_session(), AT).state, State::Operative);
        let locked = SessionFacts {
            supported: Ok(()),
            state: Err(SessionProblem::Unavailable(
                "input desktop is Winlogon".into(),
            )),
        };
        assert_eq!(session_verdict(&locked, AT).state, State::Degraded);
        let service = SessionFacts {
            supported: Ok(()),
            state: Err(SessionProblem::NotInteractive("session 0".into())),
        };
        assert_eq!(session_verdict(&service, AT).state, State::Inoperative);
        let failed = SessionFacts {
            supported: Ok(()),
            state: Err(SessionProblem::Unknown("OpenInputDesktop: 5".into())),
        };
        assert_eq!(session_verdict(&failed, AT).state, State::Unknown);
    }

    #[test]
    fn capture_is_operative_only_when_a_frame_decoded() {
        let m = Marker::new(MarkerKind::Calibration, 0, 0);
        let decoded = CaptureFacts::Frame {
            decoded: Ok(m),
            degraded: None,
            detail: "1 frame".into(),
        };
        assert_eq!(
            capture_verdict(CAPTURE_WGC, &decoded, AT).state,
            State::Operative
        );
        let undecoded = CaptureFacts::Frame {
            decoded: Err("off_palette".into()),
            degraded: None,
            detail: String::new(),
        };
        assert_eq!(
            capture_verdict(CAPTURE_WGC, &undecoded, AT).state,
            State::Degraded
        );
        let no_shm = CaptureFacts::Frame {
            decoded: Ok(m),
            degraded: Some("no MIT-SHM".into()),
            detail: String::new(),
        };
        assert_eq!(
            capture_verdict(CAPTURE_X11, &no_shm, AT).state,
            State::Degraded
        );
        // A hard dependency absent: inoperative here.
        for f in [
            CaptureFacts::NotApplicable("linux".into()),
            CaptureFacts::NoSession("x".into()),
            CaptureFacts::Unavailable("IsSupported false".into()),
        ] {
            assert_eq!(
                capture_verdict(CAPTURE_DDA, &f, AT).state,
                State::Inoperative,
                "{f:?}"
            );
        }
        // The probe got no readable answer: unknown, never inoperative.
        for f in [
            CaptureFacts::OpenFailed("DuplicateOutput: 0x887A0004".into()),
            CaptureFacts::Failed("0x887A0004".into()),
            CaptureFacts::NoFrame("2000 ms".into()),
        ] {
            assert_eq!(
                capture_verdict(CAPTURE_DDA, &f, AT).state,
                State::Unknown,
                "{f:?}"
            );
        }
    }

    #[test]
    fn input_safety_needs_the_session_and_both_halves() {
        let session = session_verdict(&ok_session(), AT);
        let good = InputFacts {
            supported: Ok(()),
            listener: Ok("raw input registered".into()),
            injector: Ok("SendInput".into()),
        };
        assert_eq!(input_verdict(&good, &session, AT).state, State::Operative);
        let no_listener = InputFacts {
            listener: Err("RegisterRawInputDevices failed".into()),
            ..good.clone()
        };
        assert_eq!(
            input_verdict(&no_listener, &session, AT).state,
            State::Inoperative
        );
        let locked = session_verdict(
            &SessionFacts {
                supported: Ok(()),
                state: Err(SessionProblem::Unavailable("locked".into())),
            },
            AT,
        );
        assert_eq!(input_verdict(&good, &locked, AT).state, State::Degraded);
    }

    #[test]
    fn clock_alignment_is_measured_not_assumed() {
        assert!(alignment(200_000, 300_000, 40).aligned);
        assert!(alignment(-1_100_000, 300_000, 40).aligned);
        assert!(!alignment(5_000_000_000, 300_000, 40).aligned);
        let missing = clock_verdict(&ClockFacts::NoNode("not on PATH".into()), AT);
        assert_eq!(missing.state, State::Unknown);
        let off = clock_verdict(&ClockFacts::Measured(alignment(9_000_000, 100_000, 40)), AT);
        assert_eq!(off.state, State::Inoperative);
    }

    #[test]
    fn gpu_counters_map() {
        assert_eq!(
            gpu_verdict(&GpuFacts::Instances(12), AT).state,
            State::Operative
        );
        assert_eq!(
            gpu_verdict(&GpuFacts::Instances(0), AT).state,
            State::Degraded
        );
        assert_eq!(
            gpu_verdict(&GpuFacts::Absent("no object".into()), AT).state,
            State::Inoperative
        );
    }

    fn unknown_everywhere() -> Facts {
        Facts {
            host: "h".into(),
            os: "windows".into(),
            session: ok_session(),
            wgc: CaptureFacts::NoFrame("no frame within 2000 ms".into()),
            dda: CaptureFacts::OpenFailed("DuplicateOutput: 0x887A0004".into()),
            x11: CaptureFacts::NotApplicable("windows".into()),
            input: InputFacts {
                supported: Ok(()),
                listener: Ok("l".into()),
                injector: Ok("i".into()),
            },
            clock: ClockFacts::NoNode("not on PATH".into()),
            gpu: GpuFacts::Failed("PdhCollectQueryData: 0x800007d5".into()),
            foreground_violation: None,
        }
    }

    #[test]
    fn only_a_capture_that_opened_in_an_operative_session_gates_while_unknown() {
        let f = unknown_everywhere();
        let r = report(&f);
        for id in [CAPTURE_WGC, CAPTURE_DDA, CLOCK_ALIGNED, COUNTERS_GPU] {
            assert_eq!(state_of(&r, id), State::Unknown, "{id}");
        }
        // The S2 regression guard: WGC opened in an operative session and
        // produced no frame — the probe is the likeliest cause.
        assert!(gates(&f, &r, CAPTURE_WGC));
        // The environment's to decide: a source that never opened, a GPU
        // counter query that errored, a node that could not run.
        assert!(!gates(&f, &r, CAPTURE_DDA), "never opened");
        assert!(!gates(&f, &r, COUNTERS_GPU), "a PDH error");
        assert!(!gates(&f, &r, CLOCK_ALIGNED), "node could not run");
        assert!(!gates(&f, &r, CAPTURE_X11), "inoperative");
        assert!(gates(&f, &r, SESSION_INTERACTIVE), "operative");
        assert!(gates(&f, &r, INPUT_SAFE), "operative");
        // Opened, then failed before a frame: gated too.
        let failed = Facts {
            wgc: CaptureFacts::Failed("the capture ended".into()),
            ..f.clone()
        };
        assert!(gates(&failed, &report(&failed), CAPTURE_WGC));
        // ... but not in a session the doctor could not establish.
        let session_unknown = Facts {
            session: SessionFacts {
                supported: Ok(()),
                state: Err(SessionProblem::Unknown("OpenInputDesktop: 5".into())),
            },
            ..f.clone()
        };
        assert!(!gates(
            &session_unknown,
            &report(&session_unknown),
            CAPTURE_WGC
        ));
        // DEGRADED gates: frames arrive but the marker does not decode.
        let undecoded = Facts {
            wgc: CaptureFacts::Frame {
                decoded: Err("off_palette".into()),
                degraded: None,
                detail: String::new(),
            },
            ..f.clone()
        };
        assert!(gates(&undecoded, &report(&undecoded), CAPTURE_WGC));
    }

    #[test]
    fn a_foreground_change_around_the_probe_is_a_recorded_d9_violation() {
        let f = Facts {
            foreground_violation: Some("0x10 before, 0x20 after".into()),
            ..unknown_everywhere()
        };
        let r = report(&f);
        assert_eq!(
            r.violations,
            vec!["D9: 0x10 before, 0x20 after".to_string()]
        );
        let line = crate::records::to_line(&crate::records::Record::Doctor(r));
        assert!(line.contains("\"violations\":[\"D9: "), "{line}");
    }

    #[test]
    fn the_report_lists_every_mechanism_once_and_counts_them() {
        let f = Facts {
            host: "h".into(),
            os: "windows".into(),
            session: ok_session(),
            wgc: CaptureFacts::NoFrame("x".into()),
            dda: CaptureFacts::Unavailable("y".into()),
            x11: CaptureFacts::NotApplicable("windows".into()),
            input: InputFacts {
                supported: Ok(()),
                listener: Ok("l".into()),
                injector: Ok("i".into()),
            },
            clock: ClockFacts::NoNode("n".into()),
            gpu: GpuFacts::Instances(3),
            foreground_violation: None,
        };
        let r = report(&f);
        let ids: Vec<&str> = r.mechanisms.iter().map(|m| m.id).collect();
        let want: Vec<&str> = MECHANISMS.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, want);
        assert_eq!(
            (
                r.summary.operative,
                r.summary.inoperative,
                r.summary.unknown,
                r.summary.degraded
            ),
            (3, 2, 2, 0)
        );
        assert!(r.violations.is_empty());
        let line = crate::records::to_line(&crate::records::Record::Doctor(r));
        assert!(line.contains("\"INOPERATIVE-ON-THIS-MACHINE\""));
    }
}
