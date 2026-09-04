//! `PaneIo` — the byte-source seam beneath a terminal pane.
//!
//! Everything downstream of `TerminalSession::spawn`'s reader thread consumes
//! `&[u8]` only — grid, scrollback ring, visibility tiering, the emission gate,
//! auto-response, the transcript watcher. The one place the runner assumed
//! those bytes come from a *local PTY* was the spawn function itself, which
//! held the `portable_pty` triple (reader / writer / master) plus the child it
//! waits on and kills. This module names that assumption once, behind a trait,
//! so a pane whose bytes arrive from another machine can slot in later without
//! the session layer learning a second byte source.
//!
//! [`LocalPty`] is the only implementation. Phase 1 of plan
//! `2026-08-31-remote-session-tabs-in-runner-terminal` is deliberately a
//! zero-behaviour-change refactor: the existing Rust suite is the regression
//! gate, and every error string a caller could observe is the one the inline
//! code produced before.
//!
//! # What the trait carries on purpose
//!
//! Two members are not obvious from the local case. The plan's vet added them
//! because a later `Remote` impl cannot add them without widening the interface
//! every intervening phase was built on:
//!
//! - [`PaneIo::set_paused`] — the backpressure affordance. The runner's flow
//!   control gates *emission* and never pauses reads (`EmissionGate` in
//!   `session.rs`, plan `2026-07-22-runner-pty-flow-control-emission-gating`);
//!   a local PTY needs nothing more, so [`LocalPty`] treats it as a no-op. A
//!   remote pane adds a second hop with a buffer the local gate cannot see, and
//!   this is the member that lets the gate reach across it. It is NOT a licence
//!   to import tmux-style source pausing — one regime, one invariant.
//! - [`PaneIo::credential_scrub`] — the credential-scrub obligation, made
//!   explicit. See below.
//!
//! # The credential-scrub obligation
//!
//! The PTY seam strips [`super::CREDENTIAL_VALUE_ENV_VARS`] out of the child
//! environment via [`super::scrub_credential_env_pty`], which is bound to
//! `portable_pty::CommandBuilder`. A remote pane builds no `CommandBuilder`, so
//! it inherits no scrub by construction — and the environment it would need to
//! scrub lives on another machine. The trait therefore carries the obligation in
//! two forms:
//!
//! 1. **By type, for the local impl.** [`LocalPty`] can only be spawned from a
//!    [`ScrubbedCommand`], and the ONLY constructor of that type runs the scrub.
//!    No spawn path can reach `spawn_command` with an unscrubbed builder — even
//!    one that skipped `TerminalSession::finalize_child_env`, which remains the
//!    production env tail and keeps its own (earlier) call to the same scrub.
//! 2. **By declaration, for every impl.** [`PaneIo::credential_scrub`] has no
//!    default, so an implementation must state how it discharged the
//!    obligation, and a reviewer can read the answer off the type.
//!
//! Both forms read the ONE name list in `terminal/mod.rs`; nothing here restates
//! it.

use std::io::{Read, Write};
use std::sync::Mutex;
use std::time::Duration;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtyPair, PtySize};

/// How a [`PaneIo`] implementation discharged the credential-scrub obligation
/// described in the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CredentialScrub {
    /// The child's environment was assembled in THIS process and passed
    /// through [`super::scrub_credential_env_pty`] before the child was
    /// spawned — witnessed by [`ScrubbedCommand`].
    InProcessEnv,
    /// The implementation launches no child and hands no environment to
    /// anything. Only an in-memory double can honestly answer this.
    NoChildEnv,
}

/// A byte source and sink for one terminal pane.
///
/// Constructed once, inside `TerminalSession::spawn`, and shared with the
/// reader and waiter threads. Every method takes `&self` because the
/// implementation owns whatever locking its handles need — the session layer
/// never sees a PTY master, a child handle, or a socket.
///
/// The `String` error type is the session layer's own, so the seam adds no
/// conversion at the call sites it replaced.
pub trait PaneIo: Send + Sync {
    /// A blocking reader over the pane's output. Called once per session;
    /// the reader thread owns the result for the session's life.
    fn reader(&self) -> Result<Box<dyn Read + Send>, String>;

    /// A writer into the pane's input. Called once per session.
    fn writer(&self) -> Result<Box<dyn Write + Send>, String>;

    /// Tell the source its viewport is now `cols` × `rows`.
    fn resize(&self, cols: u16, rows: u16) -> Result<(), String>;

    /// Block until the pane's process ends and return its exit code.
    ///
    /// Called from the waiter thread, at most once. The local mapping is
    /// `0` on success and `1` otherwise, because `portable_pty` does not
    /// expose the raw code on every platform; the session layer keeps that
    /// contract rather than this trait inventing a richer one.
    fn wait(&self) -> Result<i32, String>;

    /// Terminate the pane's process tree, spending at most `budget` on any
    /// blocking step. `Err` means the kill could not be confirmed inside the
    /// budget — the caller logs it and carries on with teardown.
    fn kill(&self, budget: Duration) -> Result<(), String>;

    /// Backpressure affordance: ask the source to hold (`true`) or resume
    /// (`false`) production. See the module docs for why a local PTY is a
    /// no-op here.
    fn set_paused(&self, paused: bool) -> Result<(), String>;

    /// The OS pid of the pane's process, when there is one in THIS process's
    /// pid namespace.
    fn pid(&self) -> Option<u32>;

    /// How this implementation discharged the credential-scrub obligation.
    fn credential_scrub(&self) -> CredentialScrub;

    /// Close the underlying handles so a reader blocked in `read()` unblocks.
    /// Bounded by `budget`; `Err` means the handles could not be reached in
    /// time and will be released by process exit instead.
    fn release(&self, budget: Duration) -> Result<(), String>;
}

/// A `CommandBuilder` that has been through [`super::scrub_credential_env_pty`].
///
/// The only way to obtain one is [`ScrubbedCommand::seal`], which runs the
/// scrub — so [`LocalPty::spawn`], which takes only this type, cannot be handed
/// an unscrubbed environment. `env_remove` is idempotent, so a builder that
/// `finalize_child_env` already scrubbed is unchanged by sealing.
pub struct ScrubbedCommand(CommandBuilder);

impl ScrubbedCommand {
    /// Run the credential scrub and witness it in the type.
    pub fn seal(mut cmd: CommandBuilder) -> Self {
        super::scrub_credential_env_pty(&mut cmd);
        Self(cmd)
    }

    /// The sealed builder, for assertions.
    #[cfg(test)]
    pub(crate) fn as_command(&self) -> &CommandBuilder {
        &self.0
    }
}

/// A PTY pair that has been opened but not yet given a child.
///
/// Two steps rather than one because `TerminalSession::spawn` opens the PTY
/// BEFORE it assembles the child environment (identity seam, install
/// intercept, account pin), so an `openpty` failure leaves none of that
/// side-effecting work behind. Keeping the order keeps the behaviour.
pub struct OpenedPty {
    label: String,
    pair: PtyPair,
}

impl OpenedPty {
    /// Spawn `cmd` on the slave side and hand back the live pane.
    pub fn spawn(self, cmd: ScrubbedCommand) -> Result<LocalPty, String> {
        let OpenedPty { label, pair } = self;
        let child = pair
            .slave
            .spawn_command(cmd.0)
            .map_err(|e| format!("Failed to spawn shell: {}", e))?;
        let pid = child.process_id();
        // `pair.slave` drops here, after the child holds its own copy of the
        // slave side, so the master sees EOF once the child exits.
        Ok(LocalPty {
            label,
            pid,
            master: Mutex::new(Some(pair.master)),
            child: Mutex::new(Some(child)),
        })
    }
}

/// The local `portable_pty` implementation of [`PaneIo`].
pub struct LocalPty {
    /// Terminal id, for tracing fields only.
    label: String,
    pid: Option<u32>,
    /// `None` once [`PaneIo::release`] has dropped the OS handle.
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    /// `None` once the waiter thread has taken it via [`PaneIo::wait`].
    child: Mutex<Option<Box<dyn Child + Send + Sync>>>,
}

impl LocalPty {
    /// Open a PTY of the given size. `label` is the terminal id, used only to
    /// attribute log lines.
    pub fn open(label: &str, cols: u16, rows: u16) -> Result<OpenedPty, String> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("Failed to open PTY: {}", e))?;
        Ok(OpenedPty {
            label: label.to_string(),
            pair,
        })
    }
}

impl PaneIo for LocalPty {
    fn reader(&self) -> Result<Box<dyn Read + Send>, String> {
        let master = self
            .master
            .lock()
            .map_err(|e| format!("Master lock poisoned: {}", e))?;
        match master.as_ref() {
            Some(m) => m
                .try_clone_reader()
                .map_err(|e| format!("Failed to clone PTY reader: {}", e)),
            None => Err("PTY master already released".to_string()),
        }
    }

    fn writer(&self) -> Result<Box<dyn Write + Send>, String> {
        let master = self
            .master
            .lock()
            .map_err(|e| format!("Master lock poisoned: {}", e))?;
        match master.as_ref() {
            Some(m) => m
                .take_writer()
                .map_err(|e| format!("Failed to take PTY writer: {}", e)),
            None => Err("PTY master already released".to_string()),
        }
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        let master = self
            .master
            .lock()
            .map_err(|e| format!("Master lock poisoned: {}", e))?;
        // A released master resizes to nothing, successfully — the same
        // answer the old no-op placeholder gave after close.
        let Some(m) = master.as_ref() else {
            return Ok(());
        };
        m.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("Failed to resize PTY: {}", e))
    }

    fn wait(&self) -> Result<i32, String> {
        // Take the child OUT of the lock before blocking on it, so a
        // concurrent `pid()`/`kill()` never queues behind a wait that lasts
        // the session's whole life.
        let child = self
            .child
            .lock()
            .map_err(|e| format!("Child lock poisoned: {}", e))?
            .take();
        let Some(mut child) = child else {
            return Err("child already waited on".to_string());
        };
        let status = child.wait().map_err(|e| e.to_string())?;
        // ExitStatus doesn't expose the code directly on all platforms via
        // portable-pty. Use success() check; non-zero falls back to 1.
        Ok(if status.success() { 0 } else { 1 })
    }

    fn kill(&self, budget: Duration) -> Result<(), String> {
        let Some(pid) = self.pid else {
            return Ok(());
        };
        // `/T` is CORRECT here: this is the terminal's OWN shell and whatever
        // it spawned, and leaving that tree behind is precisely the process
        // leak this call exists to prevent. It is categorically different from
        // `/T` on the runner's own PID.
        #[cfg(target_os = "windows")]
        {
            let mut cmd = crate::process_helpers::no_window("taskkill");
            cmd.args(["/F", "/T", "/PID", &pid.to_string()]);
            match crate::drain::output_with_timeout(cmd, budget) {
                Ok(Some(_)) => Ok(()),
                Ok(None) => Err(format!(
                    "taskkill of pid {pid} exceeded its {budget:?} budget — abandoned"
                )),
                Err(e) => Err(format!("taskkill of pid {pid} could not be spawned: {e}")),
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = budget;
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
            Ok(())
        }
    }

    fn set_paused(&self, _paused: bool) -> Result<(), String> {
        // Local flow control gates emission and never pauses reads; there is
        // nothing upstream of a local PTY to hold.
        Ok(())
    }

    fn pid(&self) -> Option<u32> {
        self.pid
    }

    fn credential_scrub(&self) -> CredentialScrub {
        CredentialScrub::InProcessEnv
    }

    fn release(&self, budget: Duration) -> Result<(), String> {
        // Dropping the master closes the OS pipe and unblocks a reader thread
        // stuck in a blocking `read()`. Bounded: a lock held by a thread
        // blocked on a full PTY must not park a shutdown past its slice.
        match crate::safe_lock::lock_with_deadline(&self.master, "terminal master pty", budget) {
            Some(mut master) => {
                drop(master.take());
                Ok(())
            }
            None => Err(format!(
                "Could not acquire the master-PTY lock for {} within the shutdown budget",
                self.label
            )),
        }
    }
}

/// An inert [`PaneIo`] for session fixtures that never spawn threads: the
/// reader is empty, the writer is a sink, everything else succeeds.
#[cfg(test)]
pub(crate) struct InertPaneIo;

#[cfg(test)]
impl PaneIo for InertPaneIo {
    fn reader(&self) -> Result<Box<dyn Read + Send>, String> {
        Ok(Box::new(std::io::empty()))
    }
    fn writer(&self) -> Result<Box<dyn Write + Send>, String> {
        Ok(Box::new(std::io::sink()))
    }
    fn resize(&self, _cols: u16, _rows: u16) -> Result<(), String> {
        Ok(())
    }
    fn wait(&self) -> Result<i32, String> {
        Ok(0)
    }
    fn kill(&self, _budget: Duration) -> Result<(), String> {
        Ok(())
    }
    fn set_paused(&self, _paused: bool) -> Result<(), String> {
        Ok(())
    }
    fn pid(&self) -> Option<u32> {
        None
    }
    fn credential_scrub(&self) -> CredentialScrub {
        CredentialScrub::NoChildEnv
    }
    fn release(&self, _budget: Duration) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A scripted, in-memory [`PaneIo`]: output is a fixed byte script, input
    /// lands in a shared buffer, and every control call is recorded. Proves
    /// the seam is complete — a consumer written against the trait needs no
    /// PTY to drive a full read → write → resize → pause → wait → release
    /// lifecycle.
    struct ScriptedPaneIo {
        script: Vec<u8>,
        input: Arc<Mutex<Vec<u8>>>,
        resizes: Mutex<Vec<(u16, u16)>>,
        paused: AtomicBool,
        pause_calls: AtomicUsize,
        exit_code: i32,
        killed: AtomicBool,
        released: AtomicBool,
    }

    impl ScriptedPaneIo {
        fn new(script: &[u8], exit_code: i32) -> Self {
            Self {
                script: script.to_vec(),
                input: Arc::new(Mutex::new(Vec::new())),
                resizes: Mutex::new(Vec::new()),
                paused: AtomicBool::new(false),
                pause_calls: AtomicUsize::new(0),
                exit_code,
                killed: AtomicBool::new(false),
                released: AtomicBool::new(false),
            }
        }
    }

    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl PaneIo for ScriptedPaneIo {
        fn reader(&self) -> Result<Box<dyn Read + Send>, String> {
            Ok(Box::new(std::io::Cursor::new(self.script.clone())))
        }
        fn writer(&self) -> Result<Box<dyn Write + Send>, String> {
            Ok(Box::new(SharedWriter(self.input.clone())))
        }
        fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
            self.resizes.lock().unwrap().push((cols, rows));
            Ok(())
        }
        fn wait(&self) -> Result<i32, String> {
            Ok(self.exit_code)
        }
        fn kill(&self, _budget: Duration) -> Result<(), String> {
            self.killed.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn set_paused(&self, paused: bool) -> Result<(), String> {
            self.pause_calls.fetch_add(1, Ordering::SeqCst);
            self.paused.store(paused, Ordering::SeqCst);
            Ok(())
        }
        fn pid(&self) -> Option<u32> {
            None
        }
        fn credential_scrub(&self) -> CredentialScrub {
            CredentialScrub::NoChildEnv
        }
        fn release(&self, _budget: Duration) -> Result<(), String> {
            self.released.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    /// The whole lifecycle a session drives, through `dyn PaneIo` alone.
    #[test]
    fn scripted_pane_drives_the_full_lifecycle_without_a_pty() {
        let pane: Arc<dyn PaneIo> = Arc::new(ScriptedPaneIo::new(b"hello from the pane\r\n", 7));

        // Read: the reader thread's loop shape, to EOF.
        let mut reader = pane.reader().expect("reader");
        let mut out = Vec::new();
        let mut buf = [0u8; 8];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
            }
        }
        assert_eq!(out, b"hello from the pane\r\n");

        // Write + flush: the input path.
        let mut writer = pane.writer().expect("writer");
        writer.write_all(b"ls\r").expect("write");
        writer.flush().expect("flush");

        pane.resize(120, 40).expect("resize");
        pane.set_paused(true).expect("pause");
        pane.set_paused(false).expect("resume");
        assert_eq!(pane.wait().expect("wait"), 7);
        pane.kill(Duration::from_millis(1)).expect("kill");
        pane.release(Duration::from_millis(1)).expect("release");
        assert_eq!(pane.pid(), None);
        assert_eq!(pane.credential_scrub(), CredentialScrub::NoChildEnv);
    }

    /// Same lifecycle, but asserting on the double's own record — the calls
    /// really reached the implementation rather than being absorbed by a
    /// default.
    #[test]
    fn scripted_pane_records_every_control_call() {
        let pane = ScriptedPaneIo::new(b"", 0);

        {
            let mut writer = pane.writer().expect("writer");
            writer.write_all(b"typed\r").expect("write");
        }
        pane.resize(80, 24).expect("resize");
        pane.resize(132, 50).expect("resize");
        pane.set_paused(true).expect("pause");
        assert!(pane.paused.load(Ordering::SeqCst));
        pane.set_paused(false).expect("resume");
        assert!(!pane.paused.load(Ordering::SeqCst));
        pane.kill(Duration::ZERO).expect("kill");
        pane.release(Duration::ZERO).expect("release");

        assert_eq!(pane.input.lock().unwrap().as_slice(), b"typed\r");
        assert_eq!(*pane.resizes.lock().unwrap(), vec![(80, 24), (132, 50)]);
        assert_eq!(pane.pause_calls.load(Ordering::SeqCst), 2);
        assert!(pane.killed.load(Ordering::SeqCst));
        assert!(pane.released.load(Ordering::SeqCst));
    }

    /// The credential scrub is discharged by the ONLY constructor of
    /// [`ScrubbedCommand`], against the one shared name list — seeded first so
    /// the assertion cannot pass vacuously (see `assert_credentials_scrubbed_pty`).
    #[test]
    fn sealing_a_command_scrubs_every_credential_value() {
        let mut cmd = CommandBuilder::new("dummy");
        for name in crate::terminal::CREDENTIAL_VALUE_ENV_VARS {
            cmd.env(name, "hunter2");
        }
        cmd.env("KEEP_ME", "yes");

        let sealed = ScrubbedCommand::seal(cmd);

        crate::terminal::assert_credentials_scrubbed_pty(
            sealed.as_command(),
            "pane_io::ScrubbedCommand::seal",
        );
        assert_eq!(
            sealed
                .as_command()
                .get_env("KEEP_ME")
                .and_then(|v| v.to_str()),
            Some("yes"),
            "the seal removes credentials and nothing else"
        );
    }

    /// The inert double answers the way the old `NoopMaster` placeholder did:
    /// a resize after release still succeeds, and there is nothing to read.
    #[test]
    fn inert_pane_is_a_faithful_noop() {
        let pane = InertPaneIo;
        pane.release(Duration::ZERO).expect("release");
        pane.resize(100, 40).expect("resize after release");
        let mut reader = pane.reader().expect("reader");
        let mut buf = [0u8; 4];
        assert_eq!(reader.read(&mut buf).expect("read"), 0);
        assert_eq!(pane.credential_scrub(), CredentialScrub::NoChildEnv);
    }

    /// A real local PTY through the seam: open, spawn a one-shot echo through
    /// a sealed command, read its output via `reader()`, wait for exit via
    /// `wait()`, release. The same shape as `drive_real_pty_into` in
    /// `session.rs`, but with nothing but `dyn PaneIo` in the caller's hands.
    #[test]
    fn local_pty_round_trips_through_the_trait() {
        let mut cmd = if cfg!(windows) {
            let mut c = CommandBuilder::new("cmd");
            c.arg("/C");
            c.arg("echo PANEIO_MARKER");
            c
        } else {
            let mut c = CommandBuilder::new("sh");
            c.arg("-c");
            c.arg("echo PANEIO_MARKER");
            c
        };
        cmd.env("TERM", "xterm-256color");
        // Seed a credential so the seal is exercised on the production path,
        // not only in the unit test above.
        for name in crate::terminal::CREDENTIAL_VALUE_ENV_VARS {
            cmd.env(name, "hunter2");
        }

        let pane: Arc<dyn PaneIo> = Arc::new(
            LocalPty::open("paneio-test", 80, 24)
                .expect("openpty")
                .spawn(ScrubbedCommand::seal(cmd))
                .expect("spawn"),
        );
        assert_eq!(pane.credential_scrub(), CredentialScrub::InProcessEnv);
        assert!(pane.pid().is_some(), "a spawned child has a pid");

        let mut reader = pane.reader().expect("reader");
        let collected = Arc::new(Mutex::new(Vec::new()));
        let sink = collected.clone();
        // Own thread, like production: on ConPTY `read()` keeps blocking after
        // the child exits until the MASTER is released.
        let reader_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => sink.lock().unwrap().extend_from_slice(&buf[..n]),
                }
            }
        });

        let code = pane.wait().expect("wait");
        assert_eq!(code, 0, "echo exits cleanly");
        assert!(
            matches!(pane.wait(), Err(_)),
            "a second wait has no child to wait on"
        );

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if String::from_utf8_lossy(&collected.lock().unwrap()).contains("PANEIO_MARKER") {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        pane.release(Duration::from_secs(2)).expect("release");
        let _ = reader_thread.join();

        let text = String::from_utf8_lossy(&collected.lock().unwrap()).to_string();
        assert!(
            text.contains("PANEIO_MARKER"),
            "read via the seam: {text:?}"
        );
        pane.resize(100, 40)
            .expect("resize after release is a successful no-op");
        assert!(
            matches!(pane.reader(), Err(_)),
            "no reader after the master is released"
        );
    }
}
