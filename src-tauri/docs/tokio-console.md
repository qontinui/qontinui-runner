# `tokio-console` (dev-only, `debug-tokio-console`)

Phase 5 of `2026-08-30-runner-blocking-pool-exhaustion-and-wedge-diagnostics`.

## What it is for

The bug class this exists for is the **wedge**: the runner process is alive, its
threads are all accounted for, and it answers nothing. Either tokio's blocking
pool is pinned at its 512-thread ceiling, or a worker thread is parked inside a
call that will never return, and the only symptom anyone sees is "requests time
out".

Phase 4 adds a native OS-level census — thread count, handle count, child
processes. That proves the process is *in* the wedged state and is enough to
raise an alarm. It **cannot**, structurally, say *which async task* is stalled or
*for how long*: the operating system has no concept of a tokio task, so a thread
dump shows you a parked worker and nothing about the future it was polling.

`tokio-console` reads the runtime's own task graph, which is the only place that
information exists:

- per-task poll duration (and the busy/idle split, which separates *blocked*
  from *starved*),
- what each task is currently awaiting,
- task-level contention and the "this task has not yielded in N seconds" warning,
- live blocking-pool and worker-thread occupancy.

That is real diagnostic capability nothing else in this crate provides. It is
here to reproduce and diagnose the **next** instance of this bug class live on a
dev box.

## Why it is off by default and cannot ship

`console-subscriber` only functions if the whole dependency graph — tokio
included — was compiled with `--cfg tokio_unstable`. That is a **build-wide rustc
flag, not a Cargo feature**, and it opts the binary into tokio's explicitly
unstable API surface, whose compatibility guarantees are deliberately weaker than
tokio's semver promise. A shipped desktop binary must not be built that way, and
`tokio-console` is a developer's debugger, not an end-user capability.

Three independent properties keep it out of a shipped build:

1. **`debug-tokio-console` is absent from `default`.** `console-subscriber` is an
   `optional = true` dependency reached only through that feature, so a normal
   `cargo build` / `cargo tauri build` never compiles it. Verify with
   `cargo tree -i console-subscriber` — it reports nothing for a default build.
2. **`--cfg tokio_unstable` is set nowhere in this repository.** Not in
   `src-tauri/.cargo/config.toml`, not in `build.rs`, not in CI. A
   `[build] rustflags` entry was rejected precisely because it applies
   unconditionally to *every* build of the crate, release bundles included. The
   flag exists only in the environment of the developer command below.
3. **`build.rs` fails the build if 1 and 2 disagree.** `guard_tokio_console_cfg`
   reads `CARGO_ENCODED_RUSTFLAGS` (Cargo's authoritative, config-inclusive flag
   list) and panics with a one-line explanation when the feature is on and the
   cfg is missing. Without it the failure would be a runtime `assert!` inside
   `ConsoleLayer::build` — a binary that compiles and then panics on launch.

## Running it

Install the client once:

```bash
cargo install --locked tokio-console
```

Build and run the runner with the feature. Use the wrapper — it composes the
rustflags correctly (see the caveat below):

```bash
# bash / WSL
scripts/dev-tokio-console.sh run

# PowerShell (this fleet's primary dev box)
scripts\dev-tokio-console.ps1 -Action run
```

By hand, from `src-tauri/`:

```bash
RUSTFLAGS="--cfg tokio_unstable" cargo run --features debug-tokio-console
```

Then attach from a second terminal:

```bash
tokio-console http://127.0.0.1:6669
```

The runner logs one INFO line at startup naming the address it bound:

```
tokio-console instrumentation ACTIVE — attach with `tokio-console http://127.0.0.1:6669` …
```

Override the listen address with the standard `TOKIO_CONSOLE_BIND` environment
variable (`ip:port`); `console_subscriber::Builder::with_default_env` reads it,
and the log line reports whatever it resolved to.

## Two caveats that will otherwise cost you an hour

### Changing `RUSTFLAGS` invalidates the entire build cache

`RUSTFLAGS` is part of every compile unit's fingerprint. The first build with
`--cfg tokio_unstable` rebuilds the whole dependency graph, and so does the first
build after you switch back. That is expected, not a fault. Budget for it, and
prefer a dedicated `CARGO_TARGET_DIR` if you want to keep both fingerprints warm.

### `RUSTFLAGS` **replaces** `.cargo/config.toml` rustflags — it does not merge

Cargo picks exactly one source of rustflags, in precedence order
`CARGO_ENCODED_RUSTFLAGS` > `RUSTFLAGS` > `[target.<triple>] rustflags` >
`[build] rustflags`. Setting `RUSTFLAGS` by hand therefore **silently drops**
everything in `src-tauri/.cargo/config.toml`:

- `/STACK:8388608` on `x86_64-pc-windows-msvc` — without it, linking the large
  test binary fails with `STATUS_STACK_BUFFER_OVERRUN`;
- `/Brepro` and `--build-id=none`, which the content-addressed build cache needs
  for byte-identical output;
- the `--remap-path-prefix` entries that make sccache keys worktree-independent.

`scripts/dev-tokio-console.{sh,ps1}` re-state the host's flags and append
`--cfg tokio_unstable`. They also use `CARGO_ENCODED_RUSTFLAGS` rather than
`RUSTFLAGS`, because plain `RUSTFLAGS` is split on whitespace with no quoting and
the Windows value `-C link-args=/STACK:8388608 /Brepro` cannot survive that.
**If you edit the `[target.*]` blocks in `.cargo/config.toml`, update both
scripts.**

## How it is wired

`src-tauri/src/logging.rs`, module `tokio_console` (itself
`#[cfg(feature = "debug-tokio-console")]`), plus a handful of `#[cfg]` arms in
`init_logging`. The existing logging stack — rolling file appender, non-blocking
console writer, JSONL span layer, OTel layer — is **added to, never replaced**:
every other diagnostic in this crate reads those, and the non-feature path is
behaviourally identical to a build without the feature.

Two subtleties are worth knowing if you touch it:

- **The global `EnvFilter` had to be widened.** `init_logging` installs
  `EnvFilter` as a *global* filter layer, and a global filter short-circuits
  `Subscriber::enabled` for every layer, per-layer-filtered ones included. The
  default directives (`qontinui_runner=info,tauri=info`) match neither `tokio`
  nor `runtime`, so without `tokio_console::widen_env_filter` the console layer
  would bind its port and then show an empty task list forever — tokio would
  never construct the spans at all.
- **…and the human logs are then filtered back down.** Widening the global filter
  turns on one TRACE callsite per poll per task. `not_runtime_instrumentation`
  (the exact complement of the predicate `console_subscriber` attaches to its own
  layer) is applied to the file, console, JSONL and OTel layers, so those
  callsites reach the console collector and nothing else. Log volume in a
  `debug-tokio-console` build therefore matches a default build.
