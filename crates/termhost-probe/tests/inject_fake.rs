//! `inject` end to end against the in-memory desktop (`common`): nothing
//! happens on a desktop someone is using — not even the focus change of
//! `--activate` — and a run directory is never written twice.

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use common::{FakeHost, Faults};
use termhost_probe::inject::{run_cmd, InjectCmd, EXIT_ABORTED};
use termhost_probe::model::InputOrigin;
use termhost_probe::platform::TargetSpec;

fn run_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "termhost-probe-inject-{name}-{}-{}",
        std::process::id(),
        termhost_probe::clock::now_ns()
    ));
    std::fs::create_dir_all(&d).expect("run dir");
    d
}

fn cmd(dir: &std::path::Path) -> InjectCmd {
    InjectCmd {
        run_dir: dir.to_path_buf(),
        target: TargetSpec::Handle(0x5150),
        keys: vec!['a', 'b', 'c'],
        interval: Duration::from_millis(20),
        start_delay: Duration::ZERO,
        settle: Duration::from_millis(20),
        tag: None,
        activate: true,
        quiet: Duration::from_millis(400),
        echo_stdout: false,
        args: vec![],
    }
}

#[test]
fn input_during_the_quiet_period_refuses_before_focus_moves() {
    let dir = run_dir("busy");
    let mut host = FakeHost::new(Faults::default());
    let desk = host.desk.clone();
    // The operator moves the mouse 100 ms into the quiet period.
    let operator = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        desk.publish_input(InputOrigin::Real);
    });
    let code = run_cmd(&mut host, &cmd(&dir)).expect("runs");
    operator.join().expect("operator thread");
    assert_eq!(code, EXIT_ABORTED);
    assert_eq!(
        host.activations.load(Ordering::SeqCst),
        0,
        "focus must not move on a desktop in use"
    );
    assert_eq!(host.desk.keys_seen(), 0, "nothing was typed");
    let log = std::fs::read_to_string(dir.join("inject.jsonl")).expect("inject.jsonl");
    assert!(log.contains("\"reason\":\"operator_input\""), "{log}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_quiet_desktop_is_activated_then_typed_into_and_never_rewritten() {
    let dir = run_dir("quiet");
    let mut host = FakeHost::new(Faults::default());
    let code = run_cmd(&mut host, &cmd(&dir)).expect("runs");
    assert_eq!(code, 0);
    assert_eq!(host.activations.load(Ordering::SeqCst), 1);
    assert_eq!(host.desk.keys_seen(), 3);
    let log = std::fs::read_to_string(dir.join("inject.jsonl")).expect("inject.jsonl");
    assert_eq!(
        log.matches("\"type\":\"inject_intent\"").count(),
        3,
        "{log}"
    );
    // A second run into the same directory is refused, not appended.
    let mut again = FakeHost::new(Faults::default());
    assert!(run_cmd(&mut again, &cmd(&dir)).is_err());
    assert_eq!(again.desk.keys_seen(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}
