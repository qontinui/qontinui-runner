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
//! On the ack it **commits**: it retires its reader for good
//! ([`Pane::hand_off_reads`]), sends `commit` with the input it held, answers
//! the requester `upgraded`, and exits with `_exit` — closing its copy of the
//! master WITHOUT signalling the child (the child is a session leader on its
//! own PTY; the holder is not its controlling process, so nothing hangs it
//! up), and running none of the pane-end work a normal exit does. On ANYTHING
//! else — `nak`, a timeout, the successor's exit, a closed channel, a wrong
//! pid, a failed commit write — it **rolls back**: SIGKILLs and reaps the
//! successor (which never read the master — it reads only after a complete
//! `commit`), unlinks the snapshot, resumes reading at X, and releases the
//! held input to its own writer. The pane is served exactly as before
//! (D12a rule 2).
//!
//! SUCCESSOR ([`adopt`], `main --adopt`): consumes the snapshot (unlinked on
//! open), verifies the inherited lock (`PaneLock::adopt_inherited`), the master
//! (`TIOCGWINSZ`) and that the child is still the process its start token
//! names, builds an adopted [`Pane`] whose reader starts PARKED, and only then
//! acks; any failure is a `nak` and an exit that signals nothing. On `commit`
//! it queues the held input, rewrites the lock record to name itself
//! (`PaneLock::rewrite_record`, never an empty file), takes the endpoint over
//! with an atomic rename (`transport::bind_replacing`: a client reaches the old
//! holder or the new one, never nothing — so no retry loop is needed while the
//! predecessor lets go), and resumes reading at X. A connected client's
//! connection to the predecessor ends; the runner's `DaemonPaneIo` probes, finds
//! the lock held and the endpoint answering, and reattaches at its own last
//! offset — so a late attach sees both sides of the swap with no loss marker
//! and no duplicate.
//!
//! If the PREDECESSOR dies after the ack but before a complete `commit`, the
//! successor takes the pane over anyway: a predecessor that rolled back always
//! kills and reaps its successor before it reads again, so a successor that is
//! alive while its parent is gone knows nobody else reads the master.
//!
//! # What an adopted pane cannot know
//!
//! The successor is not the child's parent. Its exit detection is master
//! EOF/`EIO`, and its exit code is `None` — UNKNOWN, never 0 (D12a rule 3;
//! the runner's `exit_result` renders it as unknown). A `kill` is preceded by
//! a start-token check, since the pid is no longer pinned by a zombie.
//!
//! **Input in the window is not lost:** frames that arrive between the freeze
//! and the commit are held and travel in `commit`. A frame the requester's
//! runner writes to the predecessor's socket AFTER the commit is lost with
//! that connection — the window is the predecessor's last few instructions
//! before `_exit`, and the runner reopens its input connection to the
//! successor.
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
use crate::protocol::{holder_build, Reply, PROTOCOL_VERSIONS};
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
/// How long a successor that acked waits for `commit`.
pub const COMMIT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the reader may take to park.
pub const QUIESCE_BOUND: Duration = Duration::from_secs(2);
/// How long queued input may take to reach the child before the upgrade is
/// refused.
pub const INPUT_DRAIN_BOUND: Duration = Duration::from_secs(2);

/// TEST SEAM, read only by a SUCCESSOR (`--adopt`): `nak` makes it refuse the
/// adoption after validating everything; `stall` makes it log its pid and
/// never answer (a test then SIGKILLs it before its ack). Any other value, or
/// none, is ignored. It can only make an upgrade FAIL, which is always safe:
/// the predecessor rolls back.
pub const ADOPT_FAULT_ENV: &str = "QONTINUI_PTY_HOLDER_TEST_ADOPT_FAULT";

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
}

/// Control-channel messages, one `KIND_CONTROL` frame each. `commit` is
/// followed by `held_frames` `KIND_DATA` frames: the input the predecessor
/// held while frozen, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMsg {
    UpgradeAck { successor_pid: u32 },
    UpgradeNak { reason: String },
    Commit { held_frames: usize },
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
    let rollback = |child: Option<&mut Child>, why: String| {
        if let Some(c) = child {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_file(snapshot_tmp_path(&ctx.pane_dir, &info.pane_id));
        let _ = std::fs::remove_file(&snap);
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
    let deadline = Instant::now() + ACK_TIMEOUT;
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
    // COMMIT. From the moment the reader is retired, only the successor may
    // read; the held input travels with the commit.
    pane.hand_off_reads();
    let held = pane.take_input_for_handoff();
    let sent = ours.set_write_timeout(Some(ACK_TIMEOUT)).and_then(|()| {
        send_msg(
            &mut ours,
            &ControlMsg::Commit {
                held_frames: held.len(),
            },
        )?;
        for frame in &held {
            write_frame(&mut ours, KIND_DATA, frame)?;
        }
        ours.flush()
    });
    if let Err(e) = sent {
        // The successor did not get a complete commit, so it never reads.
        // `rollback` kills and reaps it BEFORE this reader resumes.
        pane.restore_handed_input(held);
        return rollback(
            Some(&mut child),
            format!("commit could not be delivered: {e}"),
        );
    }
    UpgradeResult::Committed {
        successor_pid,
        handoff_offset,
    }
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
        f.write_all(&ring)?;
        f.sync_data()
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
    match await_commit(&mut control, header.predecessor_pid, &log) {
        Ok(held) => pane.inject_input(held),
        Err(why) => {
            log_line(
                &log,
                &format!("adopt: no commit ({why}); leaving the pane to the predecessor"),
            );
            return Err(8);
        }
    }
    drop(control);
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
    let bound = lock
        .rewrite_record(&record)
        .and_then(|()| crate::transport::bind_replacing(&pane_dir, &pane_id));
    let listener = match bound {
        Ok(l) => l,
        Err(e) => {
            // As at a first start: an unreachable pane is a leak, not a
            // session. The predecessor is gone; end the child.
            log_line(
                &log,
                &format!("adopt: endpoint after commit failed ({e}); ending the pane"),
            );
            pane.kill_now();
            return Err(9);
        }
    };
    pane.resume_reads();
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
    })
    .map_err(|e| format!("adopted pane: {e}"))?;
    Ok(Prepared {
        pane_dir,
        pane_id,
        header,
        pane,
        lock,
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

/// Wait for a complete `commit` (the control frame and every held input
/// frame). A channel that ends without one means the predecessor rolled back
/// — unless the predecessor itself is gone, in which case nobody else will
/// ever read the master: it is taken over (module docs).
fn await_commit(
    control: &mut UnixStream,
    predecessor_pid: u32,
    log: &Path,
) -> Result<Vec<Vec<u8>>, String> {
    let deadline = Instant::now() + COMMIT_TIMEOUT;
    let predecessor_gone = || {
        // SAFETY: getppid has no preconditions.
        let parent = unsafe { libc::getppid() };
        u32::try_from(parent).ok() != Some(predecessor_pid)
    };
    let take_over = |why: &str| {
        log_line(
            log,
            &format!("adopt: {why}, and predecessor pid {predecessor_pid} is gone — taking the pane over"),
        );
        Ok(Vec::new())
    };
    let held_frames = match recv_msg(control, deadline) {
        Ok(Some(ControlMsg::Commit { held_frames })) => held_frames,
        Ok(Some(other)) => return Err(format!("unexpected {other:?}")),
        Ok(None) if predecessor_gone() => return take_over("the control channel closed"),
        Ok(None) => return Err("the predecessor closed the control channel (rolled back)".into()),
        Err(e) if predecessor_gone() => return take_over(&format!("control channel: {e}")),
        Err(e) => return Err(format!("control channel: {e}")),
    };
    let mut held = Vec::with_capacity(held_frames.min(crate::pty::INPUT_QUEUE_FRAMES));
    for _ in 0..held_frames {
        match recv_frame(control, deadline) {
            Ok(Some(f)) if f.kind == KIND_DATA => held.push(f.payload),
            // An incomplete commit: the predecessor saw its write fail and is
            // rolling back — never read. Unless it DIED mid-commit: then the
            // input it held is lost, and the pane is not.
            Ok(_) | Err(_) if predecessor_gone() => {
                return take_over("the commit was cut short").map(|_: Vec<Vec<u8>>| held)
            }
            Ok(other) => {
                return Err(format!("incomplete commit ({:?})", other.map(|f| f.kind)));
            }
            Err(e) => return Err(format!("incomplete commit: {e}")),
        }
    }
    Ok(held)
}
