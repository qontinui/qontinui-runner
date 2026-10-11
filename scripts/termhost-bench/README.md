# termhost-bench — the measurement contract

Phase 1 of plan `2026-10-11-terminal-page-performance-kpis-across-setups` (qontinui-dev-notes): the pure, display-free half of a harness that measures how the runner's Terminal page holds up under N concurrent AI sessions against VS Code, Windows Terminal and tmux, with ONE outside-in instrument. This directory defines what is measured (KPI catalog), how results are written and checked (schema + validator), how the instrument reads the screen (marker codec), and the load every host receives (generator + profiles + calibrator). Node built-ins only; no `package.json` change (plan D8).

| File                                              | What it is                                                                                                              |
| ------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| `kpi-catalog.mjs`                                 | The KPI catalog as data, plus the vocabularies (reasons, labels, incidents, environments, setups, arms).                |
| `build-schema.mjs` → `result-schema.json`         | The `termhost-bench/v1` JSON Schema (2020-12), GENERATED from the catalog.                                              |
| `validate.mjs`                                    | A validator for the schema subset used (fails closed on any other keyword) + results checks a schema cannot state. CLI. |
| `stats.mjs`                                       | Nearest-rank percentiles, least-squares slope, degradation ratio; samples → KPI values.                                 |
| `marker.mjs` → `marker-golden.json`               | The marker codec and its golden vectors (the contract with Phase 2's Rust decoder).                                     |
| `profile.mjs`, `profiles/*.json`                  | Load profiles: `claude-tui`, `all-repaint`, `bulk`.                                                                     |
| `generator/ai-session.mjs`                        | The AI-session generator (CLI + driver); `schedule.mjs` and `content.mjs` beside it.                                    |
| `calibrate.mjs`                                   | Fits `claude-tui` from `script --log-timing` recordings; writes numbers only.                                           |
| `../tui-repaint-generator.mjs` (+ `.d.mts`, test) | runner#1856's frame builders, carried byte-identical from `db0fea935` — the one repaint implementation.                 |

Tests: `node --test scripts/termhost-bench/__tests__/*.test.mjs` (bash glob). CI runs them in `ci.yml`'s `test` job (step `Run termhost-bench unit tests`, both matrix legs) and in `.qontinui/ci.toml` (`termhost-bench-node-tests`, files listed; `ci-toml-parity.test.mjs` keeps the list honest). After editing the catalog or the codec, regenerate: `node scripts/termhost-bench/build-schema.mjs --write`, `node scripts/termhost-bench/marker.mjs --write-golden` (the tests fail on drift).

## KPI catalog and values

Every KPI value is `{state: measured | unknown | not_applicable, unit, n, value | stats, reason}`:

- **measured** carries a number (`value`, or `stats` keyed by exactly the catalog's statistics), `n >= min_n`, and no `reason`.
- **unknown / not_applicable** carries a `reason` (a `REASON_CODES` key) and NO number — an unmeasured KPI is never rendered as 0. `n` may say how many samples there were.
- Every catalog id appears in every row's `kpis`; none may be omitted.

`min_n` is never below what a statistic needs to mean its name: a nearest-rank p-th percentile needs `ceil(100 / (100 - p))` samples (p95: 20, p99: 100) — below that it IS the maximum. Row fields are documented at the top of `kpi-catalog.mjs`. Semantics the catalog encodes:

- **Attribution.** `host` rows describe the setup's process tree. `cpu_pct` / `memory_mb` EXCLUDE every generator and its parent session shell (slot-file PID → parent PID); that workload is reported as `workload_cpu_pct` / `workload_memory_mb`. The PTY host (`conhost.exe` / `OpenConsole.exe`) stays in the host tree. `shared` rows (`compositor_*` = dwm.exe or the X server, `wsl_vm_*` for tmux-wsl) are attributed to no setup and are `comparable: false`.
- **`marker_loss` vs `marker_skipped`.** Loss: a visible-pane sequence current for >= 2 refresh intervals but never observed, or observed out of order. Skipped: superseded before any composited frame carried it — the host coalesced frames, which is not loss.
- **Interval exclusion** (`exclusion: "valid_intervals"`: `hangs`, `marker_loss`, `marker_skipped`): intervals where the window was not foreground, was cloaked or occluded, or real operator input occurred are excluded; too few valid intervals makes the KPI `unknown(occluded)` / `unknown(operator_activity)`.
- **`degradation_*`**: the KPI's named statistic at N divided by the same setup's value at N = 1 (`degradation_of`); `n` counts measured operands.

`stats.mjs` builds these values: `measureKpi(id, samples | {value, n} | reading)`, `unknownKpi(id, reason)`, `notApplicableKpi(id, reason)`, plus `percentile`, `mean`, `max`, `min`, `share`, `slope`, `degradation`, `summarize` — each returns `{state: "measured", value, n}` or `{state: "unknown", reason, n}`, and throws on a non-finite sample.

## Results files (`termhost-bench/v1`)

One file = one run on one machine in one environment (the report refuses to mix environments, D6): `{schema, run, machine, capabilities, rows[], incidents[]}`; each row is one setup × variant × N × arm × run: `{setup, scenario, kpis, diagnostics, labels, incidents}`. `scenario.profile_calibrated` is on every row. A field that may be unknowable (GPU, display, build, settings digest, `v_actual`, …) holds its value or the honest absence `{state: "unknown" | "not_applicable", reason}` — never `null`. Diagnostics are runner-only and must say `comparable: false` (D1). Validate with `node scripts/termhost-bench/validate.mjs <file>...` (exit 0 valid, 1 invalid, 2 usage/read error) or `validateResults(doc)`; on top of the schema it rejects percentile disorder, mean outside [min, max], coverage above expectation, `run_index >= run_count`, `v_target`/`v_actual > n`, and duplicate rows.

## Marker codec (`marker.mjs`, `marker-golden.json`)

A marker is 20 solid-background cells (a space under `ESC[48;5;<index>m`) encoding (kind, session, seq).

**Palette** — the 8 corners of the xterm 256-colour cube, every channel 0 or 255 (indices 16-231 are fixed in xterm-compatible hosts; schemes remap only 0-15):

| value  | 0     | 1    | 2     | 3    | 4   | 5       | 6      | 7     |
| ------ | ----- | ---- | ----- | ---- | --- | ------- | ------ | ----- |
| colour | black | blue | green | cyan | red | magenta | yellow | white |
| index  | 16    | 21   | 46    | 51   | 196 | 201     | 226    | 231   |

`value = 4R + 2G + B` with each channel bit = (channel is 255).

**Digits** — 20 base-7 digits, left to right: start `[3, 6]` · kind (1: 0 output, 1 echo, 2 calibration; 3-6 reserved) · session (3, big-endian, 0..342) · seq (6, big-endian, the counter mod 117649) · checksum (5) · end `[5, 1, 3]`. Cell i's value is `(value[i-1] + 1 + digit[i]) mod 8` with `value[-1] = 0`, so adjacent cells always differ and the first two cells are always red, cyan. Checksum: `h = 0; for d in [kind, session digits, seq digits]: h = (h * 31 + d + 1) mod 16787` (the largest prime below 7^5), written as 5 base-7 digits. Every single-cell error is detected (tested exhaustively).

**Regions and cadence** — output markers occupy row 0, columns 0-19 (0-based); echo markers row 1, columns 0-19. Every write the generator makes re-draws the current marker(s), so a repaint that paints row 0 never hides one; a marker is superseded only by the next sequence, at the profile's `marker_hz` (<= 10). The overlay bytes are `ESC 7`, `ESC [ row+1 ; col+1 H`, the 20 cells, `ESC [ 0 m`, `ESC 8` — inserted before `ESC[?2026l` in a synchronized frame, appended otherwise.

**Decoding** (the Phase 2 raster decoder must agree): classify each pixel's channels as 0 (< ~96) or 1 (> ~160), anything between is impure; segment a pixel row into runs of one pure colour (adjacent cells always differ, so runs ARE cells — drop the impure anti-aliased boundary pixels); require 20 runs of one width (rejects clipped or partly occluded markers); sample run centres. Then check, in this order, reporting the first failure: `wrong_length`, `off_palette`, `adjacent_cells_equal`, `bad_start_sentinel`, `bad_end_sentinel`, `reserved_kind`, `checksum_mismatch`.

**Golden file shape** — `{schema: "termhost-bench/marker-golden/v1", description, codec, valid[], invalid[]}`:

- `codec`: `version`, `cells`, `region`, `echo_region` (`{row, col, cells}`), `max_hz`, `palette` (`[{value, index, rgb: [r,g,b], name}]`), `base`, `virtual_predecessor`, `digit_rule`, `layout` (`[{name, digits, fixed?}]`), `kinds`, `session_limit`, `seq_modulus`, `checksum` (`{modulus, multiplier, rule}`), `decode_order`, `overlay`.
- `valid[]`: `{name, kind, session, seq, digits[20], values[20], indices[20], rgb[20][3], cells_hex, overlay_hex, echo_overlay_hex}` — every one must decode to its (kind, session, seq).
- `invalid[]`: `{name, reason, note, indices[], rgb[][3]}` — each must be rejected with exactly `reason` (one per rejection reason; `rgb` is what an xterm-compatible host renders, so a raster test can draw it).

## The AI-session generator (`generator/ai-session.mjs`)

Every setup's session is the platform's default shell running, with one identical command line (D13):

```
node scripts/termhost-bench/generator/ai-session.mjs --run-dir <dir> --profile <id|path> --seed <0..4294967295> --termhost-bench-generator <run-id>
```

- **Slots.** It claims the first free `<run-dir>/slot-<k>` with an exclusive create; k is its session index (the marker's session). The slot file is one JSON line: `{schema: "termhost-bench/slot/v1", slot, run_id, pid, ppid, host_terminal_id, claimed_at_ns, argv}`; `host_terminal_id` holds only allowlisted variables (`TERMHOST_BENCH_TERMINAL_ID`, `QONTINUI_TERMINAL_ID`, `WT_SESSION`, `TMUX_PANE`, `TERM_PROGRAM`, `TERM_PROGRAM_VERSION`). An adapter that can set `TERMHOST_BENCH_TERMINAL_ID` per session gets an exact slot ↔ session map.
- **Output.** The profile's deterministic byte stream (a pure function of profile, seed and slot — sessions are decorrelated), preceded by `ESC[H ESC[2J` (and the alt screen if the profile asks). Every write is one blocking `fs.writeSync(1, …)` timed on both sides. Falling behind schedule never skips bytes: the same stream is written late.
- **Records** — `<run-dir>/gen-<slot>.jsonl`, one JSON object per line; absolute `process.hrtime` timestamps (`*_ns`: QPC-based on Windows, CLOCK_MONOTONIC on Linux — Phase 2's selftest verifies this shares the probe's timebase) are DECIMAL STRINGS because they exceed 2^53:
  - `start`: `schema` (`termhost-bench/gen/v1`), `run_id`, `session`, `mode`, `profile`, `profile_digest`, `profile_calibrated`, `seed`, `geometry`, `marker_hz`, `echo`, `t0_ns`, `pid`, `ppid`, `terminal_size`, `console`, …
  - `marker`: `{kind, seq, phase, t_sched_ns, t_emit_ns, t_written_ns, bytes}` per new sequence. `t_emit_ns` is taken just before the write (output latency's start); `t_written_ns - t_emit_ns` is time blocked in it; `seq` is the full counter (the marker carries it mod 117649).
  - `echo` (`--echo`): `{seq, byte, t_read_ns, t_emit_ns, t_written_ns, bytes}` per received byte.
  - `progress` every 500 ms: cumulative `bytes_scheduled`, `bytes_written`, `writes`, `blocked_ns`, `echo_bytes`, `sched_pos_us`, `queued` — difference two records to window any interval (`producer_stall_pct` = Δblocked_ns / Δt; `delivered_ratio` = Δbytes_written / Δbytes_scheduled).
  - `exit`: `reason` (`stop_file`, `stdin_eof`, `signal:<name>`, `stdout_closed`, `interrupt`, `duration`, `error`) and the final counters, `queued` and `lag_us`.
- **Stopping.** Exit 0 on the stop file `<run-dir>/stop`, stdin EOF, a signal, the reader closing stdout, Ctrl+C (0x03) in echo mode, or `--duration-ms`; 1 on a runtime error; 2 on a usage error.
- **`--echo`** puts stdin in raw mode and answers each received byte with an echo marker (kind 1, seq = byte index) in row 1, re-drawn by every later write. **`--calibration`** paints a static calibration marker (kind 2, seq 0) and no load.
- **Windows console.** A raw `writeSync` to a console is decoded in the console code page, so the generator switches the output code page to UTF-8 (`chcp 65001`, restored at exit) and lets libuv enable VT processing — otherwise every multi-byte glyph would become several cells and hosts would see different streams.
- **Virtual clock.** `runVirtual({profile, seed, session, durationMs, writeDelayNs, echo, inputs})` runs the real driver on simulated time and returns every write — the determinism, conformance and stall tests use it, and so can display-free analysis.

## Profiles

`termhost-bench/profile/v1` (`profile.mjs` has the schema): a Markov chain over phases — `idle` (markers only), `spinner` (runner#1856 spinner frames), `repaint` (runner#1856 synchronized repaints, fixed `rows` or row counts fitted to a `frame_bytes` distribution), `stream` (prose chunks), `burst` (tool-output chunks) — each with a `duration_ms` quantile table and, except idle, a frame rate. Quantile tables `[[p, value], …]` run p = 0 → 1 and are sampled by linear inverse CDF.

| Profile       | What                                                                | `calibrated`                                                                                                            |
| ------------- | ------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| `claude-tui`  | idle / thinking / stream / repaint / tool burst                     | **false** — runner#1856's 120×40 @ 30 fps for repaint and spinner; every other number is an assumption until calibrated |
| `all-repaint` | every frame a full-screen 120×40 repaint at 30 fps (the worst case) | false (synthetic)                                                                                                       |
| `bulk`        | 16 KiB tool-output chunks at 256/s = 4 MiB/s (throughput)           | false (synthetic)                                                                                                       |

## Calibrating `claude-tui` (`calibrate.mjs`)

On a Linux box with util-linux `script` and `claude` (merytshost has both), record three short, read-only sessions in an empty temp directory, then fit and delete the recordings — they hold code and possibly secrets and are never committed:

```
mkdir -p /tmp/tb-cal && cd /tmp/tb-cal          # empty: nothing to read
for i in 1 2 3; do
  script -q --logging-format advanced --log-timing s$i.tm --log-out s$i.ts \
    -c 'timeout 300 claude --permission-mode plan'
done
node <repo>/scripts/termhost-bench/calibrate.mjs \
  --timing s1.tm --typescript s1.ts --timing s2.tm --typescript s2.ts --timing s3.tm --typescript s3.ts \
  --out <repo>/scripts/termhost-bench/profiles/claude-tui.json
cd / && rm -rf /tmp/tb-cal
```

The advanced format carries the geometry; with the classic format pass `--cols/--rows`. Keep the terminal one size (a mid-recording resize is refused). The fitted profile holds only numbers and fixed vocabulary — `assertNumbersOnly` refuses anything else — and its `expected` block (the recording's own bytes/s, synchronized-byte share, phase shares, frame-size quantiles) is what the generator is held to. Analysis thresholds are `ANALYSIS_DEFAULTS`.

## Interfaces for later phases

- **Phase 2 (`crates/termhost-probe`)** reproduces `marker-golden.json`: decode every `valid` vector, reject every `invalid` one with its `reason`, in `codec.decode_order`; markers sit at `codec.region` / `codec.echo_region` of each pane.
- **Phase 3 (orchestrator)** imports `KPI_CATALOG` / `getKpi` / vocabularies (`kpi-catalog.mjs`), `validateResults` (`validate.mjs`, run before writing any results file), `measureKpi` / `unknownKpi` / `notApplicableKpi` / `degradation` / `slope` (`stats.mjs`), `loadProfile` / `profileDigest` (`profile.mjs`) and `parseOverlays` (`marker.mjs`, display-free checks); it launches the generator with the D13 command line above, maps `slot-<k>` files to host sessions, starts measuring once every `gen-<k>.jsonl` has its first `marker`, and stops with `<run-dir>/stop`.
