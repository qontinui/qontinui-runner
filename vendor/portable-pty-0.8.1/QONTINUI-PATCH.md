# portable-pty 0.8.1 — Qontinui patch

Upstream: wezterm `pty/` (crates.io `portable-pty` 0.8.1). Upstream `main` and
0.9.0 still carry the behaviour patched here.

## Delta (src/unix.rs, `close_random_fds`)

Skip fds that have `FD_CLOEXEC` set instead of closing every fd above 2.

## Why

`close_random_fds` runs in the forked child's `pre_exec`. std's `Command::spawn`
reports an exec failure to the parent through a close-on-exec pipe; closing that
pipe makes the failed-exec report impossible, so std aborts the child with
`fatal runtime error: assertion failed: output.write(&bytes).is_ok()` on the
pane's terminal, and the parent sees EOF and believes the spawn succeeded
(runner log: gate-continuation "never started", pty exited code 1).
Observed 6 times 2026-10-17..19 during `claude` reinstall windows.
CLOEXEC fds are closed by the kernel at a successful exec, so leaving them open
leaks nothing into the child program.

## Removing

Drop the `[patch.crates-io] portable-pty` entry in the root `Cargo.toml` and
delete this directory once an upstream release carries the same fix.
