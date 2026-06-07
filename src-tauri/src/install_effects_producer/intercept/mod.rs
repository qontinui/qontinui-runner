//! **Transparent interception** of agent-typed package installs (plan
//! `2026-06-07-install-interception-pty-shim-plan.md`).
//!
//! An agent (a Claude Code session in a runner-spawned terminal) that types
//! `npm install left-pad` bypasses the explicit `POST /install-effects/run`
//! route entirely — no declare, no gate, no observe/verify. This module closes
//! that gap by prepending a per-terminal PATH-shim bin dir at PTY spawn. The
//! shim brackets the real tool's exec with two runner loopback calls:
//!
//! 1. **pre**  `POST /install-effects/run {mode:"intercept"}` — declare +
//!    predict-and-check + pre-sha capture, NO install. Returns a
//!    `correlation_id` and stashes a [`precontext::PreContext`] (TTL'd).
//! 2. the shim execs the **real** tool (transparent stdio).
//! 3. **post** `POST /install-effects/observe-verify {correlation_id, exit}` —
//!    runs the shared `observe_and_verify` body against the now-mutated
//!    worktree and pushes the signature to coord.
//!
//! Phase 1 ships **observe-only** for npm, behind the master flag
//! `QONTINUI_INSTALL_INTERCEPT_ENABLED` (default OFF — ships dark).
//!
//! ## Staged rollout flags (runner-process env, read at terminal spawn)
//! - `QONTINUI_INSTALL_INTERCEPT_ENABLED` — **master switch** (default OFF). Off
//!   ⇒ no shim dir, byte-identical child env. Truthy = `1`/`true`/`yes`/`on`.
//! - `QONTINUI_INSTALL_INTERCEPT_MODE` — `observe` (Phase 1/2; declare+observe,
//!   NEVER blocks) | `gate` (Phase 3; an `escalate` verdict blocks an
//!   un-overridden, non-lockfile-sync, non-`npx` install). Any other value
//!   (typo/empty/absent) fails open to `observe` — a garbled mode never starts
//!   blocking. See [`gate::InterceptMode::parse`].
//! The shim also reads, per-invocation: `QONTINUI_INSTALL_INTERCEPT_PORT` (the
//! BOUND runner loopback port), `QONTINUI_INSTALL_OVERRIDE=1` (the audited
//! `+overridden` re-run), and `QONTINUI_INSTALL_INTERCEPT_GUARD=1` (recursion
//! guard — a nested shim is a pure passthrough).
//!
//! ## The two-call straddle (plan §3 A3)
//! A shim brackets the real tool's exec with two runner loopback calls: **pre**
//! `POST /install-effects/run {mode:"intercept"}` (declare + predict-and-check +
//! pre-sha capture, returns `correlation_id`, stashes a TTL'd
//! [`precontext::PreContext`]); the shim runs the **real** tool transparently;
//! **post** `POST /install-effects/observe-verify {correlation_id, exit}` (runs
//! the shared `observe_and_verify` against the now-mutated worktree). The
//! producer stays a pure request/response; the shim owns the straddle.
//!
//! ## Fail-open is a HARD invariant (plan §6 / §4 Phase 4)
//! The agent's shell must NEVER be bricked by interception being down. Absent
//! port / connect-refused / non-2xx / malformed-response / short-timeout ⇒ the
//! shim logs ONE stderr line on the pre-call failure path only
//! (`qontinui: install interception unavailable — running normally`) and execs
//! the REAL tool unchanged. Pre-call uses a 3s connect timeout (20s total);
//! post-call 30s total. The compiled `.exe` stub mirrors this and never panics
//! (every fallible step falls open to exec-ing the real tool). Materialize /
//! cleanup IO failures are best-effort: a shim we can't write injects nothing.
//!
//! ## Windows shadowing policy incl. the compiled stub (plan §6)
//! - npm / npx / pnpm / yarn ship as `<name>.cmd` ⇒ a materialized `<name>.cmd`
//!   shim WINS under PowerShell/cmd (PATHEXT, no `.exe` to outrank it) + an
//!   extensionless `<name>` script wins under Git Bash.
//! - cargo / pip / pip3 ship as `<name>.exe`; a `.cmd` CANNOT shadow a `.exe`
//!   (`.EXE` precedes `.CMD` in PATHEXT). The runner therefore materializes the
//!   compiled [`qontinui-shim`](../../../bin/qontinui_shim.rs) binary as
//!   `cargo.exe`/`pip.exe`/`pip3.exe`, which DOES win; it detects which tool it
//!   impersonates from `argv[0]` and reuses [`crate::install_effects_producer::intercept::classify`]
//!   + [`gate::should_block`]. If the stub binary is absent (dev builds), the
//!   materializer falls back to scripts-only with a debug log (fail-open) — under
//!   Git Bash the extensionless shim still covers them.
//! - On Unix a single extensionless executable shell script per tool suffices.
//!
//! ## Registry credentials (plan §4 Phase 4)
//! Interception does **NOT** inject registry secrets into the agent's shell. The
//! agent's shell already carries the operator's normal registry config (its
//! `.npmrc` / `~/.cargo/credentials` / pip index auth), and the real install the
//! shim runs uses THAT. The producer's pre-call dry-run uses the OS keyring as
//! it does for the explicit route today (parent plan #441 Phase 4). Interception
//! OBSERVES; it does not re-auth. The shim and the lib-crate
//! [`crate::install_effects_producer::intercept::classify`] / [`gate`] logic are
//! credential-free by construction.
//!
//! ## Out of scope (plan §5)
//! Agent shells NOT spawned by the runner (external terminals, SSH-elsewhere,
//! CI) are never intercepted — interception only covers PTYs created through
//! `TerminalManager::create`, where the runner controls the env. Design B/C
//! (shell-integration hook, PTY-stream parsing) are rejected; Go/maven/gradle
//! and the `Full` explicit route are unchanged.
//!
//! ## Module layout
//! - [`classify`] — the single Rust source of truth for install-verb tables +
//!   argv parsing (mirrored into the shim scripts).
//! - [`gate`] — the pure `should_block` truth table (Phase 3 gating semantics)
//!   the shim's gate branch transliterates, plus the `observe`/`gate` mode parse.
//! - [`shim_materializer`] — writes the per-terminal shim scripts + computes the
//!   terminal env-seam mutations.
//! - [`precontext`] — the TTL'd `correlation_id → PreContext` stash for the
//!   two-call straddle.
//!
//! The producer endpoints (`mode:"intercept"` branch + `/observe-verify`) live
//! in the parent [`super`] module so they share `run_with_base` /
//! `observe_and_verify` without duplication.
//!
//! ## Bound-port plumbing (plan §6 — the #1 live-verification footgun)
//! The shim must loop back to the **actually-bound** runner port, not the
//! bootstrap default `get_mcp_api_port()` (wrong for secondary/temp runners).
//! The bound port lives in `app_state.api_port`, but the terminal-spawn call
//! chain (`TerminalManager::create` → `TerminalSession::spawn`) does NOT carry
//! `app_state`. Threading it through would churn every caller of `create`. So we
//! stash the bound port in a process-global [`set_bound_port`] at the SAME place
//! the server records it (`mcp_api.rs`, right after `api_port.store`). The seam
//! reads it via [`bound_port`]. This is the "documented OnceLock set-at-bind"
//! option the plan sanctions when the call chain lacks `app_state`.

pub mod classify;
pub mod gate;
pub mod precontext;
pub mod shim_materializer;

use std::sync::atomic::{AtomicU16, Ordering};

/// Process-global bound runner API port for the interception shim loopback.
/// `0` = not yet bound (the seam then falls back to the bootstrap port as a
/// last resort, but in practice `set_bound_port` runs before any terminal can
/// spawn). Set once by `mcp_api.rs` immediately after the listener binds.
static BOUND_PORT: AtomicU16 = AtomicU16::new(0);

/// Record the actually-bound runner API port. Called from `mcp_api.rs` right
/// after `app_state.api_port.store(...)` so the terminal env-seam (which has no
/// `app_state`) can read the correct port for the shim loopback.
pub fn set_bound_port(port: u16) {
    BOUND_PORT.store(port, Ordering::Relaxed);
}

/// The bound runner API port for the shim loopback. Falls back to the bootstrap
/// port (`get_mcp_api_port`) only if the bind hasn't recorded one yet — a window
/// that should never be hit in practice (bind precedes terminal spawn).
pub fn bound_port() -> u16 {
    let p = BOUND_PORT.load(Ordering::Relaxed);
    if p != 0 {
        p
    } else {
        crate::mcp::types::get_mcp_api_port()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_port_roundtrips_and_falls_back() {
        // Fresh process state may already have a value from another test; assert
        // the set/get contract rather than a specific initial value.
        set_bound_port(54321);
        assert_eq!(bound_port(), 54321);
        set_bound_port(0);
        // Falls back to the bootstrap port (non-zero) when unset.
        assert_ne!(bound_port(), 0);
    }

    // =======================================================================
    // BASH-LEVEL SMOKE (plan §4 Phase 3 verify step 5).
    //
    // Render the npm shim, point it at a tiny single-shot HTTP stub that returns
    // a canned escalate pre-call response, put a SENTINEL fake "real npm" (writes
    // a marker file) on PATH, and run the shim under `bash`. Assert:
    //   * gate mode + escalate + no override -> exit 1 + the A4 message, sentinel
    //     marker ABSENT (the real tool never ran).
    //   * override env -> sentinel marker PRESENT (real tool ran), exit 0.
    // Skips gracefully where bash is unavailable; this machine has Git Bash.
    // =======================================================================

    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};

    /// A canned pre-call HTTP response body the stub returns for every request.
    fn escalate_pre_response() -> String {
        // Matches the producer's RunResponse shape for the fields the shim reads.
        r#"{"correlation_id":"11111111-2222-3333-4444-555555555555","repo":"widget","package_manager":"npm","gate":"escalate","risk_factors":["major version bump","high-severity advisory"],"escalation_overridden":false,"dry_run_only":false,"resolution":{}}"#.to_string()
    }

    /// A minimal HTTP/1.1 server that answers EVERY request (pre-call AND any
    /// post-call) with `body`. Thread-per-connection (so a slow/concurrent
    /// post-call never blocks the next accept), and each connection is fully
    /// served + explicitly shut down so `curl` sees EOF and exits promptly (no
    /// `cmd.output()` pipe-inheritance hang). Returns the bound port; the
    /// listener thread runs until the test process exits (daemon).
    fn spawn_http_stub(body: String) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let body = body.clone();
                if let Ok(mut stream) = conn {
                    std::thread::spawn(move || {
                        // Respond IMMEDIATELY without blocking on a full request
                        // read: curl may use Expect:100-continue and withhold its
                        // body until a 100 — and we don't need the body. A short
                        // read-timeout drains what's already buffered without ever
                        // dead-locking on a body that never arrives.
                        let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(50)));
                        let mut buf = [0u8; 8192];
                        let _ = stream.read(&mut buf);
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.flush();
                        // Close the write half so curl gets EOF immediately.
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                    });
                }
            }
        });
        port
    }

    /// Resolve a usable `bash` (Git Bash on Windows). `None` ⇒ skip the smoke.
    fn find_bash() -> Option<PathBuf> {
        let candidates = if cfg!(windows) {
            vec![
                PathBuf::from("C:/Program Files/Git/bin/bash.exe"),
                PathBuf::from("C:/Program Files/Git/usr/bin/bash.exe"),
                PathBuf::from("bash.exe"),
                PathBuf::from("bash"),
            ]
        } else {
            vec![PathBuf::from("/bin/bash"), PathBuf::from("bash")]
        };
        for c in candidates {
            if c.is_absolute() && c.exists() {
                return Some(c);
            }
            // Bare name: probe via `command -v` semantics by trying to run it.
            if std::process::Command::new(&c)
                .arg("-c")
                .arg("exit 0")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
            {
                return Some(c);
            }
        }
        None
    }

    /// Materialize a runnable npm shim into `dir`, substituting placeholders and
    /// (since these tests run the EXTENSIONLESS bash script directly) returning
    /// its path. `shim_dir` is recorded so the real-tool scan skips it.
    fn write_npm_shim(dir: &Path) -> PathBuf {
        let bash_tmpl = include_str!("../../../resources/intercept/shim.bash");
        let rendered = shim_materializer::render_for_test(bash_tmpl, classify::ShimTool::Npm, dir);
        let shim = dir.join("npm");
        std::fs::write(&shim, rendered).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&shim).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&shim, p).unwrap();
        }
        shim
    }

    /// Write a sentinel "real npm" onto a SEPARATE real-bin dir: a bash script
    /// that creates `marker` and exits 0. The shim resolves it via PATH (the
    /// shim dir is skipped). Returns the real-bin dir.
    fn write_sentinel_real_npm(real_bin: &Path, marker: &Path) {
        std::fs::create_dir_all(real_bin).unwrap();
        let body = format!(
            "#!/usr/bin/env bash\nprintf 'ran real npm\\n'\ntouch '{}'\nexit 0\n",
            marker.display()
        );
        let p = real_bin.join("npm");
        std::fs::write(&p, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&p).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&p, perms).unwrap();
        }
    }

    #[test]
    fn bash_smoke_gate_blocks_escalate_and_override_runs() {
        let bash = match find_bash() {
            Some(b) => b,
            None => {
                eprintln!("bash unavailable — skipping shim smoke test");
                return;
            }
        };

        let tmp = tempfile::tempdir().unwrap();
        let shim_dir = tmp.path().join("shim");
        std::fs::create_dir_all(&shim_dir).unwrap();
        write_npm_shim(&shim_dir);

        let real_bin = tmp.path().join("realbin");
        let marker = tmp.path().join("MARKER");
        write_sentinel_real_npm(&real_bin, &marker);

        let port = spawn_http_stub(escalate_pre_response());

        // Pre-flight: some Windows + Git-Bash setups don't let a `bash.exe`
        // spawned via Rust `std::process::Command` report exit promptly when the
        // shim forks grandchildren (curl/the sentinel) — the SHIM itself is fine
        // (a direct `bash <shim>` returns in <8s) but the Command-tracked handle
        // never observes exit. Detect that here with a short fail-open probe (no
        // reachable port ⇒ the shim exec's the real tool in <8s). If even that
        // doesn't return within the budget, SKIP the smoke (it cannot run on this
        // host) rather than hang — the gate semantics are still fully covered by
        // the pure `gate::should_block` truth table + the override endpoint test.

        // The PATH the shim's child sees: shim dir FIRST (so `npm` resolves to our
        // shim if re-invoked), then real_bin (where the sentinel lives), then a
        // minimal system path so `curl`/`grep`/`sed`/`printf` resolve.
        let sys = std::env::var("PATH").unwrap_or_default();
        let sep = if cfg!(windows) { ";" } else { ":" };
        let path = format!(
            "{}{sep}{}{sep}{}",
            shim_dir.display(),
            real_bin.display(),
            sys
        );

        // Helper: run the shim under `bash -c` with the given extra env. BOUNDED:
        // spawn + poll with a hard deadline (kill on overrun) so it can NEVER
        // hang `cargo test`. The shim's OWN stderr is redirected to a FILE inside
        // the bash `-c` driver (not an inherited host pipe) — on Windows a
        // Git-Bash grandchild (curl / the sentinel) can keep an inherited pipe
        // handle open, blocking exit observation; a file sidesteps the pipe.
        // The host stdio is /dev/null. Returns (timed_out, exit_code, stderr).
        let run_shim_p = |mode: &str,
                          override_on: bool,
                          use_port: u16,
                          budget_secs: u64|
         -> (bool, Option<i32>, String) {
            use std::process::Stdio;
            let shim_path = shim_dir.join("npm");
            let stderr_file = shim_dir.parent().unwrap().join(format!(
                "shim-stderr-{}-{}-{}.txt",
                mode,
                if override_on { "ov" } else { "noov" },
                use_port
            ));
            let _ = std::fs::remove_file(&stderr_file);
            let driver = format!(
                "exec '{}' install left-pad 2> '{}'",
                shim_path.display().to_string().replace('\\', "/"),
                stderr_file.display().to_string().replace('\\', "/")
            );
            let mut cmd = std::process::Command::new(&bash);
            cmd.arg("-c")
                .arg(&driver)
                .env("PATH", &path)
                .env("QONTINUI_INSTALL_INTERCEPT_PORT", use_port.to_string())
                .env("QONTINUI_INSTALL_INTERCEPT_MODE", mode)
                .env_remove("QONTINUI_INSTALL_INTERCEPT_GUARD")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if override_on {
                cmd.env("QONTINUI_INSTALL_OVERRIDE", "1");
            } else {
                cmd.env_remove("QONTINUI_INSTALL_OVERRIDE");
            }
            let mut child = cmd.spawn().expect("spawn shim under bash");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(budget_secs);
            let mut timed_out = false;
            let status = loop {
                match child.try_wait().expect("try_wait") {
                    Some(st) => break st,
                    None => {
                        if std::time::Instant::now() >= deadline {
                            timed_out = true;
                            let _ = child.kill();
                            let st = child.wait().expect("wait after kill");
                            break st;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }
            };
            let stderr = std::fs::read_to_string(&stderr_file).unwrap_or_default();
            (timed_out, status.code(), stderr)
        };

        // Pre-flight (unreachable port ⇒ fast fail-open). Use a port nothing
        // listens on so the pre-call connect-refuses and the shim exec's the real
        // tool. If this doesn't return within 20s the host can't run the smoke.
        let _ = std::fs::remove_file(&marker);
        let dead_port = 1; // connect-refused on loopback
        let (pf_timed_out, _pf_code, _pf_err) = run_shim_p("observe", false, dead_port, 20);
        if pf_timed_out {
            eprintln!(
                "bash-under-Command does not report exit promptly on this host — \
                 skipping the shim smoke (gate semantics covered by gate::should_block \
                 + the override endpoint test)"
            );
            return;
        }

        // Bounded wrapper at the smoke port with a 40s budget.
        let run_shim = |mode: &str, override_on: bool| -> (Option<i32>, String) {
            let (_to, code, err) = run_shim_p(mode, override_on, port, 40);
            (code, err)
        };

        // ---- 1) gate mode + escalate + NO override -> BLOCK -------------------
        let _ = std::fs::remove_file(&marker);
        let (code, stderr) = run_shim("gate", false);
        assert_eq!(
            code,
            Some(1),
            "gate+escalate blocks with exit 1; stderr: {stderr}"
        );
        assert!(
            stderr.contains("predicted RISKY and was blocked"),
            "A4 message on stderr: {stderr}"
        );
        assert!(
            stderr.contains("QONTINUI_INSTALL_OVERRIDE=1"),
            "override hint on stderr: {stderr}"
        );
        assert!(
            !marker.exists(),
            "the real tool must NOT have run when blocked"
        );

        // ---- 2) override env -> the real tool RUNS ---------------------------
        let _ = std::fs::remove_file(&marker);
        let (code, stderr) = run_shim("gate", true);
        assert_eq!(
            code,
            Some(0),
            "override runs the sentinel (exit 0); stderr: {stderr}"
        );
        assert!(
            marker.exists(),
            "override must run the real tool (sentinel marker present)"
        );

        // ---- 3) observe mode never blocks (even on escalate) -----------------
        let _ = std::fs::remove_file(&marker);
        let (code, _stderr) = run_shim("observe", false);
        assert_eq!(code, Some(0), "observe never blocks");
        assert!(marker.exists(), "observe runs the real tool");
    }

    // =======================================================================
    // FAIL-OPEN smoke (plan §4 Phase 4 explicit test). Render the npm shim and
    // run it with (a) QONTINUI_INSTALL_INTERCEPT_PORT UNSET and (b) a port
    // nobody listens on. In BOTH cases the shim must run the SENTINEL real tool
    // and exit 0 — interception unavailable ⇒ straight passthrough, the agent's
    // shell is never bricked. Reuses the Phase-3 bounded harness pattern WITH
    // the fail-open pre-flight skip guard (some Windows + Git-Bash hosts can't
    // observe a forking grandchild's exit via std::process::Command).
    // =======================================================================
    #[test]
    fn bash_smoke_fail_open_when_runner_unreachable() {
        let bash = match find_bash() {
            Some(b) => b,
            None => {
                eprintln!("bash unavailable — skipping fail-open smoke test");
                return;
            }
        };

        let tmp = tempfile::tempdir().unwrap();
        let shim_dir = tmp.path().join("shim");
        std::fs::create_dir_all(&shim_dir).unwrap();
        write_npm_shim(&shim_dir);

        let real_bin = tmp.path().join("realbin");
        let marker = tmp.path().join("MARKER");
        write_sentinel_real_npm(&real_bin, &marker);

        let sys = std::env::var("PATH").unwrap_or_default();
        let sep = if cfg!(windows) { ";" } else { ":" };
        let path = format!(
            "{}{sep}{}{sep}{}",
            shim_dir.display(),
            real_bin.display(),
            sys
        );

        // Bounded runner: spawn `bash -c "exec <shim> install left-pad 2> err"`
        // with the given PORT handling, hard-kill on overrun. `port_mode`:
        //   None     → leave QONTINUI_INSTALL_INTERCEPT_PORT UNSET (case a).
        //   Some(p)  → set it to p (case b: a dead port nobody listens on).
        // Returns (timed_out, exit_code, stderr).
        let run = |port_mode: Option<u16>, budget_secs: u64| -> (bool, Option<i32>, String) {
            use std::process::Stdio;
            let shim_path = shim_dir.join("npm");
            let stderr_file = shim_dir.parent().unwrap().join(format!(
                "failopen-stderr-{}.txt",
                port_mode
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "unset".into())
            ));
            let _ = std::fs::remove_file(&stderr_file);
            let driver = format!(
                "exec '{}' install left-pad 2> '{}'",
                shim_path.display().to_string().replace('\\', "/"),
                stderr_file.display().to_string().replace('\\', "/")
            );
            let mut cmd = std::process::Command::new(&bash);
            cmd.arg("-c")
                .arg(&driver)
                .env("PATH", &path)
                .env("QONTINUI_INSTALL_INTERCEPT_MODE", "observe")
                .env_remove("QONTINUI_INSTALL_INTERCEPT_GUARD")
                .env_remove("QONTINUI_INSTALL_OVERRIDE")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            match port_mode {
                Some(p) => {
                    cmd.env("QONTINUI_INSTALL_INTERCEPT_PORT", p.to_string());
                }
                None => {
                    cmd.env_remove("QONTINUI_INSTALL_INTERCEPT_PORT");
                }
            }
            let mut child = cmd.spawn().expect("spawn shim under bash");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(budget_secs);
            let mut timed_out = false;
            let status = loop {
                match child.try_wait().expect("try_wait") {
                    Some(st) => break st,
                    None => {
                        if std::time::Instant::now() >= deadline {
                            timed_out = true;
                            let _ = child.kill();
                            break child.wait().expect("wait after kill");
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }
            };
            let stderr = std::fs::read_to_string(&stderr_file).unwrap_or_default();
            (timed_out, status.code(), stderr)
        };

        // Pre-flight skip guard (host can't observe forking-grandchild exit).
        let _ = std::fs::remove_file(&marker);
        let (pf_to, _c, _e) = run(Some(1), 20);
        if pf_to {
            eprintln!(
                "bash-under-Command does not report exit promptly on this host — \
                 skipping fail-open smoke (covered by the stub's http_post Err path \
                 + the shim's structural pre-call guard)"
            );
            return;
        }

        // ---- (a) PORT unset → fail-open passthrough --------------------------
        let _ = std::fs::remove_file(&marker);
        let (to, code, stderr) = run(None, 20);
        assert!(!to, "must not hang when port is unset");
        assert_eq!(
            code,
            Some(0),
            "fail-open exits with the real tool's code (0)"
        );
        assert!(
            marker.exists(),
            "port unset ⇒ the real tool still ran (passthrough)"
        );
        assert!(
            stderr.contains("install interception unavailable"),
            "the one-time fail-open notice is on stderr: {stderr}"
        );

        // ---- (b) dead port (connect-refused) → fail-open passthrough ---------
        let _ = std::fs::remove_file(&marker);
        let (to, code, stderr) = run(Some(1), 20);
        assert!(!to, "must not hang on a connect-refused port");
        assert_eq!(code, Some(0), "connect-refused ⇒ fail-open exit 0");
        assert!(marker.exists(), "connect-refused ⇒ the real tool still ran");
        assert!(
            stderr.contains("install interception unavailable"),
            "the fail-open notice fires on connect-refused: {stderr}"
        );
    }
}
