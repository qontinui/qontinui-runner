//! The input arm (plan D9). A key is sent only while the probe ESTABLISHES,
//! immediately before that key, that:
//!
//! 1. no real input has occurred since the probe started listening — the
//!    non-blocking real-input listener (Raw Input / XInput2; never a
//!    low-level keyboard hook, which would add latency to every keystroke)
//!    reports only the probe's own tagged events;
//! 2. the listener is healthy AND hears: after every key it must have
//!    recognised that key's own events (an unheard operator is not an absent
//!    one, and a listener that stopped hearing the probe would not hear the
//!    operator either);
//! 3. the target window is foreground on an interactive, unlocked desktop.
//!    On X11 that is the keyboard focus. One redirection stays undetected: a
//!    keyboard grab held by another client takes XTEST's keys whatever the
//!    focus says, and probing for one would itself disturb the target —
//!    accepted on the benchmark's private Xvfb, where nothing grabs (see
//!    `platform::common::x11_focus_verdict`).
//!
//! Before anything happens — before `--activate` moves focus, before the
//! first key — the desktop must have been quiet for a while (`--quiet-ms`),
//! and every key of the arm must be typeable, so an arm never starts on a
//! desktop in use and never stops half-typed for a key it could not send.
//!
//! Any failure aborts the arm at once, before the key — the arm fails closed,
//! because an injected keystroke whose target has not been established is
//! destructive (policy operating-rules/what-makes-an-action-destructive). The
//! foreground is checked again right after each key: a change in that window
//! means the key may have reached another window, which is reported, never
//! smoothed over. Each key is announced in an `inject_intent` record (flushed)
//! BEFORE it is sent, so a `watch` in another process recognises it.

use std::time::Duration;

use crate::clock::{now_ns, sleep_until_ns};
use crate::model::{InputDevice, InputOrigin};
use crate::platform::{stop_requested, InjectBlock, KeyInjector, Subscription};
use crate::records::{
    IncidentRecord, InjectAbortRecord, InjectIntentRecord, InjectRecord, Ns, Record, Sink,
};

/// The listener must recognise a key's own events within this long.
pub const HEARD_WITHIN: Duration = Duration::from_millis(250);

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
    /// The listener did not recognise the probe's own key in time.
    MonitorDeaf,
    /// A key of the arm cannot be typed on this platform.
    UnsupportedKey,
    /// A modifier is held or CapsLock is on: the key would not type what is
    /// intended, or would become a shortcut.
    KeyboardState,
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
            AbortReason::MonitorDeaf => "monitor_deaf",
            AbortReason::UnsupportedKey => "unsupported_key",
            AbortReason::KeyboardState => "keyboard_state",
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

/// Require `quiet` with no real or foreign input — and a healthy listener —
/// before anything happens on the desktop. `input` must have been subscribed
/// before this is called; events already queued count.
pub fn await_quiet(input: &mut Subscription, quiet: Duration) -> Result<(), (AbortReason, String)> {
    let until = now_ns() + quiet.as_nanos() as u64;
    loop {
        if let Some((reason, detail)) = foreign(input) {
            return Err((
                reason,
                format!(
                    "the desktop was in use within the {} ms quiet period: {detail}",
                    quiet.as_millis()
                ),
            ));
        }
        if let Err(why) = input.health() {
            return Err((AbortReason::MonitorDown, why));
        }
        if stop_requested() {
            return Err((AbortReason::StopRequested, "stop requested".into()));
        }
        let now = now_ns();
        if now >= until {
            return Ok(());
        }
        std::thread::sleep(Duration::from_nanos((until - now).min(20_000_000)));
    }
}

/// Emit the abort (and, for operator activity or a misdirected key, an
/// incident) and build the outcome.
fn abort(
    sink: &mut Sink,
    index: u32,
    reason: AbortReason,
    detail: String,
    sent: u32,
) -> ArmOutcome {
    sink.emit(&Record::InjectAbort(InjectAbortRecord {
        index,
        reason: reason.as_str(),
        detail: detail.clone(),
        keys_sent: sent,
    }));
    if reason.is_operator_activity() || reason == AbortReason::ForegroundChangedDuringInjection {
        sink.emit(&Record::Incident(IncidentRecord::new(
            if reason.is_operator_activity() {
                "operator_input"
            } else {
                "harness_error"
            },
            format!("input arm aborted at key {index}: {detail}"),
            false,
        )));
    }
    sink.flush();
    ArmOutcome::Aborted {
        index,
        reason,
        detail,
        sent,
    }
}

/// The first key this injector cannot type, as (index, why).
fn untypeable(keys: &[char], inj: &dyn KeyInjector) -> Option<(u32, String)> {
    keys.iter()
        .enumerate()
        .find(|(_, &c)| !inj.can_send(c))
        .map(|(i, &ch)| {
            (
                i as u32,
                format!("key {i} ({ch:?}) cannot be typed here; nothing was sent"),
            )
        })
}

/// Run one arm. `input` should have been subscribed before anything that
/// precedes the arm (activation): input queued in it aborts the arm before
/// the first key. `after_key(index, t_inject_ns, injector)` runs after each
/// key is sent and checked (the selftest uses it to expect echoes and to send
/// its one untagged key). `inject` establishes typeability before it moves
/// focus; the arm checks it again, for callers that did not.
pub fn run_arm(
    cfg: &ArmConfig,
    inj: &mut dyn KeyInjector,
    input: &mut Subscription,
    sink: &mut Sink,
    mut after_key: impl FnMut(u32, u64, &mut dyn KeyInjector),
) -> ArmOutcome {
    if let Some((i, detail)) = untypeable(&cfg.keys, inj) {
        return abort(sink, i, AbortReason::UnsupportedKey, detail, 0);
    }
    inj.begin_arm();
    let start = now_ns() + cfg.start_delay.as_nanos() as u64;
    let interval = cfg.interval.as_nanos() as u64;
    let mut sent = 0u32;
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
                InjectBlock::KeyboardState(_) => AbortReason::KeyboardState,
            };
            return abort(sink, i, reason, block.to_string(), sent);
        }
        let t_check = now_ns();
        // Announce the key first: another process's listener (`watch`) reads
        // this to recognise the key as the probe's own when it arrives.
        sink.emit(&Record::InjectIntent(InjectIntentRecord {
            index: i,
            key: ch.to_string(),
            ledger: inj.ledger_ids(ch),
            t_ns: Ns(now_ns()),
        }));
        sink.flush();
        let heard_before = inj.heard();
        let t_inject = now_ns();
        if let Err(why) = inj.send_char(ch, true) {
            return abort(sink, i, AbortReason::SendFailed, why, sent);
        }
        sent += 1;
        if let Err(block) = inj.check_target() {
            // A modifier that went down while the key was sent is not a
            // focus change: report it as what it is.
            let (reason, what) = match &block {
                InjectBlock::KeyboardState(_) => (
                    AbortReason::KeyboardState,
                    "a modifier went down (or CapsLock on) while key {i} was being sent, so it may have typed something else or a shortcut",
                ),
                _ => (
                    AbortReason::ForegroundChangedDuringInjection,
                    "the foreground changed while key {i} was being sent, so it may have reached another window",
                ),
            };
            return abort(
                sink,
                i,
                reason,
                format!("{}: {block}", what.replace("{i}", &i.to_string())),
                sent,
            );
        }
        let deadline = now_ns() + HEARD_WITHIN.as_nanos() as u64;
        while inj.heard() < heard_before + 2 {
            if now_ns() >= deadline {
                return abort(
                    sink,
                    i,
                    AbortReason::MonitorDeaf,
                    format!(
                        "the real-input listener did not recognise key {i}'s own events within {} ms; a listener that cannot hear the probe would not hear an operator either",
                        HEARD_WITHIN.as_millis()
                    ),
                    sent,
                );
            }
            std::thread::sleep(Duration::from_millis(1));
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
    /// The desktop must be quiet this long before focus moves or a key goes.
    pub quiet: Duration,
    pub echo_stdout: bool,
    pub args: Vec<String>,
}

/// Run `inject`: writes `<run-dir>/inject.jsonl`; exit 0 = every key sent,
/// [`EXIT_ABORTED`] = refused or aborted for safety, 1 = setup error.
pub fn run_cmd(host: &mut dyn crate::platform::Host, cmd: &InjectCmd) -> anyhow::Result<i32> {
    use crate::records::{EndRecord, FileMode};
    use anyhow::Context;

    std::fs::create_dir_all(&cmd.run_dir)
        .with_context(|| format!("creating {}", cmd.run_dir.display()))?;
    let path = cmd.run_dir.join(crate::watch::INJECT_LOG);
    let mut sink = Sink::create(&path, cmd.echo_stdout, FileMode::New)
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
    // Listening starts before anything happens on the desktop.
    let mut sub = monitor.bus.subscribe();
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
    // Every key must be typeable before anything happens on the desktop: an
    // arm that moved focus and then refused would have disturbed the
    // operator's desktop for nothing.
    if let Some((i, detail)) = untypeable(&cmd.keys, inj.as_ref()) {
        let _refused = abort(&mut sink, i, AbortReason::UnsupportedKey, detail.clone(), 0);
        return finish(
            &mut sink,
            EXIT_ABORTED,
            format!("refused (unsupported_key) before any focus change or key: {detail}"),
        );
    }
    if let Err((reason, detail)) = await_quiet(&mut sub, cmd.quiet) {
        let _refused = abort(&mut sink, 0, reason, detail.clone(), 0);
        return finish(
            &mut sink,
            EXIT_ABORTED,
            format!(
                "refused ({}) before any focus change or key: {detail}",
                reason.as_str()
            ),
        );
    }
    if cmd.activate {
        if let Err(why) = inj.activate() {
            sink.emit(&Record::Incident(IncidentRecord::new(
                "harness_error",
                format!("--activate could not make the target foreground: {why}"),
                false,
            )));
        }
    }
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
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    /// An injector whose target is foreground until `lose_focus_at`, and which
    /// publishes its own (tagged or untagged) events to the bus like a real
    /// listener would — and counts them as heard unless `deaf`.
    struct FakeInjector {
        bus: Arc<InputBus>,
        lose_focus_at: Option<u32>,
        sent: Arc<Mutex<Vec<(char, bool)>>>,
        heard: Arc<AtomicU64>,
        deaf: bool,
        unsupported: Option<char>,
        /// A modifier is held from this key on (`check_target` refuses).
        keyboard_held_at: Option<u32>,
        /// The sink's lines at the moment each key was sent.
        lines: Option<Arc<Mutex<Vec<String>>>>,
        seen_at_send: Arc<Mutex<Vec<usize>>>,
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
            if self.keyboard_held_at.is_some_and(|at| n >= at) {
                return Err(InjectBlock::KeyboardState("Shift is held".into()));
            }
            match self.lose_focus_at {
                Some(at) if n >= at => Err(InjectBlock::NotForeground("another window".into())),
                _ => Ok(()),
            }
        }
        fn can_send(&self, ch: char) -> bool {
            self.unsupported != Some(ch)
        }
        fn ledger_ids(&self, ch: char) -> Vec<u32> {
            vec![ch as u32, ch as u32 | 0x100]
        }
        fn send_char(&mut self, ch: char, tagged: bool) -> Result<(), String> {
            if let Some(lines) = &self.lines {
                let n = lines
                    .lock()
                    .expect("lines")
                    .iter()
                    .filter(|l| l.contains("\"type\":\"inject_intent\""))
                    .count();
                self.seen_at_send.lock().expect("seen").push(n);
            }
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
                if tagged && !self.deaf {
                    self.heard.fetch_add(1, Ordering::Relaxed);
                }
            }
            Ok(())
        }
        fn heard(&self) -> u64 {
            self.heard.load(Ordering::Relaxed)
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
            heard: Arc::new(AtomicU64::new(0)),
            deaf: false,
            unsupported: None,
            keyboard_held_at: None,
            lines: None,
            seen_at_send: Arc::new(Mutex::new(Vec::new())),
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
        assert!(lines
            .iter()
            .any(|l| l.contains("\"type\":\"inject\"") && l.contains("\"t_inject_ns\":\"")));
    }

    #[test]
    fn every_key_is_announced_before_it_is_sent() {
        let (mut inj, mut sub, _bus, _) = setup(None);
        let (mut sink, lines) = Sink::memory();
        inj.lines = Some(lines.clone());
        let seen = inj.seen_at_send.clone();
        let out = run_arm(&cfg(4), &mut inj, &mut sub, &mut sink, |_, _, _| {});
        assert!(
            matches!(out, ArmOutcome::Completed { sent: 4, .. }),
            "{out:?}"
        );
        // When key k was sent, k + 1 intents were already written.
        assert_eq!(*seen.lock().expect("seen"), vec![1, 2, 3, 4]);
        let first = lines
            .lock()
            .expect("lines")
            .iter()
            .find(|l| l.contains("inject_intent"))
            .cloned()
            .expect("an intent");
        assert!(first.contains("\"ledger\":[97,353]"), "{first}");
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
    fn input_queued_before_the_arm_aborts_it_before_the_first_key() {
        // E.g. the operator typed while `--activate` moved focus: the
        // subscription was already listening, and the arm must not discard it.
        let (mut inj, mut sub, bus, sent) = setup(None);
        bus.publish(InputObs {
            t_ns: now_ns(),
            device: InputDevice::Keyboard,
            origin: InputOrigin::Real,
        });
        let (mut sink, _) = Sink::memory();
        let out = run_arm(&cfg(4), &mut inj, &mut sub, &mut sink, |_, _, _| {});
        assert!(
            matches!(
                out,
                ArmOutcome::Aborted {
                    index: 0,
                    reason: AbortReason::OperatorInput,
                    sent: 0,
                    ..
                }
            ),
            "{out:?}"
        );
        assert!(sent.lock().expect("sent").is_empty());
    }

    #[test]
    fn the_quiet_period_refuses_a_desktop_in_use() {
        let (_, mut sub, bus, _) = setup(None);
        let b = bus.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            b.publish(InputObs {
                t_ns: now_ns(),
                device: InputDevice::Pointer,
                origin: InputOrigin::Real,
            });
        });
        let r = await_quiet(&mut sub, Duration::from_millis(300));
        t.join().expect("publisher");
        assert!(matches!(r, Err((AbortReason::OperatorInput, _))), "{r:?}");
        // A quiet desktop passes.
        let (_, mut quiet_sub, _bus2, _) = setup(None);
        assert_eq!(
            await_quiet(&mut quiet_sub, Duration::from_millis(60)),
            Ok(())
        );
    }

    #[test]
    fn a_deaf_listener_aborts_after_the_first_key() {
        let (mut inj, mut sub, _bus, sent) = setup(None);
        inj.deaf = true;
        let (mut sink, _) = Sink::memory();
        let out = run_arm(&cfg(5), &mut inj, &mut sub, &mut sink, |_, _, _| {});
        assert!(
            matches!(
                out,
                ArmOutcome::Aborted {
                    index: 0,
                    reason: AbortReason::MonitorDeaf,
                    sent: 1,
                    ..
                }
            ),
            "{out:?}"
        );
        assert_eq!(
            sent.lock().expect("sent").len(),
            1,
            "no key after the deaf one"
        );
    }

    #[test]
    fn a_held_modifier_stops_the_arm_before_the_next_key() {
        // Shift goes down (another program, or the operator's hand resting
        // on it) while key 1 is being sent: the check right after that key
        // stops the arm, reported as the keyboard state — and key 2 is
        // never sent.
        let (mut inj, mut sub, _bus, sent) = setup(None);
        inj.keyboard_held_at = Some(2);
        let (mut sink, lines) = Sink::memory();
        let out = run_arm(&cfg(6), &mut inj, &mut sub, &mut sink, |_, _, _| {});
        assert!(
            matches!(
                out,
                ArmOutcome::Aborted {
                    index: 1,
                    reason: AbortReason::KeyboardState,
                    sent: 2,
                    ..
                }
            ),
            "{out:?}"
        );
        assert_eq!(sent.lock().expect("sent").len(), 2);
        assert!(lines
            .lock()
            .expect("lines")
            .iter()
            .any(|l| l.contains("\"reason\":\"keyboard_state\"")));
        // Held before the first key: nothing at all is sent.
        let (mut inj, mut sub, _bus, sent) = setup(None);
        inj.keyboard_held_at = Some(0);
        let (mut sink, _) = Sink::memory();
        let out = run_arm(&cfg(3), &mut inj, &mut sub, &mut sink, |_, _, _| {});
        assert!(
            matches!(
                out,
                ArmOutcome::Aborted {
                    index: 0,
                    reason: AbortReason::KeyboardState,
                    sent: 0,
                    ..
                }
            ),
            "{out:?}"
        );
        assert!(sent.lock().expect("sent").is_empty());
    }

    #[test]
    fn an_untypeable_key_refuses_the_arm_before_any_key() {
        let (mut inj, mut sub, _bus, sent) = setup(None);
        inj.unsupported = Some('d');
        let (mut sink, _) = Sink::memory();
        let out = run_arm(&cfg(6), &mut inj, &mut sub, &mut sink, |_, _, _| {});
        assert!(
            matches!(
                out,
                ArmOutcome::Aborted {
                    index: 3,
                    reason: AbortReason::UnsupportedKey,
                    sent: 0,
                    ..
                }
            ),
            "{out:?}"
        );
        assert!(sent.lock().expect("sent").is_empty(), "nothing was typed");
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
