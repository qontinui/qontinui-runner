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
//! `--output-fields=USER_UNIT` keeps the
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
//! ## The kernel's own record (Phase 3)
//!
//! Where the kernel log is readable to this user, its `oom-kill:` line names
//! the victim's cgroup (`task_memcg`, kernel-written, exact) and `comm`.
//! Readability is probed (`journalctl -k -b -n 1` returning an entry), never
//! assumed from an exit code. Nothing here widens host privilege: on a box
//! with `dmesg_restrict` and no journal group the arm reads `unreadable`.
//!
//! ## Readability is probed, on both arms
//!
//! An unprivileged `journalctl` that cannot read a journal exits 0 with zero
//! entries, so an empty window proves nothing by itself. Each arm forks a
//! one-entry probe beside its window read; only a probe that returns an entry
//! makes an empty window a real "nothing in the window".
//!
//! ## Bounded and quiet
//!
//! The forks run only on a tick where a watched slice has a kill to place,
//! forked as `access.rs` forks `tailscale` (no window, killed on drop), and
//! all of them run CONCURRENTLY under one overall deadline ([`READ_TIMEOUT`]),
//! so a tick waits at most that long for every journal read together. A
//! missing binary, a non-zero exit, a timeout and unparseable output are all
//! `unreadable`, and the tick goes on. The binary is a [`Journalctl`] value,
//! so a test drives the whole path with a fake one.

// Exercised by tests on every OS but forked in production only on Linux.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::time::Duration;

use chrono::{DateTime, Utc};

/// systemd's "a process of this unit has been killed by the OOM killer"
/// (`SD_MESSAGE_UNIT_OUT_OF_MEMORY`, `sd-messages.h`).
pub(crate) const UNIT_OOM_MESSAGE_ID: &str = "fe6faa94e7774663a0da52717891d8ef";

/// The only field asked for (journald adds its own `__REALTIME_TIMESTAMP`,
/// `_BOOT_ID` and cursor). `MESSAGE` and `Description` are not among them.
pub(crate) const USER_OUTPUT_FIELDS: &str = "USER_UNIT";

/// Most recent entries read per window.
const USER_MAX_ENTRIES: u32 = 200;

/// Bound on every journal read of one tick, together.
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

/// A one-entry probe of the user journal: readable iff it returns an entry.
/// Only journald's own fields are asked for. PURE.
pub(crate) fn user_probe_args() -> Vec<String> {
    [
        "--user",
        "-n",
        "1",
        "-o",
        "json",
        "--output-fields=_BOOT_ID",
        "--no-pager",
        "-q",
    ]
    .iter()
    .map(|a| a.to_string())
    .collect()
}

/// Whether a probe's stdout proves a journal readable: at least one entry.
/// PURE.
pub(crate) fn readable(probe: Option<&str>) -> bool {
    probe.and_then(json_lines).is_some_and(|l| !l.is_empty())
}

/// The user-journal read from the probe's and the window's stdout (`None` =
/// that fork failed). An unreadable journal answers an empty window with
/// exit 0, so only a probe that returned an entry makes the window count.
/// PURE.
pub(crate) fn user_read_from(probe: Option<&str>, window: Option<&str>) -> JournalRead {
    if !readable(probe) {
        return JournalRead::Unreadable;
    }
    window.map_or(JournalRead::Unreadable, parse_user_journal)
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
    match run_journalctl_status(bin, args, timeout).await {
        Some((Some(0), stdout)) => Some(stdout),
        _ => None,
    }
}

/// Fork `bin args` as [`run_journalctl`] does, but keep the exit code:
/// `(code, stdout)`, or `None` when it could not run or timed out. The code
/// is what tells `journalctl --grep`'s "no match" (1, nothing printed) from
/// a read.
pub(crate) async fn run_journalctl_status(
    bin: &str,
    args: &[String],
    timeout: Duration,
) -> Option<(Option<i32>, String)> {
    let mut cmd = crate::process_helpers::tokio_no_window(bin);
    cmd.args(args)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(out)) => Some((
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )),
        _ => None,
    }
}

/// The `journalctl` to fork: a program and the arguments that lead every
/// call. Production forks `journalctl`; a test forks `sh <fake script>`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Journalctl {
    pub(crate) program: String,
    pub(crate) leading: Vec<String>,
}

impl Journalctl {
    /// The real binary, from `PATH`.
    pub(crate) fn live() -> Self {
        Self {
            program: "journalctl".into(),
            leading: Vec::new(),
        }
    }

    fn argv(&self, args: Vec<String>) -> Vec<String> {
        let mut all = self.leading.clone();
        all.extend(args);
        all
    }

    async fn run(&self, args: Vec<String>, timeout: Duration) -> Option<String> {
        run_journalctl(&self.program, &self.argv(args), timeout).await
    }

    async fn run_status(&self, args: Vec<String>, timeout: Duration) -> Option<(Option<i32>, String)> {
        run_journalctl_status(&self.program, &self.argv(args), timeout).await
    }
}

/// Layer C's read of the user journal over `[since, until)`: the probe and
/// the window, concurrently.
pub(crate) async fn read_user(
    jc: &Journalctl,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    timeout: Duration,
) -> JournalRead {
    let (probe, window) = tokio::join!(
        jc.run(user_probe_args(), timeout),
        jc.run(user_journal_args(since, until), timeout),
    );
    user_read_from(probe.as_deref(), window.as_deref())
}

/// Every journal read one tick asked for, concurrently, under ONE deadline:
/// `(user, kernel)`, each `None` when not asked. Each arm is bounded by the
/// time left to that deadline on its own, so a slow arm never discards one
/// that already finished; an arm the deadline cut is `Unreadable`, never a
/// stalled tick.
pub(crate) async fn read_all(
    jc: &Journalctl,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    user: bool,
    kernel: bool,
    deadline: Duration,
) -> (Option<JournalRead>, Option<KernelRead>) {
    let end = tokio::time::Instant::now() + deadline;
    let user_read = async {
        if !user {
            return None;
        }
        Some(
            tokio::time::timeout_at(end, read_user(jc, since, until, deadline))
                .await
                .unwrap_or(JournalRead::Unreadable),
        )
    };
    let kernel_read = async {
        if !kernel {
            return None;
        }
        Some(
            tokio::time::timeout_at(end, read_kernel(jc, since, until, deadline))
                .await
                .unwrap_or(KernelRead::Unreadable),
        )
    };
    tokio::join!(user_read, kernel_read)
}

// ---------------------------------------------------------------------------
// Phase 3: the kernel's own OOM record
// ---------------------------------------------------------------------------

/// Most recent `oom-kill:` entries read per window when journalctl can
/// filter by pattern (`--grep`).
const KERNEL_MAX_RECORDS: u32 = 200;

/// Most recent kernel entries per window when it cannot (a build whose
/// `--version` does not list `+PCRE2`). An OOM report prints its task dump BEFORE its `oom-kill:` line,
/// so the newest entries hold the newest records; older ones in a long
/// window may fall outside this bound, and their kills stay the remainder.
const KERNEL_MAX_ENTRIES: u32 = 5000;

/// One kernel `oom-kill:` record, reduced to what placement uses.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct KernelOomRecord {
    /// `task_memcg=`: the victim's cgroup, written by the kernel — exact.
    pub(crate) task_memcg: String,
    /// `task=`: the victim's `comm`.
    pub(crate) comm: String,
    pub(crate) realtime_usec: u64,
    pub(crate) boot_id: String,
}

/// What the kernel-log read produced.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum KernelRead {
    /// The kernel log is readable here; these are its `oom-kill:` records in
    /// the window (possibly none).
    Read(Vec<KernelOomRecord>),
    /// Not readable by this user (on a box with `dmesg_restrict` and no
    /// journal group, an unprivileged `journalctl -k` exits 0 with ZERO
    /// entries), or the read failed.
    Unreadable,
}

/// The readability probe: the kernel log counts as readable iff this
/// returns at least one entry. Only journald's own fields are asked for.
/// PURE.
pub(crate) fn kernel_probe_args() -> Vec<String> {
    [
        "-k",
        "-b",
        "-n",
        "1",
        "-o",
        "json",
        "--output-fields=_BOOT_ID",
        "--no-pager",
        "-q",
    ]
    .iter()
    .map(|a| a.to_string())
    .collect()
}

/// The kernel-log query for `[since, until)`: `MESSAGE` is needed to read
/// the `oom-kill:` line; only `task_memcg` and `task` of it are kept. With
/// `grep`, journalctl returns only the `oom-kill:` lines, so a task dump
/// cannot push an earlier record out of the bound. PURE.
pub(crate) fn kernel_journal_args(
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    grep: bool,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-k".into(),
        "-o".into(),
        "json".into(),
        "--output-fields=MESSAGE".into(),
        "--no-pager".into(),
        "-q".into(),
        "--since".into(),
        at_secs(since, false),
        "--until".into(),
        at_secs(until, true),
        "-n".into(),
        if grep {
            KERNEL_MAX_RECORDS
        } else {
            KERNEL_MAX_ENTRIES
        }
        .to_string(),
    ];
    if grep {
        args.push("--grep=^oom-kill:".into());
    }
    args.push("_TRANSPORT=kernel".into());
    args
}

/// `(task_memcg, comm)` from a kernel
/// `oom-kill:constraint=…,oom_memcg=…,task_memcg=<path>,task=<comm>,pid=<n>,uid=<n>`
/// line. `comm` is split from the right (`,pid=`), since a process may name
/// itself with a comma. `None` for any other line. PURE.
pub(crate) fn parse_oom_kill_line(msg: &str) -> Option<(String, String)> {
    let rest = msg.trim_start().strip_prefix("oom-kill:")?;
    let at = rest
        .match_indices("task_memcg=")
        .map(|(i, _)| i)
        .find(|&i| i == 0 || rest.as_bytes().get(i - 1) == Some(&b','))?;
    let after = rest.get(at + "task_memcg=".len()..)?;
    let cut = after.find(",task=")?;
    let memcg = after.get(..cut)?;
    let tail = after.get(cut + ",task=".len()..)?;
    let comm = tail.get(..tail.rfind(",pid=")?)?;
    let clean = |v: &str| !v.chars().any(char::is_control);
    (memcg.starts_with('/') && clean(memcg) && !comm.is_empty() && comm.len() <= 64 && clean(comm))
        .then(|| (memcg.to_string(), comm.to_string()))
}

/// The `oom-kill:` records in one kernel-log read; `None` when the output is
/// unparseable. PURE.
pub(crate) fn parse_kernel_journal(stdout: &str) -> Option<Vec<KernelOomRecord>> {
    let mut out: Vec<KernelOomRecord> = json_lines(stdout)?
        .iter()
        .filter_map(|m| {
            let (task_memcg, comm) = parse_oom_kill_line(&field_text(m.get("MESSAGE"))?)?;
            let (realtime_usec, boot_id) = entry_time_and_boot(m)?;
            Some(KernelOomRecord {
                task_memcg,
                comm,
                realtime_usec,
                boot_id,
            })
        })
        .collect();
    out.sort_by_key(|r| r.realtime_usec);
    Some(out)
}

/// The kernel read from the probe's and the window read's stdout (`None` =
/// that fork failed or was not run). PURE.
pub(crate) fn kernel_read_from(probe: Option<&str>, window: Option<&str>) -> KernelRead {
    if !readable(probe) {
        return KernelRead::Unreadable;
    }
    window
        .and_then(parse_kernel_journal)
        .map_or(KernelRead::Unreadable, KernelRead::Read)
}

/// Whether a `journalctl --version` output says it was built with PCRE2,
/// i.e. supports `--grep`. Unknown (no output) is `false`: the plain read
/// is slower, never wrong. PURE.
pub(crate) fn grep_supported(version: Option<&str>) -> bool {
    version.is_some_and(|v| v.split_whitespace().any(|f| f == "+PCRE2"))
}

/// The window stdout of a `--grep` read from its exit: 0 is the output; 1
/// with nothing printed is journalctl's "no entry matched" (systemd 257) — an
/// empty window, not a failure; anything else is no window. PURE.
pub(crate) fn grep_window_from(run: Option<(Option<i32>, String)>) -> Option<String> {
    match run? {
        (Some(0), out) => Some(out),
        (Some(1), out) if out.trim().is_empty() => Some(String::new()),
        _ => None,
    }
}

/// Phase 3's read of the kernel log over `[since, until)`. The version, the
/// readability probe and the `--grep`-filtered window run concurrently; the
/// unfiltered 5000-entry window is forked only where `--version` does not
/// list `+PCRE2` (a no-match `--grep` exit is an empty window, never a reason
/// to fall back).
pub(crate) async fn read_kernel(
    jc: &Journalctl,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    timeout: Duration,
) -> KernelRead {
    let (version, probe, grepped) = tokio::join!(
        jc.run(vec!["--version".to_string()], timeout),
        jc.run(kernel_probe_args(), timeout),
        jc.run_status(kernel_journal_args(since, until, true), timeout),
    );
    if !readable(probe.as_deref()) {
        return KernelRead::Unreadable;
    }
    let window = if grep_supported(version.as_deref()) {
        grep_window_from(grepped)
    } else {
        jc.run(kernel_journal_args(since, until, false), timeout).await
    };
    kernel_read_from(probe.as_deref(), window.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// Plan test 16 (argv half): the read asks for one field — never
    /// `MESSAGE` or a `Description` — of the unit-OOM message only.
    #[test]
    fn the_user_journal_argv_asks_for_unit_and_timestamp_only() {
        let args = user_journal_args(t("2026-11-27T00:14:40.5Z"), t("2026-11-27T00:15:10.25Z"));
        assert_eq!(
            args,
            vec![
                "--user",
                "-o",
                "json",
                "--output-fields=USER_UNIT",
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
    #[cfg(unix)]
    #[tokio::test]
    async fn every_failed_read_is_unreadable() {
        let none: Vec<String> = Vec::new();
        let probe = Some(r#"{"__CURSOR":"s=1","__REALTIME_TIMESTAMP":"1","_BOOT_ID":"ab"}"#);
        let absent = run_journalctl("/nonexistent/qontinui-test/journalctl", &none, READ_TIMEOUT).await;
        assert_eq!(user_read_from(probe, absent.as_deref()), JournalRead::Unreadable, "absent");
        let nonzero = run_journalctl("false", &none, READ_TIMEOUT).await;
        assert_eq!(user_read_from(probe, nonzero.as_deref()), JournalRead::Unreadable, "non-zero");
        let started = std::time::Instant::now();
        let slow = run_journalctl("sleep", &["5".to_string()], Duration::from_millis(200)).await;
        assert_eq!(user_read_from(probe, slow.as_deref()), JournalRead::Unreadable, "timed out");
        assert!(started.elapsed() < Duration::from_secs(4), "the timeout bounds the fork");
        assert_eq!(
            user_read_from(probe, Some("journalctl: hint: you are not in the systemd-journal group\n")),
            JournalRead::Unreadable,
            "unparseable"
        );
        // And a clean exit with JSON is a read.
        let ok = run_journalctl("echo", &[r#"{"USER_UNIT":"x.scope","__REALTIME_TIMESTAMP":"1","_BOOT_ID":"ab"}"#.to_string()], READ_TIMEOUT).await;
        assert_eq!(
            user_read_from(probe, ok.as_deref()),
            JournalRead::Read(vec![UnitOomEntry {
                unit: "x.scope".into(),
                realtime_usec: 1,
                boot_id: "ab".into()
            }])
        );
    }

    #[test]
    fn the_oom_kill_line_yields_task_memcg_and_comm() {
        let line = "oom-kill:constraint=CONSTRAINT_MEMCG,nodemask=(null),cpuset=/,mems_allowed=0,oom_memcg=/user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice,task_memcg=/user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice/agent-build-x.scope,task=rustc,pid=4242,uid=1000";
        assert_eq!(
            parse_oom_kill_line(line),
            Some((
                "/user.slice/user-1000.slice/user@1000.service/agent.slice/agent-builds.slice/agent-build-x.scope".to_string(),
                "rustc".to_string()
            ))
        );
        // A comm with a comma is split from the right.
        assert_eq!(
            parse_oom_kill_line("oom-kill:constraint=CONSTRAINT_NONE,task_memcg=/a.slice/b.scope,task=a,b,pid=1,uid=0")
                .map(|x| x.1),
            Some("a,b".to_string())
        );
        assert_eq!(parse_oom_kill_line("Memory cgroup out of memory: Killed process 4242 (rustc)"), None);
        assert_eq!(parse_oom_kill_line("oom-kill:constraint=X,task_memcg=relative,task=x,pid=1,uid=0"), None);
        assert_eq!(parse_oom_kill_line("oom-kill:constraint=X,task_memcg=/a.slice,task=,pid=1,uid=0"), None);
    }

    #[test]
    fn the_kernel_argv_reads_only_kernel_messages_of_the_window() {
        assert_eq!(
            kernel_probe_args(),
            vec!["-k", "-b", "-n", "1", "-o", "json", "--output-fields=_BOOT_ID", "--no-pager", "-q"]
        );
        assert_eq!(
            kernel_journal_args(t("2026-11-27T00:14:40Z"), t("2026-11-27T00:15:10Z"), true),
            vec![
                "-k",
                "-o",
                "json",
                "--output-fields=MESSAGE",
                "--no-pager",
                "-q",
                "--since",
                "@1795738480",
                "--until",
                "@1795738510",
                "-n",
                "200",
                "--grep=^oom-kill:",
                "_TRANSPORT=kernel",
            ]
        );
        let plain = kernel_journal_args(t("2026-11-27T00:14:40Z"), t("2026-11-27T00:15:10Z"), false);
        assert_eq!(&plain[10..], &["-n", "5000", "_TRANSPORT=kernel"]);
    }

    /// The kernel log is readable iff the probe returned an entry: an
    /// unprivileged `journalctl -k` exits 0 with none.
    #[test]
    fn kernel_readability_is_probed_not_assumed() {
        let probe = r#"{"__CURSOR":"s=1","__REALTIME_TIMESTAMP":"1","_BOOT_ID":"ab"}"#;
        let window = concat!(
            r#"{"__REALTIME_TIMESTAMP":"1795738498277337","_BOOT_ID":"27cd98773ff347f083889880a31d5902","MESSAGE":"oom-kill:constraint=CONSTRAINT_MEMCG,task_memcg=/a.slice/b.scope,task=rustc,pid=9,uid=1000"}"#,
            "\n",
            r#"{"__REALTIME_TIMESTAMP":"1795738498277000","_BOOT_ID":"27cd98773ff347f083889880a31d5902","MESSAGE":"Tasks state (memory values in pages):"}"#,
            "\n",
        );
        assert_eq!(kernel_read_from(Some(""), Some(window)), KernelRead::Unreadable, "empty probe");
        assert_eq!(kernel_read_from(None, Some(window)), KernelRead::Unreadable, "failed probe");
        assert_eq!(kernel_read_from(Some(probe), None), KernelRead::Unreadable, "failed window");
        assert_eq!(kernel_read_from(Some(probe), Some("not json")), KernelRead::Unreadable);
        assert_eq!(kernel_read_from(Some(probe), Some("")), KernelRead::Read(vec![]));
        assert_eq!(
            kernel_read_from(Some(probe), Some(window)),
            KernelRead::Read(vec![KernelOomRecord {
                task_memcg: "/a.slice/b.scope".into(),
                comm: "rustc".into(),
                realtime_usec: 1795738498277337,
                boot_id: "27cd98773ff347f083889880a31d5902".into(),
            }])
        );
    }

    /// Plan A7: an unreadable user journal answers the window with exit 0 and
    /// no entries; only a probe that returned an entry makes it a read.
    #[test]
    fn an_empty_user_window_is_a_read_only_when_the_probe_answered() {
        let probe = r#"{"__CURSOR":"s=1","__REALTIME_TIMESTAMP":"1","_BOOT_ID":"ab"}"#;
        assert_eq!(user_read_from(Some(""), Some("")), JournalRead::Unreadable);
        assert_eq!(user_read_from(None, Some("")), JournalRead::Unreadable);
        assert_eq!(user_read_from(Some(probe), Some("")), JournalRead::Read(vec![]));
        assert_eq!(user_read_from(Some(probe), None), JournalRead::Unreadable);
        assert_eq!(
            user_probe_args(),
            vec!["--user", "-n", "1", "-o", "json", "--output-fields=_BOOT_ID", "--no-pager", "-q"]
        );
    }

    /// A fake `journalctl`: `sh <script>`, so no freshly written file is ever
    /// exec'd (no ETXTBSY race with a concurrent fork).
    #[cfg(unix)]
    pub(crate) fn fake_journalctl(dir: &std::path::Path, body: &str) -> Journalctl {
        let script = dir.join("journalctl.sh");
        std::fs::write(&script, body).unwrap();
        Journalctl {
            program: "sh".into(),
            leading: vec![script.to_string_lossy().into_owned()],
        }
    }

    /// Review A3: the user read and the kernel probe and window run
    /// concurrently under ONE deadline; a journalctl that never answers costs
    /// the tick the deadline, once, and every read it cut is `Unreadable`.
    #[cfg(unix)]
    #[tokio::test]
    async fn every_read_is_bounded_by_one_deadline() {
        let d = tempfile::tempdir().unwrap();
        let jc = fake_journalctl(d.path(), "sleep 10\n");
        let started = std::time::Instant::now();
        let (u, k) = read_all(
            &jc,
            t("2026-11-27T00:14:40Z"),
            t("2026-11-27T00:15:10Z"),
            true,
            true,
            Duration::from_millis(300),
        )
        .await;
        assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
        assert_eq!(u, Some(JournalRead::Unreadable));
        assert_eq!(k, Some(KernelRead::Unreadable));
        // Not asked is None, never Unreadable.
        let (u, k) = read_all(
            &jc,
            t("2026-11-27T00:14:40Z"),
            t("2026-11-27T00:15:10Z"),
            false,
            false,
            Duration::from_millis(300),
        )
        .await;
        assert_eq!((u, k), (None, None));
    }

    /// Review A8: a journalctl built without PCRE2 (its `--version` lists
    /// `-PCRE2`) is read without `--grep`, and its records still parse.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_journalctl_without_grep_falls_back_to_the_plain_window() {
        let d = tempfile::tempdir().unwrap();
        let body = r#"case "$*" in
  --version) echo 'systemd 247 (247.3-7)'; echo '+PAM -PCRE2 +ZSTD' ;;
  *"-b -n 1"*) echo '{"__CURSOR":"s=1","__REALTIME_TIMESTAMP":"1","_BOOT_ID":"ab"}' ;;
  *--grep*) echo "Compiled without pattern matching support" >&2; exit 1 ;;
  *) echo '{"__REALTIME_TIMESTAMP":"1795738498277337","_BOOT_ID":"27cd98773ff347f083889880a31d5902","MESSAGE":"oom-kill:constraint=CONSTRAINT_MEMCG,task_memcg=/a.slice/b.scope,task=rustc,pid=9,uid=1000"}' ;;
esac
"#;
        let jc = fake_journalctl(d.path(), body);
        let k = read_kernel(&jc, t("2026-11-27T00:14:40Z"), t("2026-11-27T00:15:10Z"), READ_TIMEOUT).await;
        assert_eq!(
            k,
            KernelRead::Read(vec![KernelOomRecord {
                task_memcg: "/a.slice/b.scope".into(),
                comm: "rustc".into(),
                realtime_usec: 1795738498277337,
                boot_id: "27cd98773ff347f083889880a31d5902".into(),
            }])
        );
    }

    /// Round-2 A3: on systemd 257 `journalctl --grep` exits 1 with nothing
    /// printed when no entry matches. That is an empty window — a readable
    /// kernel log with no record — not a missing PCRE2, so the 5000-entry
    /// read is never forked.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_grep_with_no_match_is_an_empty_window_not_a_fallback() {
        let d = tempfile::tempdir().unwrap();
        let plain_marker = d.path().join("plain-read");
        let body = format!(
            r#"case "$*" in
  --version) echo 'systemd 257 (257.13-1~deb13u1)'; echo '+PAM +PCRE2 +ZSTD' ;;
  *"-b -n 1"*) echo '{{"__CURSOR":"s=1","__REALTIME_TIMESTAMP":"1","_BOOT_ID":"ab"}}' ;;
  *--grep*) exit 1 ;;
  *) touch '{}'; echo '{{"__REALTIME_TIMESTAMP":"1795738498277337","_BOOT_ID":"ab","MESSAGE":"oom-kill:constraint=CONSTRAINT_MEMCG,task_memcg=/a.slice/b.scope,task=rustc,pid=9,uid=1000"}}' ;;
esac
"#,
            plain_marker.display()
        );
        let jc = fake_journalctl(d.path(), &body);
        let k = read_kernel(&jc, t("2026-11-27T00:14:40Z"), t("2026-11-27T00:15:10Z"), READ_TIMEOUT).await;
        assert_eq!(k, KernelRead::Read(vec![]));
        assert!(!plain_marker.exists(), "no fallback read on a no-match exit");
    }

    #[test]
    fn grep_exits_and_versions_read_as_journalctl_means_them() {
        assert_eq!(grep_window_from(Some((Some(0), "x".into()))), Some("x".into()));
        assert_eq!(grep_window_from(Some((Some(1), "\n".into()))), Some(String::new()));
        assert_eq!(grep_window_from(Some((Some(1), "partial".into()))), None);
        assert_eq!(grep_window_from(Some((Some(2), String::new()))), None);
        assert_eq!(grep_window_from(Some((None, String::new()))), None, "killed by a signal");
        assert_eq!(grep_window_from(None), None);
        assert!(grep_supported(Some("systemd 257 (257.13)\n+PAM +AUDIT +PCRE2 +ZSTD\n")));
        assert!(!grep_supported(Some("systemd 247\n+PAM -PCRE2\n")));
        assert!(!grep_supported(None));
    }

    /// Round-2 A4: each arm is bounded on its own, so a user read that
    /// finished survives a kernel read the deadline cut.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_fast_arm_survives_a_slow_one() {
        let d = tempfile::tempdir().unwrap();
        let body = r#"case "$1" in
  --user)
    case "$2" in
      -n) echo '{"__CURSOR":"s=1","__REALTIME_TIMESTAMP":"1","_BOOT_ID":"ab"}' ;;
      *) echo '{"__REALTIME_TIMESTAMP":"1795738498277337","_BOOT_ID":"ab","USER_UNIT":"x.scope"}' ;;
    esac ;;
  *) sleep 10 ;;
esac
"#;
        let jc = fake_journalctl(d.path(), body);
        let started = std::time::Instant::now();
        let (u, k) = read_all(
            &jc,
            t("2026-11-27T00:14:40Z"),
            t("2026-11-27T00:15:10Z"),
            true,
            true,
            Duration::from_millis(1500),
        )
        .await;
        assert!(started.elapsed() < Duration::from_secs(4), "{:?}", started.elapsed());
        assert_eq!(
            u,
            Some(JournalRead::Read(vec![UnitOomEntry {
                unit: "x.scope".into(),
                realtime_usec: 1795738498277337,
                boot_id: "ab".into(),
            }]))
        );
        assert_eq!(k, Some(KernelRead::Unreadable));
    }
}
