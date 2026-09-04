//! A TCP socket-state census for one port, split by **which side owns the fd**.
//!
//! Plan `2026-08-31-devops-runner-9876-accept-path-starved-by-close-wait-sockets`,
//! Phase 1. The instrument, not a fix: the incident that prompted the plan had
//! two causal claims and BOTH were refuted by measurement, and the one finding
//! that survived is that **nothing in this fleet can observe a socket state at
//! all**. Neither hypothesis could be confirmed or killed from any shipped
//! surface, so both were argued from a hand-run `netstat` an operator typed
//! once. This module is the collection half of the surface that ends that.
//!
//! # The local/remote split is the whole point
//!
//! The incident's hand census grouped by "port 9876 appears **anywhere** in the
//! tuple". A loopback connection is TWO rows in the kernel's table — one per
//! endpoint — and on a loopback-only API *both* rows carry the port:
//!
//! ```text
//! State       Local Address:Port    Peer Address:Port
//! CLOSE-WAIT  127.0.0.1:9876        127.0.0.1:53718     <- the RUNNER owns this fd
//! CLOSE-WAIT  127.0.0.1:53718       127.0.0.1:9876      <- the PROBE owns this fd
//! ```
//!
//! Those two rows have **opposite owners and opposite meanings**. A population
//! of the first is a server that is not calling `close()`; a population of the
//! second is the *monitoring process itself* leaking descriptors while it
//! measures. Summing them reports the monitor's own bug as the runner's, which
//! is precisely how a refuted hypothesis survived long enough to become a plan.
//! [`SocketCensus::close_wait_local`] and [`SocketCensus::close_wait_remote`]
//! are therefore separate counters and must never be collapsed into one.
//!
//! # NULL is a value here, and it is not zero
//!
//! [`collect`] returns `Option<SocketCensus>` and returns `None` — never a
//! zero-filled struct — whenever the probe could not run or its output could
//! not be recognised: no `ss`/`netstat` on `PATH`, a spawn failure, a non-zero
//! exit, a timeout, output with no TCP rows, output whose state vocabulary we
//! do not recognise. "We did not measure" and "we measured zero" are different
//! facts, and the whole reason this plan exists is that the fleet had no way to
//! tell them apart. Same discipline as [`super::resource_sample::Saturation`]'s
//! "NULL, never 0" argument, one layer down.
//!
//! # Why the state vocabulary is checked before the counts are believed
//!
//! `netstat` **localises its state column** — on a German Windows install
//! `LISTENING` prints as `ABHÖREN` and `ESTABLISHED` as `HERGESTELLT`. A parser
//! keyed on English words therefore matches nothing at all on such a box and
//! would report a confident `CLOSE_WAIT = 0` for a machine drowning in them.
//! qontinui-supervisor's `process::netstat_parse` was written after exactly
//! that defect shipped (2026-07-31, a stop path with nothing to kill), and it
//! dodges the problem by keying on row *structure* — a listener is the only TCP
//! socket with a wildcard peer.
//!
//! **That trick does not extend to this question.** `CLOSE-WAIT`, `ESTAB` and
//! `TIME-WAIT` all name a real peer, so no structural feature separates them;
//! the localised word is the only discriminator there is. So instead of
//! trusting it blindly, the scanners require that **at least one** row in the
//! whole capture carries a state token from [`KNOWN_STATES`]. A live machine
//! always has some TCP socket in one of those states, so a capture where not
//! one row matched is a locale (or format) we cannot read — reported as `None`,
//! UNKNOWN, rather than as a row of zeroes.
//!
//! # Relationship to qontinui-supervisor's parser
//!
//! `qontinui-supervisor/src/process/netstat_parse.rs` answers a different
//! question ("which PID is LISTENING here"), lives in a different crate, and
//! cannot be imported. Its conventions are followed deliberately: pure
//! functions over captured text, unit-tested on fixtures so the ubuntu-only
//! merge gate can execute the Windows predicate, and unrecognised output
//! reported as a distinct outcome rather than as an empty answer.

use std::io::Read;
use std::process::Stdio;
use std::time::{Duration, Instant};

/// Wall-clock bound on the census subprocess.
///
/// The probe rides a 30 s publish loop on a blocking thread; it must never be
/// the thing that wedges it. On timeout the child is **killed** rather than
/// abandoned — an orphaned reader thread would be a slow leak on exactly the
/// axis the 2026-08-29 thread wedge sat on.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// How often the wait loop re-checks the child. Cheap enough to be invisible
/// against a 30 s cadence, fine enough that the common case (a few ms) is not
/// rounded up into a visible stall.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Hard cap on captured output. A machine with ~100k sockets prints a few MB;
/// anything past this is not a socket table and a truncated one must not be
/// counted, so hitting the cap is reported as UNKNOWN.
const MAX_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;

/// Which instrument produced a census.
///
/// The wire vocabulary lives here and only here, the same rule
/// [`super::resource_sample::Lane::as_str`] states: a second bare `"ss"`
/// literal elsewhere is how a renamed source becomes a silently unmatched
/// string instead of a compile error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CensusSource {
    /// Linux `ss -tan`.
    Ss,
    /// `netstat -an` (macOS/BSD) or `netstat -ano` (Windows).
    Netstat,
    /// **Never produced by [`collect`]** — it returns `None` instead, because a
    /// census with no counts is not a census. This variant exists for the
    /// PUBLISHER: a lane that ran the probe and got nothing publishes
    /// `sock_source = "unavailable"` with every counter NULL, which is a
    /// materially different statement from a lane that never probes at all
    /// (the `wsl` lane) and publishes nothing whatsoever. Without it the wire
    /// cannot distinguish "asked, no answer" from "never asked".
    Unavailable,
}

impl CensusSource {
    /// The wire string. `'ss' | 'netstat' | 'unavailable'`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            CensusSource::Ss => "ss",
            CensusSource::Netstat => "netstat",
            CensusSource::Unavailable => "unavailable",
        }
    }
}

/// A measured TCP socket-state census for one port.
///
/// Only ever constructed from a capture that actually parsed — see the module
/// docs on why the absence of one is `None` and not a zeroed instance of this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SocketCensus {
    /// The port the census is about. Carried so a reader never has to infer it
    /// from which runner published the row.
    pub(crate) probe_port: u16,
    /// `CLOSE-WAIT` sockets whose **local** port is [`Self::probe_port`] —
    /// server-side, i.e. fds this runner owns and has not closed. A rising
    /// population here is the only shape that is a runner close-side bug.
    pub(crate) close_wait_local: u32,
    /// `CLOSE-WAIT` sockets whose **remote** port is [`Self::probe_port`] —
    /// client-side, i.e. fds a process *talking to* the runner owns. A rising
    /// population here indicts the caller (very often the monitoring script),
    /// not the runner. Conflating this with [`Self::close_wait_local`] is the
    /// defect this whole module exists to make impossible.
    pub(crate) close_wait_remote: u32,
    /// `ESTAB` / `ESTABLISHED` sockets with the local port — live connections
    /// the runner is serving. The denominator that makes the CLOSE-WAIT counts
    /// interpretable.
    pub(crate) established_local: u32,
    /// `TIME-WAIT` sockets with the local port. Normal churn on a busy
    /// loopback API; published so a spike in it is not mistaken for a leak.
    pub(crate) time_wait_local: u32,
    /// Which instrument produced the four counts above.
    pub(crate) source: CensusSource,
}

/// The three states counted, plus everything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TcpState {
    CloseWait,
    Established,
    TimeWait,
    /// A state we recognise as English but do not count (`LISTEN`, `SYN-SENT`,
    /// …). Distinct from [`TcpState::Unrecognised`] because seeing one still
    /// proves the state column is readable.
    OtherKnown,
    /// A token we do not recognise at all — a localised state word, or a
    /// column that is not a state.
    Unrecognised,
}

/// Every English TCP state token `ss` and `netstat` are known to print, in the
/// normalised form [`classify_state`] produces (uppercase, `_` folded to `-`).
///
/// This is a **vocabulary probe**, not a classifier: its only job is to answer
/// "can we read this machine's state column at all". Missing an exotic member
/// costs nothing as long as the capture contains at least one that is present,
/// which on a live machine it always does.
const KNOWN_STATES: &[&str] = &[
    // Counted.
    "CLOSE-WAIT",
    "ESTAB",
    "ESTABLISHED",
    "TIME-WAIT",
    // Recognised but not counted.
    "LISTEN",
    "LISTENING",
    "SYN-SENT",
    "SYN-RECV",
    "SYN-RCVD",
    "SYN-RECEIVED",
    "FIN-WAIT-1",
    "FIN-WAIT-2",
    "FIN-WAIT1",
    "FIN-WAIT2",
    "LAST-ACK",
    "CLOSING",
    "CLOSED",
    "CLOSE",
    "UNCONN",
    "BOUND",
    "DELETE-TCB",
];

/// Normalise a state token and classify it.
///
/// Normalisation folds `_` to `-` and upper-cases, which is what makes one
/// table serve `ss` (`CLOSE-WAIT`, `ESTAB`) and `netstat` (`CLOSE_WAIT`,
/// `ESTABLISHED`) at once.
fn classify_state(raw: &str) -> TcpState {
    let normalised: String = raw
        .chars()
        .map(|c| {
            if c == '_' {
                '-'
            } else {
                c.to_ascii_uppercase()
            }
        })
        .collect();
    match normalised.as_str() {
        "CLOSE-WAIT" => TcpState::CloseWait,
        "ESTAB" | "ESTABLISHED" => TcpState::Established,
        "TIME-WAIT" => TcpState::TimeWait,
        other if KNOWN_STATES.contains(&other) => TcpState::OtherKnown,
        _ => TcpState::Unrecognised,
    }
}

/// Extract the port from an address token.
///
/// Handles every separator the three tools use: `:` for Linux `ss`/`netstat`
/// and Windows `netstat` (`127.0.0.1:9876`, `[::1]:9876`), and `.` for
/// macOS/BSD `netstat` (`127.0.0.1.9876`, `::1.9876`). Keyed on the LAST
/// separator of either kind, which is what makes the bracketed and
/// dotted-IPv6 forms fall out for free.
///
/// Wildcards (`0.0.0.0:*`, `*.*`, `*:*`) yield `None` — no port, not port 0.
fn endpoint_port(endpoint: &str) -> Option<u16> {
    let cut = endpoint.rfind([':', '.'])?;
    endpoint.get(cut + 1..)?.parse::<u16>().ok()
}

/// Running tallies plus the two facts that decide whether they may be believed.
struct Tally {
    close_wait_local: u32,
    close_wait_remote: u32,
    established_local: u32,
    time_wait_local: u32,
    /// At least one row parsed as a TCP row. Zero rows means the capture is not
    /// what we think it is (a wrapper's error text, a truncated read, a format
    /// change) — never "the machine has no sockets".
    saw_tcp_row: bool,
    /// At least one row carried a state token from [`KNOWN_STATES`]. Zero means
    /// the state column is in a language we cannot read; see the module docs.
    saw_known_state: bool,
}

impl Tally {
    fn new() -> Self {
        Self {
            close_wait_local: 0,
            close_wait_remote: 0,
            established_local: 0,
            time_wait_local: 0,
            saw_tcp_row: false,
            saw_known_state: false,
        }
    }

    /// Fold one parsed row in. `local`/`remote` are the row's endpoint ports
    /// (`None` for a wildcard peer).
    fn add_row(&mut self, state: TcpState, local: Option<u16>, remote: Option<u16>, port: u16) {
        self.saw_tcp_row = true;
        if state != TcpState::Unrecognised {
            self.saw_known_state = true;
        }
        let is_local = local == Some(port);
        let is_remote = remote == Some(port);
        match state {
            // The split. Both arms are evaluated independently rather than as
            // an if/else: a row is counted on whichever side(s) actually carry
            // the port, and a hypothetical self-connection is honestly both.
            TcpState::CloseWait => {
                if is_local {
                    self.close_wait_local = self.close_wait_local.saturating_add(1);
                }
                if is_remote {
                    self.close_wait_remote = self.close_wait_remote.saturating_add(1);
                }
            }
            TcpState::Established => {
                if is_local {
                    self.established_local = self.established_local.saturating_add(1);
                }
            }
            TcpState::TimeWait => {
                if is_local {
                    self.time_wait_local = self.time_wait_local.saturating_add(1);
                }
            }
            TcpState::OtherKnown | TcpState::Unrecognised => {}
        }
    }

    /// Seal the tally into a census, or `None` if the capture failed either
    /// readability test.
    fn finish(self, probe_port: u16, source: CensusSource) -> Option<SocketCensus> {
        if !self.saw_tcp_row || !self.saw_known_state {
            return None;
        }
        Some(SocketCensus {
            probe_port,
            close_wait_local: self.close_wait_local,
            close_wait_remote: self.close_wait_remote,
            established_local: self.established_local,
            time_wait_local: self.time_wait_local,
            source,
        })
    }
}

/// Scan captured `ss -tan` output.
///
/// Layout: `State Recv-Q Send-Q Local-Address:Port Peer-Address:Port [Process]`.
/// The header (`State  Recv-Q  Send-Q  Local Address:Port  Peer Address:Port`)
/// splits into tokens whose 4th is the literal `Address:Port`, which fails the
/// port parse and so is skipped without being counted as a row.
///
/// Compiled on every platform on purpose: CI is ubuntu-only, so a predicate
/// that only exists behind a `cfg` the gate never sets is a predicate that can
/// rot silently.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn scan_ss(text: &str, probe_port: u16) -> Option<SocketCensus> {
    let mut tally = Tally::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 5 {
            continue;
        }
        // A row is a row when its LOCAL endpoint carries a port. The peer may
        // legitimately be a wildcard (`0.0.0.0:*` on a LISTEN row), so it is
        // allowed to be `None` and must not disqualify the row — a machine
        // holding only listeners still proves the capture is readable.
        let Some(local) = endpoint_port(fields[3]) else {
            continue;
        };
        let remote = endpoint_port(fields[4]);
        tally.add_row(classify_state(fields[0]), Some(local), remote, probe_port);
    }
    tally.finish(probe_port, CensusSource::Ss)
}

/// Scan captured unix `netstat -an` output (Linux net-tools and macOS/BSD).
///
/// Layout: `Proto Recv-Q Send-Q Local-Address Foreign-Address [State]`. The
/// proto column is the row filter (`tcp`, `tcp4`, `tcp6`, `tcp46`), which also
/// excludes the UDP and unix-domain sections netstat prints below the TCP one.
/// A TCP row with no state column at all is folded in as unrecognised — it is
/// evidence the table is readable but says nothing about a state.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn scan_netstat_unix(text: &str, probe_port: u16) -> Option<SocketCensus> {
    let mut tally = Tally::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 5 {
            continue;
        }
        if !fields[0].to_ascii_lowercase().starts_with("tcp") {
            continue;
        }
        let Some(local) = endpoint_port(fields[3]) else {
            continue;
        };
        let remote = endpoint_port(fields[4]);
        let state = fields
            .get(5)
            .map(|s| classify_state(s))
            .unwrap_or(TcpState::Unrecognised);
        tally.add_row(state, Some(local), remote, probe_port);
    }
    tally.finish(probe_port, CensusSource::Netstat)
}

/// Scan captured Windows `netstat -ano` output.
///
/// Layout: `Proto Local-Address Foreign-Address State... PID`. Anchored from
/// BOTH ends exactly as qontinui-supervisor's `netstat_parse::scan_listeners`
/// is, because some locales print a MULTI-WORD state (`A L'ECOUTE`) and a fixed
/// field count would mis-parse those rows rather than merely failing to
/// classify them.
///
/// Multi-word states are joined with `-` before classification, which is a
/// no-op for every English state (all single tokens) and keeps a localised
/// multi-word state landing in [`TcpState::Unrecognised`] — where, if no row in
/// the whole capture is readable, it correctly produces `None`.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn scan_netstat_windows(text: &str, probe_port: u16) -> Option<SocketCensus> {
    let mut tally = Tally::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 || !fields[0].eq_ignore_ascii_case("TCP") {
            continue;
        }
        // The trailing field is the owning PID. A row whose last field is not a
        // number is a wrapped header or a banner, not a data row.
        if fields[fields.len() - 1].parse::<u32>().is_err() {
            continue;
        }
        let Some(local) = endpoint_port(fields[1]) else {
            continue;
        };
        let remote = endpoint_port(fields[2]);
        let state_tokens = &fields[3..fields.len() - 1];
        let state = if state_tokens.is_empty() {
            TcpState::Unrecognised
        } else {
            classify_state(&state_tokens.join("-"))
        };
        tally.add_row(state, Some(local), remote, probe_port);
    }
    tally.finish(probe_port, CensusSource::Netstat)
}

/// Run a probe command and capture its stdout, bounded by [`PROBE_TIMEOUT`].
///
/// `None` on every failure mode there is: the tool is not on `PATH`, the spawn
/// failed, the child outlived the timeout (it is killed), it exited non-zero,
/// or it produced more than [`MAX_OUTPUT_BYTES`] (a truncated table must not be
/// counted). Never panics — a panicked reader thread is a `join` error, which
/// is also `None`.
///
/// stdout is drained on a dedicated thread rather than polled inline because a
/// full pipe buffer would otherwise deadlock the wait loop against a child that
/// cannot finish writing. That thread is bounded by construction: killing the
/// child closes the write end, which is the reader's EOF.
fn run_bounded(program: &str, args: &[&str]) -> Option<String> {
    let mut cmd = crate::process_helpers::no_window(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = cmd.spawn().ok()?;
    let Some(mut pipe) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };

    let reader = std::thread::spawn(move || {
        let mut buf: Vec<u8> = Vec::new();
        // `+ 1` so hitting the cap is DETECTABLE (`len > MAX`) rather than
        // indistinguishable from an output that happens to be exactly the cap.
        let _ = (&mut pipe).take(MAX_OUTPUT_BYTES + 1).read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + PROBE_TIMEOUT;
    let mut status = None;
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                status = Some(st);
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }

    // Joined unconditionally, including on the timeout path: the kill above
    // closed the pipe, so this returns promptly and leaves no orphan thread.
    let bytes = reader.join().ok()?;

    // A non-zero exit, a timeout and an over-cap capture are all UNKNOWN. In
    // particular a partial table would produce plausible-looking counts that
    // are simply wrong, which is worse than no counts at all.
    if !status?.success() || bytes.len() as u64 > MAX_OUTPUT_BYTES {
        return None;
    }
    // Every field read is ASCII; a localised state column may not be valid
    // UTF-8 (netstat writes the console's OEM codepage on Windows), so a lossy
    // decode is exact for our purposes and never drops a row.
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Take a socket census for `probe_port`, or `None` if it could not be taken.
///
/// Linux prefers `ss -tan` and falls back to `netstat -an` where `ss` is
/// missing or unreadable; other unix goes straight to `netstat -an`; Windows
/// uses `netstat -ano`.
///
/// Blocking (one short-lived subprocess), so callers run it on a blocking pool
/// — [`super::resource_sample::collect_host_lane`] already is one. Takes no
/// lock, holds no state, and cannot outlive [`PROBE_TIMEOUT`].
pub(crate) fn collect(probe_port: u16) -> Option<SocketCensus> {
    #[cfg(windows)]
    {
        let text = run_bounded("netstat", &["-ano"])?;
        scan_netstat_windows(&text, probe_port)
    }
    #[cfg(not(windows))]
    {
        // `-t` restricts to TCP, `-a` includes listeners, `-n` keeps ports
        // numeric (a resolved service name would defeat `endpoint_port`).
        if let Some(text) = run_bounded("ss", &["-tan"]) {
            if let Some(census) = scan_ss(&text, probe_port) {
                return Some(census);
            }
        }
        let text = run_bounded("netstat", &["-an"])?;
        scan_netstat_unix(&text, probe_port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ss -tan` as Linux prints it, carrying BOTH sides of the split: a
    /// server-side CLOSE-WAIT (local `:9876`) and a client-side one (remote
    /// `:9876`), plus the established/time-wait denominators and rows for other
    /// ports.
    const SS_BOTH_SIDES: &str = "\
State      Recv-Q Send-Q      Local Address:Port       Peer Address:Port
LISTEN     0      1024           127.0.0.1:9876             0.0.0.0:*
ESTAB      0      0              127.0.0.1:9876           127.0.0.1:53716
ESTAB      0      0              127.0.0.1:53716            127.0.0.1:9876
CLOSE-WAIT 1      0              127.0.0.1:9876           127.0.0.1:53718
CLOSE-WAIT 1      0              127.0.0.1:53720            127.0.0.1:9876
CLOSE-WAIT 1      0              127.0.0.1:53722            127.0.0.1:9876
TIME-WAIT  0      0              127.0.0.1:9876           127.0.0.1:53724
ESTAB      0      0              127.0.0.1:5432             127.0.0.1:44100
CLOSE-WAIT 1      0              127.0.0.1:8000             127.0.0.1:44102
";

    /// Windows `netstat -ano`, English locale, same shape as the `ss` fixture.
    const NETSTAT_WINDOWS_BOTH_SIDES: &str = "\r
Active Connections\r
\r
  Proto  Local Address          Foreign Address        State           PID\r
  TCP    0.0.0.0:135            0.0.0.0:0              LISTENING       1288\r
  TCP    127.0.0.1:9876         0.0.0.0:0              LISTENING       8872\r
  TCP    127.0.0.1:9876         127.0.0.1:53716        ESTABLISHED     8872\r
  TCP    127.0.0.1:9876         127.0.0.1:53718        CLOSE_WAIT      8872\r
  TCP    127.0.0.1:53720        127.0.0.1:9876         CLOSE_WAIT      33812\r
  TCP    127.0.0.1:53722        127.0.0.1:9876         CLOSE_WAIT      33812\r
  TCP    127.0.0.1:9876         127.0.0.1:53724        TIME_WAIT       0\r
  TCP    127.0.0.1:8000         127.0.0.1:44102        CLOSE_WAIT      4711\r
  UDP    0.0.0.0:5353           *:*                                    3120\r
";

    /// A German-locale `netstat -ano` capture, in the shape qontinui-supervisor
    /// recorded verbatim from the box where the 2026-07-31 stop-path defect was
    /// diagnosed. Every state word is localised.
    const NETSTAT_WINDOWS_GERMAN: &str = "\r
Aktive Verbindungen\r
\r
  Proto  Lokale Adresse         Remoteadresse          Status           PID\r
  TCP    127.0.0.1:9876         0.0.0.0:0              ABHÖREN         8872\r
  TCP    127.0.0.1:9876         127.0.0.1:53716        HERGESTELLT     8872\r
  TCP    127.0.0.1:9876         127.0.0.1:53718        SCHLIESSEN_WARTEN 8872\r
  TCP    127.0.0.1:57700        127.0.0.1:9876         WARTEND         0\r
";

    /// THE Phase 1 regression, on Linux. Server-side and client-side CLOSE-WAIT
    /// must land in DIFFERENT counters.
    ///
    /// The incident's hand census grouped by "9876 appears anywhere in the
    /// tuple" and so reported 3 here — a number that cannot distinguish a
    /// runner that is not closing fds from a monitoring script that is not.
    /// Both hypotheses it fed were later refuted by measurement; neither could
    /// have been settled from a single summed count.
    #[test]
    fn ss_keeps_server_side_and_client_side_close_wait_apart() {
        let c = scan_ss(SS_BOTH_SIDES, 9876).expect("a readable ss capture is measured");
        assert_eq!(c.close_wait_local, 1, "the runner owns exactly one such fd");
        assert_eq!(
            c.close_wait_remote, 2,
            "two PROBE-owned fds, not the runner's"
        );
        assert_ne!(
            c.close_wait_local, c.close_wait_remote,
            "the two counters must never be the same number by construction"
        );
        assert_eq!(c.established_local, 1);
        assert_eq!(c.time_wait_local, 1);
        assert_eq!(c.probe_port, 9876);
        assert_eq!(c.source, CensusSource::Ss);
    }

    /// The same regression on Windows. The split is a property of the census,
    /// not of one parser, so it is asserted once per format.
    #[test]
    fn windows_netstat_keeps_server_side_and_client_side_close_wait_apart() {
        let c = scan_netstat_windows(NETSTAT_WINDOWS_BOTH_SIDES, 9876)
            .expect("a readable netstat capture is measured");
        assert_eq!(c.close_wait_local, 1);
        assert_eq!(c.close_wait_remote, 2);
        assert_ne!(c.close_wait_local, c.close_wait_remote);
        assert_eq!(c.established_local, 1);
        assert_eq!(c.time_wait_local, 1);
        assert_eq!(c.probe_port, 9876);
        assert_eq!(c.source, CensusSource::Netstat);
    }

    /// Both parsers must agree on the same machine state — a fixture pair
    /// describing one box may not produce two different answers.
    #[test]
    fn the_two_formats_agree_on_the_same_machine() {
        let ss = scan_ss(SS_BOTH_SIDES, 9876).expect("ss parses");
        let ns = scan_netstat_windows(NETSTAT_WINDOWS_BOTH_SIDES, 9876).expect("netstat parses");
        assert_eq!(ss.close_wait_local, ns.close_wait_local);
        assert_eq!(ss.close_wait_remote, ns.close_wait_remote);
        assert_eq!(ss.established_local, ns.established_local);
        assert_eq!(ss.time_wait_local, ns.time_wait_local);
    }

    /// Rows for OTHER ports are excluded from every counter. Both fixtures
    /// carry a CLOSE-WAIT on a different port precisely so this can fail.
    #[test]
    fn rows_for_other_ports_are_excluded() {
        // Port 5432 has one ESTAB row (as a local endpoint) and nothing else.
        let c = scan_ss(SS_BOTH_SIDES, 5432).expect("parses");
        assert_eq!(c.established_local, 1);
        assert_eq!(c.close_wait_local, 0);
        assert_eq!(c.close_wait_remote, 0);
        assert_eq!(c.time_wait_local, 0);

        // Port 8000's single CLOSE-WAIT is server-side and must not leak into
        // the 9876 census, nor 9876's rows into its.
        let c = scan_ss(SS_BOTH_SIDES, 8000).expect("parses");
        assert_eq!(c.close_wait_local, 1);
        assert_eq!(c.close_wait_remote, 0);
        assert_eq!(c.established_local, 0);

        let c = scan_netstat_windows(NETSTAT_WINDOWS_BOTH_SIDES, 8000).expect("parses");
        assert_eq!(c.close_wait_local, 1);
        assert_eq!(c.close_wait_remote, 0);
    }

    /// A port with no rows at all is a MEASURED ZERO — the capture parsed, the
    /// machine simply has nothing on that port. This is the one case where
    /// zeroes are the honest answer, and it is what makes the `None` cases
    /// below meaningful rather than a blanket refusal.
    #[test]
    fn a_readable_capture_with_no_rows_for_the_port_is_a_measured_zero() {
        let c = scan_ss(SS_BOTH_SIDES, 9999).expect("the capture is readable");
        assert_eq!(c.close_wait_local, 0);
        assert_eq!(c.close_wait_remote, 0);
        assert_eq!(c.established_local, 0);
        assert_eq!(c.time_wait_local, 0);
    }

    /// Unparseable output is UNKNOWN, **never** `Some(zeros)`. A zero-filled
    /// census would render a box drowning in CLOSE-WAIT as perfectly healthy on
    /// the one axis built to catch it — the exact conflation this plan exists
    /// to end.
    #[test]
    fn unparseable_output_is_unknown_not_zero() {
        for text in [
            "ss: command not found",
            "bash: netstat: command not found\n",
            "<html><body>500</body></html>",
        ] {
            assert_eq!(scan_ss(text, 9876), None, "ss scanner on {text:?}");
            assert_eq!(
                scan_netstat_unix(text, 9876),
                None,
                "unix netstat scanner on {text:?}"
            );
            assert_eq!(
                scan_netstat_windows(text, 9876),
                None,
                "windows netstat scanner on {text:?}"
            );
        }
    }

    /// Empty output is the degenerate case of the same rule: a probe that
    /// produced nothing has told us nothing.
    #[test]
    fn empty_output_is_unknown_not_zero() {
        assert_eq!(scan_ss("", 9876), None);
        assert_eq!(scan_netstat_unix("", 9876), None);
        assert_eq!(scan_netstat_windows("", 9876), None);
    }

    /// A header with no data rows is UNKNOWN too — a live machine always has
    /// TCP sockets, so zero rows means the capture is not what we think it is.
    #[test]
    fn header_only_output_is_unknown_not_zero() {
        let ss_header =
            "State      Recv-Q Send-Q      Local Address:Port       Peer Address:Port\n";
        assert_eq!(scan_ss(ss_header, 9876), None);
        let win_header =
            "\nActive Connections\n\n  Proto  Local Address  Foreign Address  State  PID\n";
        assert_eq!(scan_netstat_windows(win_header, 9876), None);
    }

    /// A LOCALISED state column is UNKNOWN, not zero.
    ///
    /// This is the trap qontinui-supervisor's `netstat_parse` was written after
    /// and the one its structural trick cannot help with here: CLOSE-WAIT,
    /// ESTABLISHED and TIME-WAIT all name a real peer, so the localised word is
    /// the only discriminator. A parser keyed on English words silently reports
    /// `CLOSE_WAIT = 0` on every non-English Windows in the fleet; refusing to
    /// answer is the only honest alternative.
    #[test]
    fn a_localised_state_column_is_unknown_not_zero() {
        assert_eq!(
            scan_netstat_windows(NETSTAT_WINDOWS_GERMAN, 9876),
            None,
            "a German capture parses as ROWS but not as STATES — reporting \
             zeroes here would call a saturated box healthy"
        );
    }

    /// The bare minimum a live English capture needs to be believed: one
    /// recognised state token anywhere. A capture of nothing but listeners is
    /// readable and measures zero CLOSE-WAIT.
    #[test]
    fn a_listener_only_capture_is_readable() {
        let text = "\
State  Recv-Q Send-Q Local Address:Port Peer Address:Port
LISTEN 0      1024       127.0.0.1:9876          0.0.0.0:*
LISTEN 0      128          0.0.0.0:22            0.0.0.0:*
";
        let c = scan_ss(text, 9876).expect("listeners prove the column is readable");
        assert_eq!(c.close_wait_local, 0);
        assert_eq!(c.established_local, 0);
    }

    /// macOS/BSD `netstat -an` writes ports after a DOT and protos as
    /// `tcp4`/`tcp6`. The split must survive that, since it is the fallback
    /// path on every non-Linux unix.
    #[test]
    fn bsd_dotted_endpoints_and_tcp4_protos_parse() {
        let text = "\
Active Internet connections (including servers)
Proto Recv-Q Send-Q  Local Address          Foreign Address        (state)
tcp4       0      0  127.0.0.1.9876         127.0.0.1.53716        ESTABLISHED
tcp4       1      0  127.0.0.1.9876         127.0.0.1.53718        CLOSE_WAIT
tcp4       1      0  127.0.0.1.53720        127.0.0.1.9876         CLOSE_WAIT
tcp4       0      0  127.0.0.1.9876         *.*                    LISTEN
tcp6       0      0  ::1.9876               ::1.53730              CLOSE_WAIT
udp4       0      0  *.5353                 *.*
";
        let c = scan_netstat_unix(text, 9876).expect("parses");
        assert_eq!(
            c.close_wait_local, 2,
            "one IPv4 and one IPv6 server-side fd"
        );
        assert_eq!(c.close_wait_remote, 1);
        assert_eq!(c.established_local, 1);
        assert_eq!(c.source, CensusSource::Netstat);
    }

    /// Linux `netstat -an` (net-tools) is the `ss`-less fallback on Linux and
    /// uses colons plus a bare `tcp` proto. The unix scanner must serve both.
    #[test]
    fn linux_net_tools_netstat_parses() {
        let text = "\
Active Internet connections (servers and established)
Proto Recv-Q Send-Q Local Address           Foreign Address         State
tcp        0      0 127.0.0.1:9876          0.0.0.0:*               LISTEN
tcp        1      0 127.0.0.1:9876          127.0.0.1:53718         CLOSE_WAIT
tcp        1      0 127.0.0.1:53720         127.0.0.1:9876          CLOSE_WAIT
tcp6       0      0 :::22                   :::*                    LISTEN
unix  2      [ ACC ]     STREAM     LISTENING     12345 /run/foo.sock
";
        let c = scan_netstat_unix(text, 9876).expect("parses");
        assert_eq!(c.close_wait_local, 1);
        assert_eq!(c.close_wait_remote, 1);
    }

    /// The Windows parser is anchored from both ends, so a multi-word state
    /// column must not shift the address columns. It is still unrecognised
    /// (localised), which is the whole point of the vocabulary probe.
    #[test]
    fn a_multi_word_state_does_not_shift_the_address_columns() {
        let text = "\
  TCP    127.0.0.1:9876         127.0.0.1:53716        ESTABLISHED     8872\r
  TCP    127.0.0.1:9876         0.0.0.0:0              A L'ECOUTE      8872\r
";
        // Readable overall (the ESTABLISHED row), and the multi-word row is
        // simply not counted rather than mis-parsed into another counter.
        let c = scan_netstat_windows(text, 9876).expect("parses");
        assert_eq!(c.established_local, 1);
        assert_eq!(c.close_wait_local, 0);
        assert_eq!(c.time_wait_local, 0);
    }

    /// `ss` and `netstat` spell the same states differently (`ESTAB` vs
    /// `ESTABLISHED`, `-` vs `_`). One normalisation must cover both, or the
    /// two sources publish different numbers for one machine.
    #[test]
    fn state_spellings_normalise_across_both_tools() {
        assert_eq!(classify_state("CLOSE-WAIT"), TcpState::CloseWait);
        assert_eq!(classify_state("CLOSE_WAIT"), TcpState::CloseWait);
        assert_eq!(classify_state("close_wait"), TcpState::CloseWait);
        assert_eq!(classify_state("ESTAB"), TcpState::Established);
        assert_eq!(classify_state("ESTABLISHED"), TcpState::Established);
        assert_eq!(classify_state("TIME-WAIT"), TcpState::TimeWait);
        assert_eq!(classify_state("TIME_WAIT"), TcpState::TimeWait);
        assert_eq!(classify_state("LISTEN"), TcpState::OtherKnown);
        assert_eq!(classify_state("LISTENING"), TcpState::OtherKnown);
        assert_eq!(classify_state("ABHÖREN"), TcpState::Unrecognised);
        assert_eq!(classify_state("Recv-Q"), TcpState::Unrecognised);
    }

    #[test]
    fn endpoint_port_handles_every_separator_the_three_tools_use() {
        assert_eq!(endpoint_port("127.0.0.1:9876"), Some(9876));
        assert_eq!(endpoint_port("[::1]:9876"), Some(9876));
        assert_eq!(endpoint_port("[::]:0"), Some(0));
        assert_eq!(endpoint_port("0.0.0.0:0"), Some(0));
        // macOS/BSD dotted form, v4 and v6.
        assert_eq!(endpoint_port("127.0.0.1.9876"), Some(9876));
        assert_eq!(endpoint_port("::1.9876"), Some(9876));
        // Wildcards carry no port — `None`, not `Some(0)`.
        assert_eq!(endpoint_port("0.0.0.0:*"), None);
        assert_eq!(endpoint_port("*.*"), None);
        assert_eq!(endpoint_port("*:*"), None);
        assert_eq!(endpoint_port("Address:Port"), None);
        assert_eq!(endpoint_port("nonsense"), None);
    }

    /// The wire vocabulary is pinned here because a sibling change adds these
    /// exact three strings to coord's ingest and to a DB column; a rename on
    /// either side lands as an unmatched value, not an error.
    #[test]
    fn the_source_vocabulary_is_exactly_three_strings() {
        assert_eq!(CensusSource::Ss.as_str(), "ss");
        assert_eq!(CensusSource::Netstat.as_str(), "netstat");
        assert_eq!(CensusSource::Unavailable.as_str(), "unavailable");
    }

    /// `collect` must never be able to hand back `Unavailable` — a census with
    /// a source but no counts is the zero-filled struct this module forbids.
    /// The variant is the PUBLISHER's, and this pins that division structurally
    /// so a future edit cannot quietly move it.
    #[test]
    fn collect_never_returns_the_unavailable_source() {
        const SRC: &str = include_str!("socket_census.rs");
        let prod = SRC
            .split_once("\n#[cfg(test)]")
            .map(|(before, _)| before)
            .unwrap_or(SRC);
        let start = prod
            .find("pub(crate) fn collect(")
            .expect("the collector must exist");
        assert!(
            !prod[start..].contains("Unavailable"),
            "collect() must return None for an unmeasured census, never a \
             census carrying CensusSource::Unavailable"
        );
    }

    /// The probe must be bounded and non-blocking-forever by construction.
    /// Structural, because a wedged `ss` cannot be produced deterministically.
    #[test]
    fn the_probe_is_bounded_and_kills_what_it_times_out_on() {
        const SRC: &str = include_str!("socket_census.rs");
        let prod = SRC
            .split_once("\n#[cfg(test)]")
            .map(|(before, _)| before)
            .unwrap_or(SRC);
        let start = prod.find("fn run_bounded(").expect("the runner must exist");
        let rest = &prod[start..];
        let end = rest
            .find("\n/// Take a socket census")
            .unwrap_or(rest.len());
        let body = &rest[..end];
        assert!(
            body.contains("PROBE_TIMEOUT"),
            "the subprocess must be bounded by a deadline"
        );
        assert!(
            body.contains("child.kill()"),
            "a timed-out child must be killed, not abandoned — an orphan holds \
             a thread on the axis the 2026-08-29 wedge sat on"
        );
        assert!(
            body.contains("reader.join()"),
            "the drain thread must be joined so the probe leaks no threads"
        );
        assert!(
            PROBE_TIMEOUT.as_secs() > 0 && PROBE_TIMEOUT.as_secs() <= 10,
            "a census on a 30s loop must not be able to eat the tick"
        );
    }
}
