//! Journal reads that NAME the victim of an OOM kill inside a watched slice
//! (plan `2026-11-27-devops-oom-kills-inside-agent-build-slices-carry-no-victim`,
//! Layer C).
//!
//! The common shape is a build whose scope the kill ended: by the next tick
//! the scope is gone, so no cgroup counter can name it. The user manager logs
//! the kill as it happens — `MESSAGE_ID=fe6faa94e7774663a0da52717891d8ef`
//! ("A process of this unit has been killed by the OOM killer"), the message
//! `agent-build-scope.sh` classifies too — and that journal entry names the
//! unit.
//!
//! ## What this never reads
//!
//! `--output-fields=USER_UNIT,_SOURCE_REALTIME_TIMESTAMP` keeps the
//! `MESSAGE` text and every unit `Description` (which carries argv, and argv
//! can carry secrets) out of this process entirely, and the parser keeps only
//! `USER_UNIT`, `__REALTIME_TIMESTAMP` and `_BOOT_ID` of what does arrive.
//!
//! ## The journal names units, never totals
//!
//! One systemd message may coalesce several kills, and the parent slices log
//! the same message, so the count always comes from the kernel counters
//! (`slice_oom`). An entry is accepted only where `slice_oom` has an
//! unexplained kill to place and only for a unit its previous child listing
//! proves was inside the slice.
//!
//! ## Bounded and quiet
//!
//! One `journalctl` fork, only on a tick where a watched user-manager slice
//! has a kill no counter explained, forked as `access.rs` forks `tailscale`:
//! no window, killed on drop, a 5 s timeout. A missing binary, a non-zero
//! exit, a timeout and unparseable output are all `unreadable`, and the tick
//! goes on.

// Exercised by tests on every OS but forked in production only on Linux.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::time::Duration;

use chrono::{DateTime, Utc};

/// systemd's "a process of this unit has been killed by the OOM killer"
/// (`SD_MESSAGE_UNIT_OOM_KILLED`).
pub(crate) const UNIT_OOM_MESSAGE_ID: &str = "fe6faa94e7774663a0da52717891d8ef";

/// The only fields asked for. `MESSAGE` and `Description` are not among them.
pub(crate) const USER_OUTPUT_FIELDS: &str = "USER_UNIT,_SOURCE_REALTIME_TIMESTAMP";

/// Most recent entries read per window.
const USER_MAX_ENTRIES: u32 = 200;

/// Bound on one `journalctl` fork.
pub(crate) const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// One unit-OOM journal entry, reduced to what Layer C uses.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UnitOomEntry {
    pub(crate) unit: String,
    /// `__REALTIME_TIMESTAMP`, microseconds since the epoch — written by
    /// journald, not by the logging process.
    pub(crate) realtime_usec: u64,
    /// `_BOOT_ID`, as journald writes it (32 hex digits).
    pub(crate) boot_id: String,
}

/// What the user-journal read produced.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum JournalRead {
    /// `journalctl` exited 0 and every line parsed.
    Read(Vec<UnitOomEntry>),
    /// Missing binary, non-zero exit, timeout, or unparseable output.
    Unreadable,
}

/// A boot id as one comparable key: the kernel's dashed UUID
/// (`/proc/sys/kernel/random/boot_id`) and journald's 32 hex digits name the
/// same boot. PURE.
pub(crate) fn boot_key(id: &str) -> String {
    id.chars()
        .filter(|c| *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// `@<seconds>` for `--since` (rounded down) / `--until` (rounded up): the
/// exact microsecond bounds are applied in-process. PURE.
fn at_secs(t: DateTime<Utc>, round_up: bool) -> String {
    let secs = t.timestamp() + i64::from(round_up && t.timestamp_subsec_nanos() > 0);
    format!("@{secs}")
}

/// The user-journal query for `[since, until)`. PURE.
pub(crate) fn user_journal_args(since: DateTime<Utc>, until: DateTime<Utc>) -> Vec<String> {
    vec![
        "--user".into(),
        "-o".into(),
        "json".into(),
        format!("--output-fields={USER_OUTPUT_FIELDS}"),
        "--no-pager".into(),
        "-q".into(),
        "--since".into(),
        at_secs(since, false),
        "--until".into(),
        at_secs(until, true),
        "-n".into(),
        USER_MAX_ENTRIES.to_string(),
        format!("MESSAGE_ID={UNIT_OOM_MESSAGE_ID}"),
    ]
}

/// A systemd unit name as Layer C will compare it: non-empty, at most 255
/// bytes, systemd's unit-name alphabet. PURE.
fn valid_unit_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 255
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'.' | b'-' | b'@' | b'\\'))
}

/// One journal JSON field as text. journald writes a field that is not
/// valid UTF-8 as an array of bytes.
pub(crate) fn field_text(v: Option<&serde_json::Value>) -> Option<String> {
    match v? {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(bytes) => {
            let b: Option<Vec<u8>> = bytes
                .iter()
                .map(|x| x.as_u64().and_then(|n| u8::try_from(n).ok()))
                .collect();
            String::from_utf8(b?).ok()
        }
        _ => None,
    }
}

/// `journalctl -o json` output as one JSON object per line, or `None` when
/// any line is not one (unparseable output). PURE.
pub(crate) fn json_lines(stdout: &str) -> Option<Vec<serde_json::Map<String, serde_json::Value>>> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| match serde_json::from_str::<serde_json::Value>(l) {
            Ok(serde_json::Value::Object(m)) => Some(m),
            _ => None,
        })
        .collect()
}

/// `__REALTIME_TIMESTAMP` and `_BOOT_ID` of one entry.
pub(crate) fn entry_time_and_boot(
    m: &serde_json::Map<String, serde_json::Value>,
) -> Option<(u64, String)> {
    let at = field_text(m.get("__REALTIME_TIMESTAMP"))?.parse::<u64>().ok()?;
    let boot = field_text(m.get("_BOOT_ID"))?;
    Some((at, boot))
}

/// Parse the user-journal read. Only `USER_UNIT`, `__REALTIME_TIMESTAMP` and
/// `_BOOT_ID` survive; an entry missing one of them, or naming no valid unit,
/// is skipped. Any line that is not a JSON object makes the whole read
/// `Unreadable`. PURE.
pub(crate) fn parse_user_journal(stdout: &str) -> JournalRead {
    let Some(lines) = json_lines(stdout) else {
        return JournalRead::Unreadable;
    };
    let mut out: Vec<UnitOomEntry> = lines
        .iter()
        .filter_map(|m| {
            let unit = field_text(m.get("USER_UNIT")).filter(|u| valid_unit_name(u))?;
            let (realtime_usec, boot_id) = entry_time_and_boot(m)?;
            Some(UnitOomEntry {
                unit,
                realtime_usec,
                boot_id,
            })
        })
        .collect();
    out.sort_by_key(|e| e.realtime_usec);
    JournalRead::Read(out)
}

/// The user-journal read from one fork's stdout (`None` = the fork failed).
/// PURE.
pub(crate) fn user_read_from(stdout: Option<&str>) -> JournalRead {
    stdout.map_or(JournalRead::Unreadable, parse_user_journal)
}

/// Fork `bin args`, bounded by `timeout`: its stdout when it exited 0,
/// `None` when it is missing, exited non-zero or timed out. Forked as
/// `access.rs` forks `tailscale` — no window, killed on drop, stdin and
/// stderr closed.
pub(crate) async fn run_journalctl(
    bin: &str,
    args: &[String],
    timeout: Duration,
) -> Option<String> {
    let mut cmd = crate::process_helpers::tokio_no_window(bin);
    cmd.args(args)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => {
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        }
        _ => None,
    }
}

/// Layer C's read of the user journal over `[since, until)`.
pub(crate) async fn read_user(since: DateTime<Utc>, until: DateTime<Utc>) -> JournalRead {
    let out = run_journalctl("journalctl", &user_journal_args(since, until), READ_TIMEOUT).await;
    user_read_from(out.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// Plan test 16 (argv half): the read asks for exactly two fields —
    /// never `MESSAGE` or a `Description` — of the unit-OOM message only.
    #[test]
    fn the_user_journal_argv_asks_for_unit_and_timestamp_only() {
        let args = user_journal_args(t("2026-11-27T00:14:40.5Z"), t("2026-11-27T00:15:10.25Z"));
        assert_eq!(
            args,
            vec![
                "--user",
                "-o",
                "json",
                "--output-fields=USER_UNIT,_SOURCE_REALTIME_TIMESTAMP",
                "--no-pager",
                "-q",
                "--since",
                "@1795738480",
                "--until",
                "@1795738511",
                "-n",
                "200",
                "MESSAGE_ID=fe6faa94e7774663a0da52717891d8ef",
            ]
        );
        // A whole-second until is not rounded past itself.
        let exact = user_journal_args(t("2026-11-27T00:14:40Z"), t("2026-11-27T00:15:10Z"));
        assert_eq!(exact[9], "@1795738510");
    }

    #[test]
    fn the_parser_keeps_only_unit_time_and_boot() {
        let out = concat!(
            r#"{"__CURSOR":"s=1","__REALTIME_TIMESTAMP":"1795738498277337","_BOOT_ID":"27cd98773ff347f083889880a31d5902","USER_UNIT":"agent-build-1795734446-543417-858931110.scope","_SOURCE_REALTIME_TIMESTAMP":"1795738498274118"}"#,
            "\n",
            // No USER_UNIT: skipped, not fatal.
            r#"{"__REALTIME_TIMESTAMP":"1795738498000000","_BOOT_ID":"27cd98773ff347f083889880a31d5902"}"#,
            "\n",
            // A unit name outside systemd's alphabet: skipped.
            r#"{"__REALTIME_TIMESTAMP":"1795738498000001","_BOOT_ID":"27cd98773ff347f083889880a31d5902","USER_UNIT":"a b;rm"}"#,
            "\n",
            // A non-UTF-8-safe field arrives as bytes ("x.scope").
            r#"{"__REALTIME_TIMESTAMP":"1795738497000000","_BOOT_ID":"27cd98773ff347f083889880a31d5902","USER_UNIT":[120,46,115,99,111,112,101]}"#,
            "\n\n",
        );
        assert_eq!(
            parse_user_journal(out),
            JournalRead::Read(vec![
                UnitOomEntry {
                    unit: "x.scope".into(),
                    realtime_usec: 1795738497000000,
                    boot_id: "27cd98773ff347f083889880a31d5902".into(),
                },
                UnitOomEntry {
                    unit: "agent-build-1795734446-543417-858931110.scope".into(),
                    realtime_usec: 1795738498277337,
                    boot_id: "27cd98773ff347f083889880a31d5902".into(),
                },
            ])
        );
        assert_eq!(parse_user_journal(""), JournalRead::Read(vec![]));
    }

    #[test]
    fn boot_ids_compare_across_the_kernel_and_journald_spellings() {
        assert_eq!(
            boot_key("27cd9877-3ff3-47f0-8388-9880a31d5902"),
            boot_key("27CD98773FF347F083889880A31D5902")
        );
        assert_eq!(boot_key("27cd9877-3ff3-47f0-8388-9880a31d5902"), "27cd98773ff347f083889880a31d5902");
    }

    /// Plan test 4: the four ways a read fails — the binary is absent, it
    /// exits non-zero, it outlives the timeout, or it prints something that
    /// is not JSON — are all `Unreadable`.
    #[tokio::test]
    async fn every_failed_read_is_unreadable() {
        let none: Vec<String> = Vec::new();
        let absent = run_journalctl("/nonexistent/qontinui-test/journalctl", &none, READ_TIMEOUT).await;
        assert_eq!(user_read_from(absent.as_deref()), JournalRead::Unreadable, "absent");
        let nonzero = run_journalctl("false", &none, READ_TIMEOUT).await;
        assert_eq!(user_read_from(nonzero.as_deref()), JournalRead::Unreadable, "non-zero");
        let started = std::time::Instant::now();
        let slow = run_journalctl("sleep", &["5".to_string()], Duration::from_millis(200)).await;
        assert_eq!(user_read_from(slow.as_deref()), JournalRead::Unreadable, "timed out");
        assert!(started.elapsed() < Duration::from_secs(4), "the timeout bounds the fork");
        assert_eq!(
            user_read_from(Some("journalctl: hint: you are not in the systemd-journal group\n")),
            JournalRead::Unreadable,
            "unparseable"
        );
        // And a clean exit with JSON is a read.
        let ok = run_journalctl("echo", &[r#"{"USER_UNIT":"x.scope","__REALTIME_TIMESTAMP":"1","_BOOT_ID":"ab"}"#.to_string()], READ_TIMEOUT).await;
        assert_eq!(
            user_read_from(ok.as_deref()),
            JournalRead::Read(vec![UnitOomEntry {
                unit: "x.scope".into(),
                realtime_usec: 1,
                boot_id: "ab".into()
            }])
        );
    }
}
