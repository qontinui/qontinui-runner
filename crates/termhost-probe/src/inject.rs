//! The input arm (plan D9). A key is sent only while the probe ESTABLISHES,
//! immediately before that key, that:
//!
//! 1. no real input has occurred since the arm began — the non-blocking
//!    real-input listener (Raw Input / XInput2; never a low-level keyboard
//!    hook, which would add latency to every keystroke) reports only the
//!    probe's own tagged events;
//! 2. the listener is healthy (an unheard operator is not an absent one);
//! 3. the target window is foreground on an interactive, unlocked desktop.
//!
//! Any failure aborts the arm at once, before the key — the arm fails closed,
//! because an injected keystroke whose target has not been established is
//! destructive (policy operating-rules/what-makes-an-action-destructive). The
//! foreground is checked again right after each key: a change in that window
//! means the key may have reached another window, which is reported, never
//! smoothed over.

use std::time::Duration;

use crate::clock::{now_ns, sleep_until_ns, wall_rfc3339};
use crate::model::{InputDevice, InputOrigin};
use crate::platform::{stop_requested, InjectBlock, KeyInjector, Subscription};
use crate::records::{IncidentRecord, InjectAbortRecord, InjectRecord, Ns, Record, Sink};

/// One arm.
#[derive(Clone, Debug, PartialEq)]
pub struct ArmConfig {
    pub keys: Vec<char>,
    pub interval: Duration,
    /// Before the first key.
    pub start_delay: Duration,
    /// After the last key, still listening (late real input is reported).
    pub settle: Duration,
    pub target: String,
}

/// Why an arm stopped early.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbortReason {
    NotForeground,
    DesktopUnavailable,
    TargetCheckFailed,
    OperatorInput,
    ForeignInjection,
    MonitorDown,
    SendFailed,
    ForegroundChangedDuringInjection,
    StopRequested,
}

impl AbortReason {
    pub fn as_str(self) -> &'static str {
        match self {
            AbortReason::NotForeground => "not_foreground",
            AbortReason::DesktopUnavailable => "desktop_unavailable",
            AbortReason::TargetCheckFailed => "target_check_failed",
            AbortReason::OperatorInput => "operator_input",
            AbortReason::ForeignInjection => "foreign_injection",
            AbortReason::MonitorDown => "monitor_down",
            AbortReason::SendFailed => "send_failed",
            AbortReason::ForegroundChangedDuringInjection => "foreground_changed_during_injection",
            AbortReason::StopRequested => "stop_requested",
        }
    }

    /// Real input (or someone else's injection) aborted the arm.
    pub fn is_operator_activity(self) -> bool {
        matches!(
            self,
            AbortReason::OperatorInput | AbortReason::ForeignInjection
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ArmOutcome {
    Completed {
        sent: u32,
        /// Non-probe input heard during the settle period after the last key.
        late_real_input: u32,
    },
    Aborted {
        index: u32,
        reason: AbortReason,
        detail: String,
        sent: u32,
    },
}

impl ArmOutcome {
    pub fn sent(&self) -> u32 {
        match self {
            ArmOutcome::Completed { sent, .. } | ArmOutcome::Aborted { sent, .. } => *sent,
        }
    }
}

fn device_name(d: InputDevice) -> &'static str {
    match d {
        InputDevice::Keyboard => "keyboard",
        InputDevice::Pointer => "pointer",
        InputDevice::Other => "other device",
    }
}

/// Non-probe input since the last drain, as an abort reason.
fn foreign(input: &mut Subscription) -> Option<(AbortReason, String)> {
    let events = input.drain();
    let real = events.iter().find(|e| e.origin == InputOrigin::Real);
    if let Some(e) = real {
        return Some((
            AbortReason::OperatorInput,
            format!("real {} input at t={} ns", device_name(e.device), e.t_ns),
        ));
    }
    events
        .iter()
        .find(|e| e.origin == InputOrigin::ForeignInjected)
        .map(|e| {
            (
                AbortReason::ForeignInjection,
                format!(
                    "{} input injected WITHOUT this run's tag at t={} ns — not the probe's, so treated as operator activity",
                    device_name(e.device),
                    e.t_ns
                ),
            )
        })
}

/// Run one arm. `after_key(index, t_inject_ns, injector)` runs after each key
/// is sent and checked (the selftest uses it to expect echoes and to send its
/// one untagged key).
pub fn run_arm(
    cfg: &ArmConfig,
    inj: &mut dyn KeyInjector,
    input: &mut Subscription,
    sink: &mut Sink,
    mut after_key: impl FnMut(u32, u64, &mut dyn KeyInjector),
) -> ArmOutcome {
    // Input before the arm is not this arm's concern.
    let _ = input.drain();
    let start = now_ns() + cfg.start_delay.as_nanos() as u64;
    let interval = cfg.interval.as_nanos() as u64;
    let mut sent = 0u32;
    let abort = |sink: &mut Sink, index: u32, reason: AbortReason, detail: String, sent: u32| {
        sink.emit(&Record::InjectAbort(InjectAbortRecord {
            index,
            reason: reason.as_str(),
            detail: detail.clone(),
            keys_sent: sent,
        }));
        if reason.is_operator_activity() || reason == AbortReason::ForegroundChangedDuringInjection
        {
            sink.emit(&Record::Incident(IncidentRecord {
                kind: if reason.is_operator_activity() {
                    "operator_input"
                } else {
                    "harness_error"
                },
                at: wall_rfc3339(),
                detail: format!("input arm aborted at key {index}: {detail}"),
                stops_run: false,
                source: None,
            }));
        }
        ArmOutcome::Aborted {
            index,
            reason,
            detail,
            sent,
        }
    };
    for (i, &ch) in cfg.keys.iter().enumerate() {
        let i = i as u32;
        sleep_until_ns(start + u64::from(i) * interval);
        if stop_requested() {
            return abort(
                sink,
                i,
                AbortReason::StopRequested,
                "stop requested".into(),
                sent,
            );
        }
        if let Some((reason, detail)) = foreign(input) {
            return abort(sink, i, reason, detail, sent);
        }
        if let Err(why) = input.health() {
            return abort(sink, i, AbortReason::MonitorDown, why, sent);
        }
        if let Err(block) = inj.check_target() {
            let reason = match &block {
                InjectBlock::NotForeground(_) => AbortReason::NotForeground,
                InjectBlock::DesktopUnavailable(_) => AbortReason::DesktopUnavailable,
                InjectBlock::Unknown(_) => AbortReason::TargetCheckFailed,
            };
            return abort(sink, i, reason, block.to_string(), sent);
        }
        let t_check = now_ns();
        let t_inject = now_ns();
        if let Err(why) = inj.send_char(ch, true) {
            return abort(sink, i, AbortReason::SendFailed, why, sent);
        }
        sent += 1;
        if let Err(block) = inj.check_target() {
            return abort(
                sink,
                i,
                AbortReason::ForegroundChangedDuringInjection,
                format!("the foreground changed while key {i} was being sent, so it may have reached another window: {block}"),
                sent,
            );
        }
        sink.emit(&Record::Inject(InjectRecord {
            index: i,
            key: ch.to_string(),
            t_inject_ns: Ns(t_inject),
            t_check_ns: Ns(t_check),
            tagged: true,
            target: cfg.target.clone(),
        }));
        after_key(i, t_inject, inj);
    }
    std::thread::sleep(cfg.settle);
    let late = input
        .drain()
        .iter()
        .filter(|e| e.origin != InputOrigin::Probe)
        .count() as u32;
    ArmOutcome::Completed {
        sent,
        late_real_input: late,
    }
}

/// The keys an arm types: printable letters only — every one answers with one
/// echo byte, and none is Ctrl+C (which stops the generator).
pub fn letters(n: usize) -> Vec<char> {
    (0..n).map(|i| (b'a' + (i % 26) as u8) as char).collect()
}

/// Exit code for an arm that was refused or aborted for safety (nothing was
/// typed after the condition was seen).
pub const EXIT_ABORTED: i32 = 3;

/// The `inject` subcommand.
#[derive(Clone, Debug)]
pub struct InjectCmd {
    pub run_dir: std::path::PathBuf,
    pub target: crate::platform::TargetSpec,
    pub keys: Vec<char>,
    pub interval: Duration,
    pub start_delay: Duration,
    pub settle: Duration,
    pub tag: Option<u32>,
    pub activate: bool,
    pub echo_stdout: bool,
    pub args: Vec<String>,
}

/// Run `inject`: writes `<run-dir>/inject.jsonl`; exit 0 = every key sent,
/// [`EXIT_ABORTED`] = refused or aborted for safety, 1 = setup error.
pub fn run_cmd(host: &mut dyn crate::platform::Host, cmd: &InjectCmd) -> anyhow::Result<i32> {
    use crate::records::{EndRecord, Ns};
    use anyhow::Context;

    std::fs::create_dir_all(&cmd.run_dir)
        .with_context(|| format!("creating {}", cmd.run_dir.display()))?;
    let path = cmd.run_dir.join("inject.jsonl");
    let mut sink = Sink::to_file(&path, cmd.echo_stdout)
        .with_context(|| format!("opening {}", path.display()))?;
    let mut tagging = crate::platform::Tagging::new();
    if let Some(t) = cmd.tag {
        tagging.tag = t;
    }
    sink.emit(&crate::watch::start_record(
        "inject",
        &cmd.args,
        Some(tagging.tag),
    ));
    let finish = |sink: &mut Sink, code: i32, reason: String| {
        let end = Record::End(EndRecord {
            t_ns: Ns(now_ns()),
            exit_code: code,
            reason,
        });
        sink.emit(&end);
        sink.flush();
        if !cmd.echo_stdout {
            println!("{}", crate::records::to_line(&end));
        }
        Ok(code)
    };
    let target = match host.resolve_target(&cmd.target) {
        Ok(t) => t,
        Err(why) => return finish(&mut sink, 1, format!("target window not found: {why}")),
    };
    let monitor = match host.input_monitor(&tagging) {
        Ok(m) => m,
        Err(why) => {
            return finish(
                &mut sink,
                EXIT_ABORTED,
                format!("refused: the real-input listener could not start, so real input could not be detected (D9) — nothing was typed: {why}"),
            )
        }
    };
    let mut inj = match host.injector(target, &tagging) {
        Ok(i) => i,
        Err(why) => {
            return finish(
                &mut sink,
                EXIT_ABORTED,
                format!("refused: no injector — nothing was typed: {why}"),
            )
        }
    };
    if cmd.activate {
        if let Err(why) = inj.activate() {
            sink.emit(&Record::Incident(IncidentRecord {
                kind: "harness_error",
                at: wall_rfc3339(),
                detail: format!("--activate could not make the target foreground: {why}"),
                stops_run: false,
                source: None,
            }));
        }
    }
    let mut sub = monitor.bus.subscribe();
    let arm = ArmConfig {
        keys: cmd.keys.clone(),
        interval: cmd.interval,
        start_delay: cmd.start_delay,
        settle: cmd.settle,
        target: target.to_string(),
    };
    let outcome = run_arm(&arm, inj.as_mut(), &mut sub, &mut sink, |_, _, _| {});
    match outcome {
        ArmOutcome::Completed { sent, late_real_input } => finish(
            &mut sink,
            0,
            format!("completed: {sent} keys sent; {late_real_input} non-probe input event(s) during the settle period"),
        ),
        ArmOutcome::Aborted {
            index,
            reason,
            detail,
            sent,
        } => finish(
            &mut sink,
            EXIT_ABORTED,
            format!("aborted at key {index} ({}): {detail}; {sent} keys were sent before", reason.as_str()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::InputObs;
    use crate::platform::InputBus;
    use std::sync::{Arc, Mutex};

    /// An injector whose target is foreground until `lose_focus_at`, and which
    /// publishes its own (tagged or untagged) events to the bus like a real
    /// listener would.
    struct FakeInjector {
        bus: Arc<InputBus>,
        lose_focus_at: Option<u32>,
        sent: Arc<Mutex<Vec<(char, bool)>>>,
    }

    impl KeyInjector for FakeInjector {
        fn check_target(&mut self) -> Result<(), InjectBlock> {
            let n = self
                .sent
                .lock()
                .expect("sent")
                .iter()
                .filter(|(_, t)| *t)
                .count() as u32;
            match self.lose_focus_at {
                Some(at) if n >= at => Err(InjectBlock::NotForeground("another window".into())),
                _ => Ok(()),
            }
        }
        fn send_char(&mut self, ch: char, tagged: bool) -> Result<(), String> {
            self.sent.lock().expect("sent").push((ch, tagged));
            for _ in 0..2 {
                self.bus.publish(InputObs {
                    t_ns: now_ns(),
                    device: InputDevice::Keyboard,
                    origin: if tagged {
                        InputOrigin::Probe
                    } else {
                        InputOrigin::ForeignInjected
                    },
                });
            }
            Ok(())
        }
        fn activate(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn describe(&self) -> String {
            "fake".into()
        }
    }

    fn cfg(n: usize) -> ArmConfig {
        ArmConfig {
            keys: letters(n),
            interval: Duration::from_millis(5),
            start_delay: Duration::ZERO,
            settle: Duration::from_millis(5),
            target: "fake".into(),
        }
    }

    type Sent = Arc<Mutex<Vec<(char, bool)>>>;

    fn setup(lose_focus_at: Option<u32>) -> (FakeInjector, Subscription, Arc<InputBus>, Sent) {
        let bus = InputBus::new();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let inj = FakeInjector {
            bus: bus.clone(),
            lose_focus_at,
            sent: sent.clone(),
        };
        let sub = bus.subscribe();
        (inj, sub, bus, sent)
    }

    #[test]
    fn a_clean_arm_sends_every_key_and_logs_t_inject() {
        let (mut inj, mut sub, _bus, sent) = setup(None);
        let (mut sink, lines) = Sink::memory();
        let out = run_arm(&cfg(6), &mut inj, &mut sub, &mut sink, |_, _, _| {});
        assert_eq!(
            out,
            ArmOutcome::Completed {
                sent: 6,
                late_real_input: 0
            }
        );
        assert_eq!(sent.lock().expect("sent").len(), 6);
        let lines = lines.lock().expect("lines");
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.contains("\"type\":\"inject\""))
                .count(),
            6
        );
        assert!(lines[0].contains("\"t_inject_ns\":\""));
    }

    #[test]
    fn real_input_aborts_before_the_next_key() {
        let (mut inj, mut sub, bus, sent) = setup(None);
        let (mut sink, lines) = Sink::memory();
        let out = run_arm(&cfg(8), &mut inj, &mut sub, &mut sink, |i, _, _| {
            if i == 2 {
                bus.publish(InputObs {
                    t_ns: now_ns(),
                    device: InputDevice::Pointer,
                    origin: InputOrigin::Real,
                });
            }
        });
        match out {
            ArmOutcome::Aborted {
                index,
                reason,
                sent: n,
                ..
            } => {
                assert_eq!((index, reason, n), (3, AbortReason::OperatorInput, 3));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(sent.lock().expect("sent").len(), 3);
        assert!(lines
            .lock()
            .expect("lines")
            .iter()
            .any(|l| l.contains("\"kind\":\"operator_input\"")));
    }

    #[test]
    fn an_untagged_injection_is_not_ours_and_aborts() {
        let (mut inj, mut sub, _bus, sent) = setup(None);
        let (mut sink, _) = Sink::memory();
        let out = run_arm(&cfg(10), &mut inj, &mut sub, &mut sink, |i, _, inj| {
            if i == 1 {
                inj.send_char('z', false).expect("send");
            }
        });
        match out {
            ArmOutcome::Aborted {
                index,
                reason,
                sent: n,
                ..
            } => {
                assert_eq!((index, reason, n), (2, AbortReason::ForeignInjection, 2));
            }
            other => panic!("{other:?}"),
        }
        let tagged = sent
            .lock()
            .expect("sent")
            .iter()
            .filter(|(_, t)| *t)
            .count();
        assert_eq!(tagged, 2, "no tagged key after the untagged one");
    }

    #[test]
    fn a_target_that_is_not_foreground_gets_no_key_at_all() {
        let (mut inj, mut sub, _bus, sent) = setup(Some(0));
        let (mut sink, _) = Sink::memory();
        let out = run_arm(&cfg(10), &mut inj, &mut sub, &mut sink, |_, _, _| {});
        assert!(matches!(
            out,
            ArmOutcome::Aborted {
                index: 0,
                reason: AbortReason::NotForeground,
                sent: 0,
                ..
            }
        ));
        assert!(sent.lock().expect("sent").is_empty());
    }

    #[test]
    fn a_foreground_change_while_a_key_was_sent_is_reported() {
        let (mut inj, mut sub, _bus, sent) = setup(Some(4));
        let (mut sink, lines) = Sink::memory();
        let out = run_arm(&cfg(10), &mut inj, &mut sub, &mut sink, |_, _, _| {});
        match out {
            ArmOutcome::Aborted {
                index,
                reason,
                sent: n,
                ..
            } => {
                // Key 3 was sent, then the post-send check saw the change.
                assert_eq!(
                    (index, reason, n),
                    (3, AbortReason::ForegroundChangedDuringInjection, 4)
                );
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(sent.lock().expect("sent").len(), 4);
        assert!(lines
            .lock()
            .expect("lines")
            .iter()
            .any(|l| l.contains("\"type\":\"incident\"")));
    }

    #[test]
    fn a_failed_listener_aborts() {
        let (mut inj, mut sub, bus, _) = setup(None);
        bus.set_health(Some("raw input window destroyed".into()));
        let (mut sink, _) = Sink::memory();
        let out = run_arm(&cfg(3), &mut inj, &mut sub, &mut sink, |_, _, _| {});
        assert!(matches!(
            out,
            ArmOutcome::Aborted {
                reason: AbortReason::MonitorDown,
                sent: 0,
                ..
            }
        ));
    }

    #[test]
    fn letters_never_include_control_characters() {
        assert_eq!(
            letters(28).iter().collect::<String>(),
            "abcdefghijklmnopqrstuvwxyzab"
        );
    }
}
