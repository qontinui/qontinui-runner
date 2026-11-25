//! `qontinui-pty-holder --protocol-versions` — the line a runner reads from a
//! CANDIDATE build's holder before upgrading to it (plan
//! `2026-11-24-runner-upgrades-itself-daily-keeping-sessions-alive-with-no-outage-and-automatic-rollback`,
//! Phase 5). Driven through the real binary, the way the runner calls it.

use std::process::Command;

use qontinui_pty_holder::protocol::{parse_protocol_versions, PROTOCOL_VERSIONS};

const BIN: &str = env!("CARGO_BIN_EXE_qontinui-pty-holder");

#[test]
fn protocol_versions_flag_prints_the_build_list_and_exits_zero() {
    let out = Command::new(BIN)
        .arg("--protocol-versions")
        .output()
        .expect("run the holder binary");
    assert!(out.status.success(), "exit {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    assert_eq!(stdout.lines().count(), 1, "exactly one line: {stdout:?}");
    assert_eq!(
        parse_protocol_versions(&stdout).as_deref(),
        Some(PROTOCOL_VERSIONS)
    );
}

#[test]
fn protocol_versions_flag_refuses_to_be_combined_with_a_pane() {
    let out = Command::new(BIN)
        .args(["--protocol-versions", "--pane-id", "p1"])
        .output()
        .expect("run the holder binary");
    assert_eq!(out.status.code(), Some(2));
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    assert!(stdout.starts_with("holder_error=usage"), "{stdout}");
}
