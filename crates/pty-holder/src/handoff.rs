//! In-place holder upgrade (Unix only): hand a live pane to a successor
//! holder process without its child noticing.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 8 and D12a: **spawn with an inherited fd, ack before dispose** — not
//! `execve` (a bad successor binary would cost the pane, with no predecessor
//! left to fall back to) and not `SCM_RIGHTS`. A ConPTY handle cannot be handed
//! to another process (D12), so there is no Windows half: a Windows holder
//! answers `prepare_upgrade` with `unsupported`, and an incompatible or
//! defective holder there is left running or ended and resumed (Phase 6).
//!
//! # The exchange
//!
//! PREDECESSOR (the running holder, on a `prepare_upgrade` request):
//!
//! 1. **Freeze input** ([`Pane::freeze_input`]): what is queued for the child
//!    is written out first; input arriving from now on is HELD.
//! 2. **Quiesce reads at offset X** ([`Pane::quiesce_reads`], D12a rule 1):
//!    the reader parks between two reads, so every byte before X is in the
//!    ring and none after it has left the kernel. Two processes reading one
//!    master would split its stream; the kernel buffers instead (at worst the
//!    child blocks on a full PTY) until exactly one of them resumes at X.
//! 3. **Snapshot** — pane metadata, the child's pid and start token, the
//!    mode-tracker state, the ring and X — to `<pane-id>.upgrade`, written to a
//!    temp file (mode 0600, `O_NOFOLLOW`, `O_EXCL`) and renamed over.
//! 4. **Spawn** the successor: this holder's own executable with
//!    `--adopt <snapshot>`, and three inherited fds at fixed numbers —
//!    [`SUCCESSOR_MASTER_FD`] (the PTY master), [`SUCCESSOR_LOCK_FD`] (the pane
//!    lock's open file description, so the lock is never free during the swap
//!    and a prober never reads the pane as dead) and [`SUCCESSOR_CONTROL_FD`]
//!    (one end of a socketpair, the control channel). Each is `dup2`'d in
//!    `pre_exec` from a high-numbered copy, and `FD_CLOEXEC` is cleared on the
//!    target explicitly (D12a rule 4: `portable-pty` sets it on the master).
//! 5. **Wait for `ack {successor_pid}`** under [`ACK_TIMEOUT`].
//!
//! On the ack it **commits**:
//!
//! 6. **Drain input** ([`Pane::drain_input_for_handoff`]): new input
//!    connections are refused before their `ok` and the hold cap is lifted.
//!    **Linux:** every open input connection is `shutdown(SHUT_RD)` — what
//!    its socket already carries is read into the held input, after which a
//!    client's write to it FAILS (`EPIPE`) rather than vanishing. **Other
//!    Unix:** `SHUT_RD` there may silently discard, so nothing is shut down;
//!    an input connection still open after the drain bound fails the drain
//!    and the upgrade is rolled back with that reason.
//! 7. **Commit**: sends `commit` with the held input (and the child's exit,
//!    if the child exited mid-swap and this holder — its parent — reaped it),
//!    then retires its reader for good ([`Pane::hand_off_reads`]).
//! 8. **Wait for `serving`** — the successor's report that its endpoint is in
//!    place — at most [`SERVING_WAIT`], with its own reader still PARKED. If
//!    the successor dies in that wait (`try_wait` proves it: it is this
//!    holder's child), it had read nothing and written no held input — it
//!    does both only after `serving` — so this holder takes the pane back:
//!    it restores its lock record, re-binds its endpoint if the successor's
//!    rename had replaced it, returns the held input to its writer, resumes
//!    at X, and answers `upgrade_failed`. A successor found dead with a
//!    `serving` already QUEUED had taken over (it may have written the held
//!    input and read output past X): the pane is taken back all the same —
//!    the child must keep a holder — but the held input is NOT replayed, an
//!    in-stream notice declares that output since X may be missing
//!    ([`upgrade_loss_notice`]), and the log says so at error level. X is
//!    recorded (`Pane::record_stream_replaced_at`): a client that had attached
//!    to the dead successor holds an offset on ITS stream, so any attach past
//!    X gets an explicit `output_lost` from X, never this holder's own bytes
//!    replayed at a misaligned offset (H1, coord finding f4672ed2;
//!    `server::attach_start`). Otherwise it answers the requester
//!    `upgraded` and exits with `_exit`: closing its copy of the master WITHOUT signalling the
//!    child (the child is a session leader on its own PTY; the holder is not
//!    its controlling process, so nothing hangs it up), and running none of the
//!    pane-end work a normal exit does. A `serving` that does not come in time
//!    is logged and the exit happens anyway: the commit is final.
//!
//! On ANYTHING before the commit is delivered — `nak`, the ack timeout, the
//! successor's exit, a closed channel, a wrong pid, a failed drain, a failed
//! commit write — it **rolls back**: SIGKILLs and reaps the successor (which
//! never read the master — it reads only after a complete `commit`), unlinks
//! the snapshot and the successor's staged socket, resumes reading at X, and
//! releases the held input to its own writer. The pane is served exactly as
//! before (D12a rule 2).
//!
//! SUCCESSOR ([`adopt`], `main --adopt`): consumes the snapshot (unlinked on
//! open), verifies the inherited lock (`PaneLock::adopt_inherited`), the master
//! (`TIOCGWINSZ`) and that the child is still the process its start token
//! names, builds an adopted [`Pane`] whose reader starts PARKED, and binds its
//! endpoint at a STAGED path beside the pane's socket — and only then acks;
//! any failure is a `nak` and an exit that signals nothing. It then waits for
//! `commit` with NO timeout of its own while its predecessor lives: a
//! predecessor that gives up always kills it, so a timeout here could only
//! race a late commit and lose the pane. On `commit` it queues the held input,
//! rewrites the lock record to name itself (`PaneLock::rewrite_record`, never
//! an empty file; on failure the record is CLEARED, which reads UNKNOWN —
//! nothing is killed), `rename`s its staged socket over the pane's endpoint,
//! reports `serving` — only if the rename succeeded — and only THEN queues the
//! held input and resumes reading at X. After a failed rename it sends no
//! `serving`, and waits for the predecessor to leave before it reads. Nothing
//! after the commit can end the child.
//!
//! **The endpoint gap.** Until the rename, the path names the predecessor's
//! socket, which answers until the predecessor exits — and it exits only after
//! `serving` (or [`SERVING_WAIT`]). So a client reaches one holder or the
//! other, except when `serving` times out: then, between the predecessor's
//! exit and the successor's rename, a connect is refused. A prober never reads
//! that as a dead pane — the lock is held throughout — but as UNKNOWN, and the
//! runner's `DaemonPaneIo` retries. A connected client's connection to the
//! predecessor ends at its exit; the runner probes, finds the lock held and the
//! endpoint answering, and reattaches at its own last offset — so a late
//! attach sees both sides of the swap with no loss marker and no duplicate.
//!
//! If the PREDECESSOR dies after the ack but before a complete `commit`, the
//! successor takes the pane over anyway — with whatever held input it had
//! received: a predecessor that rolled back always kills and reaps its
//! successor before it reads again, so a successor that is alive while its
//! parent is gone knows nobody else reads the master.
//!
//! # What an adopted pane cannot know
//!
//! The successor is not the child's parent. Its exit detection is master
//! EOF/`EIO`, and its exit code is `None` — UNKNOWN, never 0 (D12a rule 3;
//! the runner's `exit_result` renders it as unknown) — unless the child exited
//! during the swap and the commit carried the code. A `kill` is preceded by a
//! start-token check, since the pid is no longer pinned by a zombie.
//!
//! # Input across the swap — the residual bound
//!
//! **Effectively Linux-only.** On macOS/BSD the drain refuses whenever an
//! input connection is open, and a runner keeps its pane's input connection
//! open for the pane's whole life — so there, in practice, every upgrade of a
//! pane a runner is attached to rolls back. The feature is unavailable on
//! those platforms until a drain that does not depend on `SHUT_RD` exists.
//!
//! **This bound is established for Linux**, whose `SHUT_RD` keeps queued data
//! readable and fails later writes; on other Unix an upgrade with an open
//! input connection is refused instead (see step 6). On Linux, every input
//! byte a client's write DELIVERED to a predecessor socket before the drain's
//! `shutdown(SHUT_RD)` reaches the child, in order: frames queued
//! to the writer are written first (the freeze waits for that), frames that
//! arrive later are held — over the normal hold cap they wait, and during the
//! drain the cap is lifted, so none is dropped — and travel in `commit`. What
//! is NOT delivered: a frame whose write the shutdown cut in half (discarded
//! whole) and any write after it. Both FAIL at the writer (`EPIPE`), never
//! silently — and a write that fails before the frame's last byte left the
//! client PROVES the holder does not have that frame
//! (`client::InputFailure::not_delivered`). The runner's `HolderConn::input`
//! resends such a frame, and the rest of its write, exactly once on a
//! reopened input connection — to the successor, or to this holder after a
//! rollback — so the swap is lossless for input; a frame whose delivery is
//! unknown is reported, never resent. A predecessor that dies mid-commit
//! loses the held frames it had not yet sent. **A rollback after the drain**
//! (the commit write failed, or the successor died before `serving`) keeps
//! every held frame, but the drained connections stay shut: writes a client
//! makes on them after the drain fail at the writer exactly as in a swap,
//! and so do `open_input` attempts refused while the commit was in flight —
//! the same residual, fixed by the same runner-side resend.
//!
//! DATA-PATH module: `source_guard` bans text decoding here.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::frame::{read_frame, write_frame, KIND_CONTROL, KIND_DATA};
use crate::lock::{LockRecord, PaneLock};
use crate::modes::{ModeSnapshot, ModeTracker};
use crate::pane::{
    check_private_dir, lock_path, log_path, snapshot_path, snapshot_tmp_path, PaneId,
};
use crate::protocol::{holder_build, ExitReply, Reply, PROTOCOL_VERSIONS};
use crate::pty::{dup_cloexec, get_winsize, AdoptedParts, Pane};
use crate::ring::OutputRing;
use crate::server::{log_line, Holder, HolderInfo};
use crate::spec::WireOs;

/// The successor's PTY master.
pub const SUCCESSOR_MASTER_FD: RawFd = 3;
/// The successor's copy of the pane lock (the predecessor's open file
/// description: the `flock` is shared, never released during the swap).
pub const SUCCESSOR_LOCK_FD: RawFd = 4;
/// The successor's end of the control channel.
pub const SUCCESSOR_CONTROL_FD: RawFd = 5;
/// Every fd a successor keeps from what it inherited.
pub const SUCCESSOR_FDS: [RawFd; 3] =
    [SUCCESSOR_MASTER_FD, SUCCESSOR_LOCK_FD, SUCCESSOR_CONTROL_FD];

/// How long the predecessor waits for the successor's `ack` (or `nak`).
pub const ACK_TIMEOUT: Duration = Duration::from_secs(15);
/// How often a successor waiting for `commit` re-checks that its
/// predecessor is alive (it has no timeout of its own; see the module docs).
pub const COMMIT_POLL: Duration = Duration::from_millis(250);
/// After its control channel closed, how long a successor waits to see its
/// predecessor gone (its fds close before it is reparented) before reading the
/// close as a rollback.
pub const PREDECESSOR_EXIT_GRACE: Duration = Duration::from_secs(5);
/// How long the input connections may take to drain at the commit.
pub const INPUT_CONN_DRAIN_BOUND: Duration = Duration::from_secs(2);
/// How long a committed predecessor waits for the successor's `serving`.
pub const SERVING_WAIT: Duration = Duration::from_secs(5);
/// How long the reader may take to park.
pub const QUIESCE_BOUND: Duration = Duration::from_secs(2);
/// How long queued input may take to reach the child before the upgrade is
/// refused.
pub const INPUT_DRAIN_BOUND: Duration = Duration::from_secs(2);

/// TEST SEAM, read only by a SUCCESSOR (`--adopt`): `nak` makes it refuse the
/// adoption after validating everything; `stall` makes it log its pid and
/// never answer (a test then SIGKILLs it before its ack); `exit-after-commit`
/// and `exit-after-publish` make it exit right after the commit (before it
/// touches the lock record, or after its endpoint rename) and before
/// `serving`; `exit-after-serving` makes it take over (held input written,
/// output read) and exit 300 ms later; `fail-publish` makes its endpoint
/// rename fail. Those four act after the commit ([`POST_COMMIT_FAULTS`]) and
/// exist in DEBUG builds only; a release holder ignores and logs them. Any other value, or none, is ignored. None of them can cost
/// the child: the predecessor rolls back, takes the pane back (declaring a
/// loss after `exit-after-serving`), or leaves it to the successor.
pub const ADOPT_FAULT_ENV: &str = "QONTINUI_PTY_HOLDER_TEST_ADOPT_FAULT";

/// TEST SEAM, read by a PREDECESSOR: replaces [`ACK_TIMEOUT`] (milliseconds),
/// so the ack-timeout rollback can be exercised. Clamped to
/// [`MAX_TEST_ACK_TIMEOUT`]: the variable is read in every build, and an
/// unbounded value would freeze a pane (output parked, input held) for as
/// long behind a stalled successor.
pub const ACK_TIMEOUT_ENV: &str = "QONTINUI_PTY_HOLDER_TEST_ACK_TIMEOUT_MS";

/// TEST SEAM, read by a PREDECESSOR: sleep this many milliseconds between the
/// successor's ack and the commit — the window in which a starved predecessor
/// must not cost the pane, and in which input is held. It is NOT a bound on a
/// rollback: it lengthens every upgrade's freeze (output parked, input held)
/// by that much, so it is clamped to [`MAX_TEST_COMMIT_DELAY`].
pub const COMMIT_DELAY_ENV: &str = "QONTINUI_PTY_HOLDER_TEST_COMMIT_DELAY_MS";

/// TEST SEAM, read by a PREDECESSOR: stall this many milliseconds after the
/// commit before it first looks for `serving` — so a successor can report
/// `serving` and die before the predecessor notices either (review r3, G).
/// Clamped to [`MAX_TEST_COMMIT_DELAY`].
pub const SERVING_POLL_DELAY_ENV: &str = "QONTINUI_PTY_HOLDER_TEST_SERVING_POLL_DELAY_MS";

/// Largest [`ACK_TIMEOUT_ENV`] honoured.
pub const MAX_TEST_ACK_TIMEOUT: Duration = Duration::from_secs(60);
/// Largest [`COMMIT_DELAY_ENV`] honoured.
pub const MAX_TEST_COMMIT_DELAY: Duration = Duration::from_secs(30);

/// A test-seam duration from the environment, clamped to `max`.
fn env_ms(name: &str, max: Duration) -> Option<Duration> {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|ms| Duration::from_millis(ms).min(max))
}

/// Leads every snapshot file.
const SNAPSHOT_MAGIC: &[u8] = b"QPTYHOLDER-SNAPSHOT-1\n";
/// Largest snapshot header a successor will read.
pub const MAX_SNAPSHOT_HEADER: u32 = 8 * 1024 * 1024;
/// Largest ring a snapshot may carry.
pub const MAX_SNAPSHOT_RING: u64 = 256 * 1024 * 1024;

/// One upgrade at a time per holder (a holder serves exactly one pane).
static IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// What the successor needs, besides the ring bytes that follow it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotHeader {
    pub pane_id: String,
    pub predecessor_pid: u32,
    pub predecessor_build: String,
    pub child_pid: u32,
    pub child_start: Option<String>,
    pub spawn_cwd: Option<WireOs>,
    pub cols: u16,
    pub rows: u16,
    pub ring_capacity: usize,
    /// Absolute offset of the first ring byte in the file.
    pub ring_start: u64,
    /// X: where the predecessor stopped reading, and the successor resumes.
    /// The ring bytes in the file end exactly here.
    pub handoff_offset: u64,
    pub exit_linger_ms: u64,
    pub modes: ModeSnapshot,
    /// The predecessor's declared-loss take-back offset, if it ever took its
    /// pane back from a successor that died after serving (H1). Carried so a
    /// client still holding that dead successor's offset is told the loss by
    /// this successor too. Absent in a snapshot from an older predecessor.
    #[serde(default)]
    pub stream_replaced_at: Option<u64>,
}

/// Control-channel messages, one `KIND_CONTROL` frame each. `commit` is
/// followed by `held_frames` `KIND_DATA` frames: the input the predecessor
/// held while frozen, in order. `serving` goes successor → predecessor once
/// the successor's endpoint is in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMsg {
    UpgradeAck {
        successor_pid: u32,
    },
    UpgradeNak {
        reason: String,
    },
    Commit {
        held_frames: usize,
        /// The child's exit, when it exited mid-swap and the predecessor (its
        /// parent) observed it.
        #[serde(default)]
        child_exit: Option<ExitReply>,
    },
    Serving,
}

fn send_msg(w: &mut UnixStream, msg: &ControlMsg) -> io::Result<()> {
    let payload = serde_json::to_vec(msg).map_err(io::Error::other)?;
    write_frame(w, KIND_CONTROL, &payload)
}

/// Read one frame by `deadline`. `Ok(None)` is a clean EOF.
fn recv_frame(r: &mut UnixStream, deadline: Instant) -> io::Result<Option<crate::frame::Frame>> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "control channel timed out",
        ));
    }
    r.set_read_timeout(Some(left))?;
    read_frame(r)
}

fn recv_msg(r: &mut UnixStream, deadline: Instant) -> io::Result<Option<ControlMsg>> {
    match recv_frame(r, deadline)? {
        None => Ok(None),
        Some(f) if f.kind == KIND_CONTROL => serde_json::from_slice(&f.payload)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Some(f) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected control-channel frame kind 0x{:02x}", f.kind),
        )),
    }
}

/// The executable a successor is started from: this holder's own path. When
/// an upgrade REPLACED that file (a rename over it, as installers do), Linux
/// reports the running image as `<path> (deleted)`; the new file at `<path>`
/// is then the successor — which is the point of an upgrade.
pub fn successor_exe() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    if exe.exists() {
        return Ok(exe);
    }
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let raw = exe.as_os_str().as_bytes();
    if let Some(stripped) = raw.strip_suffix(b" (deleted)") {
        let p = PathBuf::from(std::ffi::OsString::from_vec(stripped.to_vec()));
        if p.exists() {
            return Ok(p);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "no executable at {} to start a successor from",
            exe.display()
        ),
    ))
}

/// What the predecessor needs to run an upgrade, shared with every
/// connection thread.
#[derive(Debug, Clone)]
pub struct UpgradeCtx {
    pub pane_dir: PathBuf,
    /// The holder's pane-lock fd. Valid for as long as the holder serves:
    /// `Holder::run` drops the lock only after the pane's exit is settled,
    /// and an upgrade is refused once it is.
    pub lock_fd: RawFd,
    pub log: PathBuf,
}

/// How a `prepare_upgrade` ended for the predecessor.
#[derive(Debug)]
pub enum UpgradeResult {
    /// Committed: answer `upgraded`, then exit at once ([`exit_after_handoff`]).
    Committed {
        successor_pid: u32,
        handoff_offset: u64,
    },
    /// Rolled back (or refused before anything moved): answer
    /// `upgrade_failed` and keep serving.
    Failed(String),
}

impl UpgradeResult {
    pub fn reply(&self) -> Reply {
        match self {
            UpgradeResult::Committed {
                successor_pid,
                handoff_offset,
            } => Reply::Upgraded {
                successor_pid: *successor_pid,
                handoff_offset: *handoff_offset,
            },
            UpgradeResult::Failed(reason) => Reply::UpgradeFailed {
                reason: reason.clone(),
            },
        }
    }
}

/// End the predecessor after a committed handoff. `_exit`: no destructor runs
/// (the `portable-pty` writer's would type EOF into the child's terminal), no
/// pane-end record is written and nothing is reaped — the pane is the
/// successor's. Closing this process's master copy does not hang the PTY up:
/// the successor holds another.
pub fn exit_after_handoff() -> ! {
    // SAFETY: _exit has no preconditions; it never returns.
    unsafe { libc::_exit(0) }
}

/// PREDECESSOR: run the whole exchange (module docs). Never leaves the pane
/// in a state other than "served here" or "committed to a successor".
pub fn prepare_upgrade(ctx: &UpgradeCtx, info: &HolderInfo, pane: &Arc<Pane>) -> UpgradeResult {
    if IN_PROGRESS.swap(true, Ordering::SeqCst) {
        return UpgradeResult::Failed("an upgrade of this holder is already in progress".into());
    }
    let result = run_upgrade(ctx, info, pane);
    if let UpgradeResult::Failed(reason) = &result {
        log_line(&ctx.log, &format!("upgrade rolled back: {reason}"));
        IN_PROGRESS.store(false, Ordering::SeqCst);
    } else {
        log_line(&ctx.log, &format!("upgrade committed: {result:?}"));
    }
    result
}

fn run_upgrade(ctx: &UpgradeCtx, info: &HolderInfo, pane: &Arc<Pane>) -> UpgradeResult {
    if let Err(e) = pane.freeze_input(INPUT_DRAIN_BOUND) {
        return UpgradeResult::Failed(format!("input freeze: {e}"));
    }
    let handoff_offset = match pane.quiesce_reads(QUIESCE_BOUND) {
        Ok(x) => x,
        Err(e) => {
            pane.unfreeze_input();
            return UpgradeResult::Failed(format!("quiesce: {e}"));
        }
    };
    let snap = snapshot_path(&ctx.pane_dir, &info.pane_id);
    // Which socket file serves the pane now — to tell, if the successor dies
    // after the commit, whether it already replaced it (review r2, D).
    let sock = crate::pane::socket_path(&ctx.pane_dir, &info.pane_id);
    let sock_ino_before = socket_inode(&sock);
    let rollback = |child: Option<&mut Child>, why: String| {
        if let Some(c) = child {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_file(snapshot_tmp_path(&ctx.pane_dir, &info.pane_id));
        let _ = std::fs::remove_file(&snap);
        // A successor killed between its staged bind and its exit leaves the
        // staged socket behind.
        let _ = std::fs::remove_file(crate::transport::staged_socket_path(
            &ctx.pane_dir,
            &info.pane_id,
        ));
        pane.resume_reads();
        pane.unfreeze_input();
        UpgradeResult::Failed(why)
    };
    if let Err(e) = write_snapshot(ctx, info, pane, handoff_offset) {
        return rollback(None, format!("snapshot: {e}"));
    }
    let (mut ours, theirs) = match UnixStream::pair() {
        Ok(p) => p,
        Err(e) => return rollback(None, format!("control channel: {e}")),
    };
    let mut child = match spawn_successor(ctx, pane, &snap, theirs) {
        Ok(c) => c,
        Err(e) => return rollback(None, format!("spawn successor: {e}")),
    };
    let successor_pid = child.id();
    let deadline =
        Instant::now() + env_ms(ACK_TIMEOUT_ENV, MAX_TEST_ACK_TIMEOUT).unwrap_or(ACK_TIMEOUT);
    match recv_msg(&mut ours, deadline) {
        Ok(Some(ControlMsg::UpgradeAck { successor_pid: p })) if p == successor_pid => {}
        Ok(Some(ControlMsg::UpgradeAck { successor_pid: p })) => {
            return rollback(
                Some(&mut child),
                format!("ack names pid {p}, but the successor is pid {successor_pid}"),
            )
        }
        Ok(Some(ControlMsg::UpgradeNak { reason })) => {
            return rollback(Some(&mut child), format!("successor refused: {reason}"))
        }
        Ok(Some(other)) => {
            return rollback(
                Some(&mut child),
                format!("unexpected {other:?} before the ack"),
            )
        }
        Ok(None) => {
            let status = child.try_wait().ok().flatten();
            return rollback(
                Some(&mut child),
                format!("successor closed the control channel without an ack (exit: {status:?})"),
            );
        }
        Err(e) => return rollback(Some(&mut child), format!("waiting for the ack: {e}")),
    }
    if let Some(delay) = env_ms(COMMIT_DELAY_ENV, MAX_TEST_COMMIT_DELAY) {
        std::thread::sleep(delay);
    }
    // COMMIT. First drain the input connections into the held input (review
    // r1, finding 3); a drain that does not finish is still a rollback.
    if let Err(e) = pane.drain_input_for_handoff(INPUT_CONN_DRAIN_BOUND) {
        return rollback(Some(&mut child), format!("input drain: {e}"));
    }
    let held = pane.take_input_for_handoff();
    let child_exit = pane.lock_state().observed_exit;
    let sent = ours.set_write_timeout(Some(ACK_TIMEOUT)).and_then(|()| {
        send_msg(
            &mut ours,
            &ControlMsg::Commit {
                held_frames: held.len(),
                child_exit,
            },
        )?;
        for frame in &held {
            write_frame(&mut ours, KIND_DATA, frame)?;
        }
        ours.flush()
    });
    if let Err(e) = sent {
        // The successor did not get a complete commit, so it never reads.
        // `rollback` kills and reaps it BEFORE this reader resumes (the
        // reader is still parked, never retired).
        pane.restore_handed_input(held);
        return rollback(
            Some(&mut child),
            format!("commit could not be delivered: {e}"),
        );
    }
    // Committed — but this process does not retire its reader until the
    // successor reports `serving`: a successor that dies before that has
    // read nothing from the master and written none of the held input (it
    // does both only after `serving`), so this holder can take its pane back
    // losslessly (review r2, D). Meanwhile this endpoint keeps answering.
    match await_serving(&mut ours, &mut child) {
        AfterCommit::Serving => {}
        AfterCommit::SuccessorDied(status) => {
            let why = format!(
                "successor {successor_pid} died after the commit, before `serving` ({status}); \
                 this holder took its pane back"
            );
            take_pane_back(
                ctx,
                info,
                pane,
                &sock,
                sock_ino_before,
                Back::Lossless(held),
                &why,
            );
            return UpgradeResult::Failed(why);
        }
        AfterCommit::SuccessorDiedServing(status) => {
            // Review r3, G: it DID take over — it may have written the held
            // input and read output past X. The child still needs a holder,
            // so take the pane back anyway, but declare what may be lost.
            let why = format!(
                "successor {successor_pid} died AFTER reporting `serving` ({status}); this \
                 holder took its pane back, but output since offset {handoff_offset} may be \
                 missing and the input held across the upgrade may or may not have reached \
                 the child"
            );
            take_pane_back(
                ctx,
                info,
                pane,
                &sock,
                sock_ino_before,
                Back::DeclaredLoss { handoff_offset },
                &why,
            );
            return UpgradeResult::Failed(why);
        }
        AfterCommit::NoServing(detail) => log_line(
            &ctx.log,
            &format!(
                "upgrade: no `serving` from successor {successor_pid} within {SERVING_WAIT:?} \
                 ({detail}); it is alive, so the commit stands and this holder exits — a \
                 connect may be refused until its endpoint is in place"
            ),
        ),
    }
    pane.hand_off_reads();
    UpgradeResult::Committed {
        successor_pid,
        handoff_offset,
    }
}

/// The inode `path` names, if any.
fn socket_inode(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path)
        .ok()
        .map(|m| (m.dev(), m.ino()))
}

/// Whether a `serving` is queued on the control channel of a successor that
/// has exited (so nothing more can arrive): read until EOF, never waiting.
fn serving_queued(ours: &mut UnixStream) -> bool {
    loop {
        match recv_msg(ours, Instant::now() + Duration::from_millis(10)) {
            Ok(Some(ControlMsg::Serving)) => return true,
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return false,
        }
    }
}

/// How [`take_pane_back`] treats what the dead successor may have touched.
enum Back {
    /// It died before `serving`: it read nothing and wrote nothing, so the
    /// held input goes to this holder's writer and the stream is continuous.
    Lossless(Vec<Vec<u8>>),
    /// It died after `serving`: it may have written the held input (so it is
    /// NOT replayed — a duplicate would be worse than a declared loss) and
    /// read output past X (so the stream gets an explicit notice at the
    /// resume point).
    DeclaredLoss { handoff_offset: u64 },
}

/// The in-stream notice of a [`Back::DeclaredLoss`] take-back. It starts with
/// CAN (0x18), which aborts any escape, CSI or OSC sequence the stream was in
/// the middle of at X — so the text is displayed, not swallowed into a
/// half-received sequence (review r4 nit). The rest is plain text: it passes
/// through the mode tracker without changing a mode.
pub fn upgrade_loss_notice(handoff_offset: u64) -> Vec<u8> {
    format!(
        "\x18\r\n[qontinui pty holder] holder upgrade: the successor holder died after taking \
         over; output since offset {handoff_offset} may be missing, and input sent during the \
         upgrade may not have reached the program\r\n"
    )
    .into_bytes()
}

/// Successor faults that act AFTER the commit — the ones that can cost a
/// pane's input or output, or leave it unreachable. Compiled into debug
/// builds only (review r4, H2); a release holder ignores them.
pub const POST_COMMIT_FAULTS: &[&str] = &[
    "exit-after-commit",
    "exit-after-publish",
    "exit-after-serving",
    "fail-publish",
];

/// Release builds: a post-commit fault value in the environment is ignored —
/// and said so in the pane's log, so a test that expected it can tell.
#[cfg(not(debug_assertions))]
fn ignore_post_commit_fault(log: &Path) {
    if let Ok(v) = std::env::var(ADOPT_FAULT_ENV) {
        if POST_COMMIT_FAULTS.contains(&v.as_str()) {
            log_line(
                log,
                &format!(
                    "adopt: ignoring test fault {v:?}: post-commit faults exist in debug \
                     builds only"
                ),
            );
        }
    }
}

/// What a committed predecessor learned while waiting for `serving`.
enum AfterCommit {
    Serving,
    /// The successor exited AFTER queuing `serving`: it had taken over.
    SuccessorDiedServing(String),
    /// The successor exited (it is our child: `try_wait` proves it), before
    /// `serving` — so before it read the master or wrote any held input.
    SuccessorDied(String),
    /// The successor is alive but did not report `serving` within
    /// [`SERVING_WAIT`].
    NoServing(String),
}

fn await_serving(ours: &mut UnixStream, child: &mut Child) -> AfterCommit {
    if let Some(stall) = env_ms(SERVING_POLL_DELAY_ENV, MAX_TEST_COMMIT_DELAY) {
        std::thread::sleep(stall);
    }
    let deadline = Instant::now() + SERVING_WAIT;
    let mut channel_closed = false;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            // Dead — but did it report `serving` first (review r3, G)? A
            // `serving` can be queued after the last receive timed out. The
            // successor is gone, so everything it wrote is already queued:
            // drain the channel without waiting.
            return if serving_queued(ours) {
                AfterCommit::SuccessorDiedServing(status.to_string())
            } else {
                AfterCommit::SuccessorDied(status.to_string())
            };
        }
        if Instant::now() >= deadline {
            return AfterCommit::NoServing(if channel_closed {
                "its control channel closed".into()
            } else {
                "timed out".into()
            });
        }
        if channel_closed {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        match recv_msg(ours, Instant::now() + Duration::from_millis(100)) {
            Ok(Some(ControlMsg::Serving)) => return AfterCommit::Serving,
            Ok(Some(other)) => {
                return AfterCommit::NoServing(format!("unexpected {other:?}"));
            }
            // EOF: the successor closed its end — exiting, most likely; the
            // next `try_wait` says.
            Ok(None) => channel_closed = true,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) => {}
            Err(_) => channel_closed = true,
        }
    }
}

/// The successor died after the commit and before `serving`: this holder
/// serves its pane again. Its lock record is rewritten to name it (the
/// successor may already have named itself), its endpoint is re-bound if the
/// successor's rename had replaced it, the held input goes back to its
/// writer, and its reader resumes at X — the successor read nothing.
fn take_pane_back(
    ctx: &UpgradeCtx,
    info: &HolderInfo,
    pane: &Arc<Pane>,
    sock: &Path,
    sock_ino_before: Option<(u64, u64)>,
    back: Back,
    why: &str,
) {
    let level = match back {
        Back::Lossless(_) => "",
        Back::DeclaredLoss { .. } => "ERROR: ",
    };
    log_line(&ctx.log, &format!("upgrade: {level}{why}"));
    let record = LockRecord {
        holder_pid: info.holder_pid,
        child_pid: info.child_pid,
        versions: PROTOCOL_VERSIONS.to_vec(),
        started_at_unix_ms: info.started_at_unix_ms,
        holder_build: holder_build(),
        child_start: pane.child_start().map(str::to_string),
        holder_start: crate::proc_identity::start_token(info.holder_pid),
        scope_unit: crate::server::own_scope_unit(),
        ended: None,
    };
    let rewritten = dup_cloexec(ctx.lock_fd, 0)
        .map(File::from)
        .and_then(|mut f| crate::lock::rewrite_record_in(&mut f, &record));
    if let Err(e) = rewritten {
        log_line(
            &ctx.log,
            &format!("upgrade: ERROR: lock record not restored: {e}"),
        );
    }
    if socket_inode(sock) != sock_ino_before {
        // The dead successor's socket sits at the path: bind a fresh one and
        // accept on it (the old listener keeps an orphaned inode).
        let rebound =
            crate::transport::bind_staged(&ctx.pane_dir, &info.pane_id).and_then(|staged| {
                let (listener, published) = staged.publish();
                published?;
                crate::server::spawn_accept_loop(
                    listener,
                    Arc::new(info.clone()),
                    Arc::clone(pane),
                    ctx.log.clone(),
                )
            });
        if let Err(e) = rebound {
            log_line(
                &ctx.log,
                &format!("upgrade: ERROR: endpoint not re-bound ({e}); the pane is unreachable"),
            );
        }
    }
    let _ = std::fs::remove_file(crate::transport::staged_socket_path(
        &ctx.pane_dir,
        &info.pane_id,
    ));
    match back {
        Back::Lossless(held) => pane.restore_handed_input(held),
        Back::DeclaredLoss { handoff_offset } => {
            pane.restore_handed_input(Vec::new());
            // H1 (coord finding f4672ed2): record X BEFORE the notice enters
            // the stream, so no attach can see this holder's bytes past X
            // without the rule that refuses to replay them to a client whose
            // offset came from the dead successor.
            pane.record_stream_replaced_at(handoff_offset);
            pane.inject_stream_notice(&upgrade_loss_notice(handoff_offset));
        }
    }
    pane.resume_reads();
}

fn write_snapshot(ctx: &UpgradeCtx, info: &HolderInfo, pane: &Pane, x: u64) -> io::Result<()> {
    let (header, ring) = {
        let st = pane.lock_state();
        let (ring_start, ring) = st.ring.contents();
        if ring_start + ring.len() as u64 != x || st.modes.fed() != x {
            return Err(io::Error::other(
                "the ring moved after the reader parked — refusing a split stream",
            ));
        }
        let header = SnapshotHeader {
            pane_id: info.pane_id.as_str().to_string(),
            predecessor_pid: info.holder_pid,
            predecessor_build: holder_build(),
            child_pid: pane.child_pid(),
            child_start: pane.child_start().map(str::to_string),
            spawn_cwd: pane.spawn_cwd().map(|c| WireOs::from_os(c)),
            cols: st.size.0,
            rows: st.size.1,
            ring_capacity: pane.ring_capacity(),
            ring_start,
            handoff_offset: x,
            exit_linger_ms: pane.exit_linger().as_millis() as u64,
            modes: st.modes.snapshot(),
            stream_replaced_at: st.stream_replaced_at,
        };
        (header, ring)
    };
    let json = serde_json::to_vec(&header).map_err(io::Error::other)?;
    let header_len = u32::try_from(json.len())
        .ok()
        .filter(|n| *n <= MAX_SNAPSHOT_HEADER)
        .ok_or_else(|| io::Error::other("snapshot header too large"))?;
    let tmp = snapshot_tmp_path(&ctx.pane_dir, &info.pane_id);
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut opts = std::fs::OpenOptions::new();
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = opts.open(&tmp)?;
    let written = (|| {
        f.write_all(SNAPSHOT_MAGIC)?;
        f.write_all(&header_len.to_be_bytes())?;
        f.write_all(&json)?;
        // No fsync: the file lives for milliseconds, carries the pane's
        // output, and is worthless after a reboot (review r1 nit).
        f.write_all(&ring)
    })();
    drop(f);
    if let Err(e) =
        written.and_then(|()| std::fs::rename(&tmp, snapshot_path(&ctx.pane_dir, &info.pane_id)))
    {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Start `successor_exe() --adopt <snap>` with the master, the lock and the
/// control channel at [`SUCCESSOR_FDS`].
fn spawn_successor(
    ctx: &UpgradeCtx,
    pane: &Pane,
    snap: &Path,
    control: UnixStream,
) -> io::Result<Child> {
    let exe = successor_exe()?;
    let master_fd = pane
        .master_raw_fd()
        .ok_or_else(|| io::Error::other("the pane's master has no fd"))?;
    // High-numbered CLOEXEC copies, so the dup2s in pre_exec can never
    // clobber a source that happens to sit on a target number (3..=5).
    const ABOVE: i32 = 10;
    let master = dup_cloexec(master_fd, ABOVE)?;
    let lock = dup_cloexec(ctx.lock_fd, ABOVE)?;
    let control_dup = dup_cloexec(control.as_raw_fd(), ABOVE)?;
    drop(control);
    let sources = [
        master.as_raw_fd(),
        lock.as_raw_fd(),
        control_dup.as_raw_fd(),
    ];
    let mut cmd = Command::new(exe);
    cmd.arg("--adopt")
        .arg(snap)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: the closure only calls dup2 and fcntl, which are
    // async-signal-safe, on fds that are open in the forked child.
    unsafe {
        cmd.pre_exec(move || {
            for (src, dst) in sources.iter().zip(SUCCESSOR_FDS) {
                if libc::dup2(*src, dst) < 0 {
                    return Err(io::Error::last_os_error());
                }
                // dup2 clears FD_CLOEXEC on a NEW number; say so explicitly
                // anyway (D12a rule 4), in case src == dst ever held.
                if libc::fcntl(dst, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let child = cmd.spawn();
    // Our copies close here: the successor's control end must have exactly
    // one owner, so its exit is an EOF on ours.
    drop((master, lock, control_dup));
    child
}

/// SUCCESSOR: a pane that was handed over, ready to serve, or the exit code to
/// leave with (the predecessor keeps the pane).
pub fn adopt(snap: &Path) -> Result<Holder, u8> {
    // The three inherited fds. Each is checked open before it is owned: an
    // `OwnedFd` over a number this process does not hold would close a
    // stranger's fd later.
    let open = |fd: RawFd| {
        // SAFETY: F_GETFD only reads the flags of `fd`.
        unsafe { libc::fcntl(fd, libc::F_GETFD) >= 0 }
    };
    if !SUCCESSOR_FDS.iter().all(|&fd| open(fd)) {
        return Err(2);
    }
    // SAFETY: each fd was checked open above, was inherited for exactly this
    // purpose, and is owned by nothing else in this process.
    let (master, lockf, mut control) = unsafe {
        (
            File::from(OwnedFd::from_raw_fd(SUCCESSOR_MASTER_FD)),
            File::from(OwnedFd::from_raw_fd(SUCCESSOR_LOCK_FD)),
            UnixStream::from(OwnedFd::from_raw_fd(SUCCESSOR_CONTROL_FD)),
        )
    };
    for fd in SUCCESSOR_FDS {
        // SAFETY: setting FD_CLOEXEC on an owned, open fd. Nothing this
        // process starts may inherit the pane's master or lock.
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
    let prepared = match prepare_adoption(snap, master, lockf) {
        Ok(p) => p,
        Err(reason) => {
            let _ = send_msg(&mut control, &ControlMsg::UpgradeNak { reason });
            return Err(6);
        }
    };
    let Prepared {
        pane_dir,
        pane_id,
        header,
        pane,
        mut lock,
        staged,
    } = prepared;
    let log = log_path(&pane_dir, &pane_id);
    if send_msg(
        &mut control,
        &ControlMsg::UpgradeAck {
            successor_pid: std::process::id(),
        },
    )
    .is_err()
    {
        // No ack delivered: the predecessor rolls back. Signal nothing.
        return Err(7);
    }
    log_line(&log, "adopt: acked; waiting for the commit");
    // The post-commit faults exist in debug builds only (review r4, H2).
    #[cfg(debug_assertions)]
    let fault = std::env::var(ADOPT_FAULT_ENV).ok();
    #[cfg(not(debug_assertions))]
    ignore_post_commit_fault(&log);
    let commit = match await_commit(&mut control, header.predecessor_pid, &log) {
        Ok(commit) => commit,
        Err(why) => {
            log_line(
                &log,
                &format!("adopt: no commit ({why}); leaving the pane to the predecessor"),
            );
            return Err(8);
        }
    };
    #[cfg(debug_assertions)]
    if fault.as_deref() == Some("exit-after-commit") {
        log_line(&log, "adopt: test fault: exit-after-commit");
        return Err(10);
    }
    // Committed: the pane is ours now.
    let info = HolderInfo {
        pane_id: pane_id.clone(),
        holder_pid: std::process::id(),
        child_pid: Some(header.child_pid),
        started_at_unix_ms: crate::server::now_unix_ms(),
        upgrade: Some(UpgradeCtx {
            pane_dir: pane_dir.clone(),
            lock_fd: lock.as_raw_fd(),
            log: log.clone(),
        }),
    };
    let record = LockRecord {
        holder_pid: info.holder_pid,
        child_pid: info.child_pid,
        versions: PROTOCOL_VERSIONS.to_vec(),
        started_at_unix_ms: info.started_at_unix_ms,
        holder_build: holder_build(),
        child_start: header.child_start.clone(),
        holder_start: crate::proc_identity::start_token(info.holder_pid),
        scope_unit: crate::server::own_scope_unit(),
        ended: None,
    };
    // From here on NOTHING may end the child (review r1, finding 2): every
    // step that could fail without a fallback ran before the ack.
    let mut record = record;
    if let Err(e) = lock.rewrite_record(&record) {
        // The lock is still held: an EMPTY record reads UNKNOWN (and a sweep
        // signals nothing for it) — never a wrong holder, never a kill.
        let cleared = lock.clear_record();
        log_line(
            &log,
            &format!(
                "adopt: ERROR: lock record not rewritten ({e}); cleared it instead: {cleared:?}"
            ),
        );
        record.holder_start = None;
    }
    #[cfg(debug_assertions)]
    if fault.as_deref() == Some("fail-publish") {
        // Take the staged socket away, so the rename fails (path C).
        let _ = std::fs::remove_file(crate::transport::staged_socket_path(&pane_dir, &pane_id));
    }
    let (listener, published) = staged.publish();
    #[cfg(debug_assertions)]
    if fault.as_deref() == Some("exit-after-publish") {
        log_line(&log, "adopt: test fault: exit-after-publish");
        return Err(10);
    }
    match &published {
        // Tell the predecessor it may go. Only now, and only before this
        // process reads the master or writes any held input: until
        // `serving`, a successor death leaves the pane to the predecessor
        // losslessly (review r2, D).
        Ok(()) => {
            let _ = send_msg(&mut control, &ControlMsg::Serving);
        }
        // No `serving` on a false report (review r2, C). The predecessor
        // waits out its bound, logs it, and exits; this process must not
        // touch the master until it has: wait for its end of the channel to
        // close.
        Err(e) => {
            log_line(
                &log,
                &format!(
                    "adopt: ERROR: endpoint not moved into place ({e}); the pane keeps running \
                     UNREACHABLE (its lock held, so it reads UNKNOWN)"
                ),
            );
            log_line(
                &log,
                "adopt: no `serving` sent; waiting for the predecessor to leave before reading",
            );
            let _ = control.set_read_timeout(None);
            while let Ok(Some(_)) = read_frame(&mut control) {}
            log_line(&log, "adopt: the predecessor left; reading");
        }
    }
    drop(control);
    pane.inject_input(commit.held);
    if let Some(exit) = commit.child_exit {
        pane.set_known_exit(exit);
    }
    pane.resume_reads();
    #[cfg(debug_assertions)]
    if fault.as_deref() == Some("exit-after-serving") {
        // Take over for real — held input written, output read past X —
        // then die before the predecessor notices (review r3, G).
        std::thread::sleep(Duration::from_millis(300));
        log_line(&log, "adopt: test fault: exit-after-serving");
        return Err(10);
    }
    log_line(
        &log,
        &format!(
            "adopt: serving pane {pane_id} (child {}) from offset {}, handed over by holder pid {} ({})",
            header.child_pid, header.handoff_offset, header.predecessor_pid, header.predecessor_build
        ),
    );
    Ok(Holder::from_parts(info, pane, listener, log, lock, record))
}

struct Prepared {
    pane_dir: PathBuf,
    pane_id: PaneId,
    header: SnapshotHeader,
    pane: Arc<Pane>,
    lock: PaneLock,
    /// The successor's endpoint, bound at the staged path BEFORE the ack, so
    /// a bind failure is a `nak` — never a failure after the commit.
    staged: crate::transport::StagedListener,
}

/// Everything a successor checks before it acks. Builds the adopted pane with
/// its reader PARKED: until `commit`, the predecessor may still roll back and
/// read.
fn prepare_adoption(snap: &Path, master: File, lockf: File) -> Result<Prepared, String> {
    let pane_dir = snap
        .parent()
        .filter(|d| d.is_absolute())
        .ok_or("the snapshot path has no absolute pane directory")?
        .to_path_buf();
    check_private_dir(&pane_dir).map_err(|e| format!("pane dir: {e}"))?;
    let (header, ring_bytes) = consume_snapshot(snap).map_err(|e| format!("snapshot: {e}"))?;
    let pane_id = PaneId::new(&header.pane_id).map_err(|e| e.to_string())?;
    if snapshot_path(&pane_dir, &pane_id) != snap {
        return Err("the snapshot is not at its pane's snapshot path".into());
    }
    let lock = PaneLock::adopt_inherited(lockf, &lock_path(&pane_dir, &pane_id))
        .map_err(|e| format!("lock: {e}"))?;
    get_winsize(master.as_raw_fd())
        .map_err(|e| format!("the inherited master is not a PTY master: {e}"))?;
    if !crate::proc_identity::is_same_process(header.child_pid, header.child_start.as_deref()) {
        return Err(format!(
            "pane process {} is not the one the snapshot names (gone, or its pid reused)",
            header.child_pid
        ));
    }
    let log = log_path(&pane_dir, &pane_id);
    match std::env::var(ADOPT_FAULT_ENV).as_deref() {
        Ok("nak") => return Err("test fault: nak".into()),
        Ok("stall") => {
            log_line(&log, "adopt: test fault: stall");
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        _ => {}
    }
    let ring = OutputRing::restore(header.ring_capacity, header.ring_start, &ring_bytes);
    if ring.end_offset() != header.handoff_offset {
        return Err("the snapshot's ring does not end at its handoff offset".into());
    }
    let modes = ModeTracker::restore(header.modes.clone(), &ring_bytes);
    if modes.fed() != header.handoff_offset {
        return Err("the snapshot's mode state does not end at its handoff offset".into());
    }
    let spawn_cwd = match &header.spawn_cwd {
        Some(w) => Some(w.to_os().map_err(|e| e.to_string())?),
        None => None,
    };
    let pane = Pane::adopt(AdoptedParts {
        master,
        child_pid: header.child_pid,
        child_start: header.child_start.clone(),
        spawn_cwd,
        ring,
        ring_capacity: header.ring_capacity,
        modes,
        size: (header.cols, header.rows),
        exit_linger: Duration::from_millis(header.exit_linger_ms),
        stream_replaced_at: header.stream_replaced_at,
    })
    .map_err(|e| format!("adopted pane: {e}"))?;
    let staged =
        crate::transport::bind_staged(&pane_dir, &pane_id).map_err(|e| format!("endpoint: {e}"))?;
    Ok(Prepared {
        pane_dir,
        pane_id,
        header,
        pane,
        lock,
        staged,
    })
}

/// Read the snapshot and unlink it the moment it is open (it carries the
/// pane's output). Refuses a symlink, a file not ours, any group/other bit,
/// and anything larger than the bounds.
fn consume_snapshot(path: &Path) -> io::Result<(SnapshotHeader, Vec<u8>)> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let _ = std::fs::remove_file(path);
    let md = f.metadata()?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    if !md.is_file() || md.uid() != me || md.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "snapshot is not a private regular file of this user",
        ));
    }
    let mut r = io::BufReader::new(f);
    let mut magic = vec![0u8; SNAPSHOT_MAGIC.len()];
    r.read_exact(&mut magic)?;
    if magic != SNAPSHOT_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a holder snapshot",
        ));
    }
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len);
    if len > MAX_SNAPSHOT_HEADER {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "snapshot header too large",
        ));
    }
    let mut json = vec![0u8; len as usize];
    r.read_exact(&mut json)?;
    let header: SnapshotHeader =
        serde_json::from_slice(&json).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let want = header
        .handoff_offset
        .checked_sub(header.ring_start)
        .filter(|n| *n <= MAX_SNAPSHOT_RING)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "snapshot ring bounds"))?;
    let mut ring = Vec::with_capacity(want as usize);
    r.by_ref().take(want + 1).read_to_end(&mut ring)?;
    if ring.len() as u64 != want {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "snapshot ring length does not match its offsets",
        ));
    }
    Ok((header, ring))
}

/// What a complete `commit` carried.
struct Commit {
    held: Vec<Vec<u8>>,
    child_exit: Option<ExitReply>,
}

/// Block until `fd` is readable, re-checking every [`COMMIT_POLL`] — with NO
/// deadline while `predecessor_gone` is false. `Ok(true)` readable (data or
/// EOF), `Ok(false)` the predecessor is gone.
fn wait_readable(fd: RawFd, predecessor_gone: &dyn Fn() -> bool) -> io::Result<bool> {
    loop {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll over one valid pollfd.
        let rc = unsafe { libc::poll(&mut pfd, 1, COMMIT_POLL.as_millis() as libc::c_int) };
        if rc > 0 {
            return Ok(true);
        }
        if rc < 0 {
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
        if predecessor_gone() {
            return Ok(false);
        }
    }
}

/// One control-channel frame, waiting with no deadline of its own while the
/// predecessor lives. `Ok(None)`: EOF, or the predecessor is gone.
fn recv_while_alive(
    control: &mut UnixStream,
    predecessor_gone: &dyn Fn() -> bool,
) -> io::Result<Option<crate::frame::Frame>> {
    if !wait_readable(control.as_raw_fd(), predecessor_gone)? {
        return Ok(None);
    }
    // Readable: the frame is being written by a live (or just-died) peer,
    // whose death is an EOF — a blocking read cannot hang on a dead writer.
    control.set_read_timeout(None)?;
    read_frame(control)
}

/// Wait for a complete `commit` (the control frame and every held input
/// frame), with no timeout while the predecessor lives (review r1, finding 1:
/// a successor that gave up could race a late commit, and then both holders
/// leave and the pane is lost — while a predecessor that gives up always
/// kills its successor). A channel that ends without a commit means the
/// predecessor rolled back — unless the predecessor itself is gone, in which
/// case nobody else will ever read the master: it is taken over (module docs),
/// with the held input frames received so far.
fn await_commit(
    control: &mut UnixStream,
    predecessor_pid: u32,
    log: &Path,
) -> Result<Commit, String> {
    let predecessor_gone = || {
        // SAFETY: getppid has no preconditions.
        let parent = unsafe { libc::getppid() };
        u32::try_from(parent).ok() != Some(predecessor_pid)
    };
    // A predecessor's fds close BEFORE it is reparented away: an EOF from a
    // dying predecessor can be seen while getppid still names it. Wait for
    // the reparenting before reading an EOF as a rollback.
    let gone_soon = || {
        let end = Instant::now() + PREDECESSOR_EXIT_GRACE;
        loop {
            if predecessor_gone() {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let take_over = |why: &str, held: Vec<Vec<u8>>| {
        log_line(
            log,
            &format!(
                "adopt: {why}, and predecessor pid {predecessor_pid} is gone — taking the pane \
                 over with {} held input frame(s)",
                held.len()
            ),
        );
        Ok(Commit {
            held,
            child_exit: None,
        })
    };
    let first = recv_while_alive(control, &predecessor_gone);
    let (held_frames, child_exit) = match first {
        Ok(Some(f)) if f.kind == KIND_CONTROL => match serde_json::from_slice(&f.payload) {
            Ok(ControlMsg::Commit {
                held_frames,
                child_exit,
            }) => (held_frames, child_exit),
            Ok(other) => return Err(format!("unexpected {other:?}")),
            Err(e) => return Err(format!("malformed control message: {e}")),
        },
        Ok(Some(f)) => return Err(format!("unexpected frame kind 0x{:02x}", f.kind)),
        Ok(None) | Err(_) if gone_soon() => {
            return take_over("the control channel closed", Vec::new())
        }
        Ok(None) => return Err("the predecessor closed the control channel (rolled back)".into()),
        Err(e) => return Err(format!("control channel: {e}")),
    };
    let mut held = Vec::with_capacity(held_frames.min(crate::pty::INPUT_QUEUE_FRAMES));
    for _ in 0..held_frames {
        match recv_while_alive(control, &predecessor_gone) {
            Ok(Some(f)) if f.kind == KIND_DATA => held.push(f.payload),
            // An incomplete commit: a live predecessor saw its write fail and
            // is rolling back — never read. A predecessor that DIED
            // mid-commit leaves the pane to us, with the frames that arrived.
            Ok(_) | Err(_) if gone_soon() => {
                return take_over("the commit was cut short", held);
            }
            Ok(other) => {
                return Err(format!("incomplete commit ({:?})", other.map(|f| f.kind)));
            }
            Err(e) => return Err(format!("incomplete commit: {e}")),
        }
    }
    Ok(Commit { held, child_exit })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review r2, finding E: the test seams are read in every build, so an
    /// absurd value is clamped rather than freezing a pane for that long.
    #[test]
    fn pty_holder_handoff_test_seams_are_clamped() {
        let name = "QONTINUI_PTY_HOLDER_TEST_CLAMP_PROBE_MS";
        std::env::set_var(name, "999999999");
        assert_eq!(
            env_ms(name, MAX_TEST_ACK_TIMEOUT),
            Some(MAX_TEST_ACK_TIMEOUT)
        );
        assert_eq!(
            env_ms(name, MAX_TEST_COMMIT_DELAY),
            Some(MAX_TEST_COMMIT_DELAY)
        );
        std::env::set_var(name, "1500");
        assert_eq!(
            env_ms(name, MAX_TEST_ACK_TIMEOUT),
            Some(Duration::from_millis(1500))
        );
        std::env::set_var(name, "not-a-number");
        assert_eq!(env_ms(name, MAX_TEST_ACK_TIMEOUT), None);
        std::env::remove_var(name);
    }
}
