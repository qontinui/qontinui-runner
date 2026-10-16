# termhost-bench — the measurement contract

Phase 1 of plan `2026-10-11-terminal-page-performance-kpis-across-setups` (qontinui-dev-notes): the pure, display-free half of a harness that measures how the runner's Terminal page holds up under N concurrent AI sessions against VS Code, Windows Terminal and tmux, with ONE outside-in instrument. This directory defines what is measured (KPI catalog), how results are written and checked (schema + validator), how the instrument reads the screen (marker codec), and the load every host receives (generator + profiles + calibrator). Node built-ins only; no `package.json` change (plan D8).

| File                                              | What it is                                                                                                              |
| ------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| `kpi-catalog.mjs`                                 | The KPI catalog as data, plus the vocabularies (reasons, needs, labels, incidents, environments, setups, arms).         |
| `build-schema.mjs` → `result-schema.json`         | The `termhost-bench/v1` JSON Schema (2020-12), GENERATED from the catalog.                                              |
| `validate.mjs`                                    | A validator for the schema subset used (fails closed on any other keyword) + results checks a schema cannot state. CLI. |
| `stats.mjs`                                       | Nearest-rank percentiles, least-squares slope, degradation ratio; samples → KPI values.                                 |
| `marker.mjs`                                      | The marker codec (v2): (kind, session, seq) ↔ 20 cells, cell-level decoding.                                            |
| `marker-raster.mjs`                               | The exact pixel rules: a captured pixel row → cells. The executable spec Phase 2 ports.                                 |
| `marker-golden.mjs` → `marker-golden.json`        | The golden file: codec description, cell vectors, raster vectors (Phase 2's contract).                                  |
| `profile.mjs`, `profiles/*.json`                  | Load profiles: `claude-tui`, `all-repaint`, `bulk`.                                                                     |
| `generator/ai-session.mjs`                        | The AI-session generator (CLI + driver); `schedule.mjs` and `content.mjs` beside it.                                    |
| `calibrate.mjs`                                   | Fits `claude-tui` from `script --log-timing` recordings; writes numbers only.                                           |
| `../tui-repaint-generator.mjs` (+ `.d.mts`, test) | runner#1856's frame builders, carried byte-identical from `db0fea935` — the one repaint implementation.                 |

Tests: `node --test scripts/termhost-bench/__tests__/*.test.mjs scripts/__tests__/tui-repaint-generator.test.mjs` (bash glob). CI runs exactly that in `ci.yml`'s `test` job (step `Run termhost-bench unit tests`, both matrix legs) and in `.qontinui/ci.toml` (`termhost-bench-node-tests`, files listed; `ci-toml-parity.test.mjs` keeps the two honest). Regenerate generated files after editing their source — the tests fail on drift: `node scripts/termhost-bench/build-schema.mjs --write` (catalog), `node scripts/termhost-bench/marker-golden.mjs --write` (codec), `node scripts/termhost-bench/__tests__/helpers/results.mjs --write` (results fixtures).

## KPI catalog and values

Every KPI value is `{state: measured | unknown | not_applicable, unit, n, value | stats, reason}`:

- **measured** carries a number (`value`, or `stats` keyed by exactly the catalog's statistics), `n >= min_n`, and no `reason`.
- **unknown / not_applicable** carries a `reason` (a `REASON_CODES` key) and NO number — an unmeasured KPI is never rendered as 0. `n` may say how many samples there were.
- Every catalog id appears in every row's `kpis`; none may be omitted.

`min_n` is never below what a statistic needs to mean its name: a nearest-rank p-th percentile needs `ceil(100 / (100 - p))` samples (p95: 20, p99: 100) — below that it IS the maximum. Row fields are documented at the top of `kpi-catalog.mjs`. Semantics the catalog encodes:

- **Attribution.** `host` rows describe the setup's process tree. `cpu_pct` / `memory_mb` EXCLUDE every generator and its parent session shell (slot-file PID → parent PID); that workload is reported as `workload_cpu_pct` / `workload_memory_mb`. The PTY host (`conhost.exe` / `OpenConsole.exe`) stays in the host tree. `shared` rows (`compositor_*` = dwm.exe or the X server, `wsl_vm_*` for tmux-wsl) are attributed to no setup and are `comparable: false`.
- **`marker_loss` vs `marker_skipped`.** Loss: a visible-pane sequence current for >= 2 refresh intervals but never observed, or observed out of order. Skipped (direction `neutral`): superseded before any composited frame carried it — the host coalesced frames, which is not loss.
- **Refresh intervals.** `jank_share`, `marker_loss` and `marker_skipped` need a known refresh rate (`needs: refresh_rate`); without one they are `unknown(refresh_rate_unknown)`.
- **Capture cross-check (`capture_disagreement`, D2).** Windows watches WGC (primary) with DXGI Desktop Duplication beside it; the two are never averaged. Each marker both saw cleanly gives a gap, WGC minus DDA. WGC stamps a later stage than DDA (the capture frame's render, not the desktop presentation), so the declared band is `[0, 10 ms]` (`WGC_RENDER_OFFSET_MS`). The bands are sized in FRAME intervals F — WGC's measured frame-spacing p50, clamped to one to two refresh intervals — because each source sees a marker first in its own next frame. The run DISAGREES — every latency KPI of that watch is `unknown(capture_disagreement)` — when the median gap (of an even count, the lower middle one) lies more than F/4 outside the declared band (a systematic offset: a time-base or unit bug, or either source a whole frame late on every marker wherever 0.75 F exceeds the 10 ms band, i.e. up to 75 Hz), or when, from 20 compared markers, more than 5% lie more than one frame outside it (the primary missing frames, or markers read from the wrong frame). Below 20 markers only the median decides; below 3, neither (`not_performed`). Each marker outside the one-frame band is still a `crosscheck` record (verdict `outside_band`), and the watch summary carries `frame_ms`, `median_gap_ms`, `outside_share`, `tail_rule_applied` and both bands. This replaces "any one marker more than one refresh interval apart voids the run": on the GPU-less hosted VM (CI run 38362046020, 64 Hz, WARP) the compositor presents every other refresh (F = 31.25 ms), so 11–15% of markers were more than one refresh interval apart — WGC later in 359 of those 361, and in all 359 with no WGC frame between the two times — while the median gap was 6.5–8.9 ms and only 0.4–1.3% lay more than one frame outside the declared band. Not caught: a primary missing an occasional frame (under 5% of markers), and at 144 Hz a one-frame lag smaller than the declared offset. Rule: `crates/termhost-probe/src/model/analysis.rs` `crosscheck`.
- **Interval exclusion** (`exclusion: "valid_intervals"`: `hangs`, `marker_loss`, `marker_skipped`): intervals where the window was not foreground, was cloaked or occluded, or real operator input occurred are excluded; too few valid intervals makes the KPI `unknown(occluded)` / `unknown(operator_activity)`.
- **`degradation_*`**: the base KPI's named statistic at N divided by the same setup's value at N = 1 (`degradation_of`); `n` counts measured operands. A ratio of 0 is valid (minimum 0). A not_applicable base makes it not_applicable; an N = 1 row's own degradation is `not_applicable(baseline_level)`.
- **`memory_mb_per_session`** (`needs: n_ladder`) is a slope over the N levels of one file: its `n` counts the distinct N of the rows for the same setup, variant, build role and arm with a measured `memory_mb`, and it is `unknown(too_few_levels)` below three.

`stats.mjs` builds these values: `measureKpi(id, samples | {value, n} | reading)`, `unknownKpi(id, reason)`, `notApplicableKpi(id, reason)`, `degradationKpi(id, kpisAtN, kpisAtOne, {n})` (reads `degradation_of` from the catalog; given `n: 1`, or the same `kpis` twice, it is `not_applicable(baseline_level)`), plus `percentile`, `mean`, `max`, `min`, `share`, `slope`, `degradation`, `summarize` — each returns `{state: "measured", value, n}` or `{state: "unknown" | "not_applicable", reason, n}`, and throws on a non-finite sample.

## A pane that stops repainting (`paneStalls`, `focus.restore_check`)

The window-level hang rule cannot see one pane freezing while the rest of the window keeps painting, and `marker_loss` is unknown without a refresh rate (Xvfb). So `kpis.mjs` `paneStalls` reads each displayed session's output marker in the soak's primary frames:

- **Which frames.** Primary-source frames inside the soak window. Frames inside an interval the probe excluded as `occluded` (not foreground, not visible, cloaked, minimized, occluded) or `operator_activity` (real or foreign input) are dropped; spans are half-open `[start, end)`. `harness_error` intervals (an absent input monitor, the standing state on Xvfb) and missing intervals are not excluded, and being partial does not by itself exclude an interval.
- **Still run.** The marker staying at one sequence. A piece of it ends at a different sequence, where the marker was absent for over `STALL_HIDDEN_GAP_MS` (900 ms, from the first frame lacking it), or at an excluded interval. Consecutive pieces at the same sequence are one run: a frozen pane covered and shown again is still frozen. Its duration is the time it was SEEN — hidden and excluded time are not counted — plus, when its last frame is the last primary frame of the window, the frameless stretch to the window's end outside excluded spans (a frozen screen sends no frames), read against the primary source's `capture_lost` incidents. "capture ended" (`CaptureStatus::Ended`) means capture stopped for good: no frame can follow it, so the tail ends there, and an end placed before the run's last frame leaves no tail. It is read from `stops_run`, which the engine sets on a `capture_lost` exactly when the PRIMARY source ended (only primary losses are read). "capture lost (re-establishing)" (`CaptureStatus::Lost`) is ONE failed read: X11 retries on its next call and capture continues, so the frameless time after it is still evidence; the tail restarts at the latest such loss placed after the run's last frame and up to the cut, and one exactly at the cut or at the window's end leaves no tail (the failed read may have consumed the repaint). WGC is the exception: nothing in `windows/wgc.rs` re-creates its D3D device or frame pool, and a lost device can go silent (no frame, no error, the window still alive), so a WGC loss with no later primary frame cuts like an end — including a WGC loss that cannot be placed in time. An incident is placed by its `t_ns` (the probe clock, taken in the capture thread when the source returned the status); a probe that writes no `t_ns` is placed by its wall-clock `at` through the start record, which can be off (stamped when the engine dequeued it; the wall clock can step). `CAPTURE_LOST_SLACK_MS` (1 s) is applied to such a mapped time when asking whether a frame followed the loss (the frame-gap exclusion and the restore check use it the same way); it is NOT applied to the tail's start, so a mapped failed read placed within the slack before the run's last frame leaves the tail starting at that frame. An end that cannot be placed ends every tail; a failed read that cannot be placed (X11) changes nothing. All primary losses in the window, unplaced ones included, are ONE bounded note on `output_latency_ms`: the count, the first and last, and per effect its count and span (`summarizeLosses`). Not consulted: a Desktop Duplication `ACCESS_LOST` (a desktop switch) — DDA is the cross-check source, never the primary. (An interval's `no_samples` is NOT a capture signal: it counts window-STATE inspector samples, which come from another thread than the frames, and is set on every interval when the inspector did not open.) Markers emitted are counted over the same seen time.

  **WGC blind spots.** The rule sees a WGC device loss only through a `capture_lost`. It cannot see (a) a device that goes silent without ever returning `Lost` — the tail then reads as a frozen screen; (b) a `Recreate` after a resize that fails on every frame — each failure is a `Lost` with no later frame, so tails are cut and a real freeze there goes unseen; (c) frames already buffered in the frame pool and composed before a loss, read after it — they count as later frames and the loss as re-established.
- **Qualifies** at >= `STALL_MIN_MS` (2000 ms) seen and >= `STALL_MIN_EMITTED` (20) output markers emitted meanwhile, and only while `clock.aligned` is OPERATIVE (it compares generator emission times with probe frame times).
- **Hitch vs wedge.** A run is recovered when any later primary frame (excluded or not) shows the marker at a later sequence (mod the counter). A recovered run is a **hitch**: the marker KPIs stay measured, the hitch is a note on `output_latency_ms` and a `pane_stalled` incident. A run whose marker did not advance in any later frame is a **wedge**, of one of two kinds, and the incident says which:
  - **Seen** (`basis: "seen"`): the frames that show the still marker alone meet both bars. `output_latency_ms`, `marker_loss` and `marker_skipped` become `unknown(pane_stalled)` (a KPI already unknown for another reason keeps that reason, with a note), plus a `pane_stalled` incident naming when the marker was last shown.
  - **Tail-dependent** (`basis: "tail"`): it qualifies only with the frameless tail. A host that sends a frame every 2.5 s, or a 2.4 s whole-screen stall that the window's end happens to cut, looks the same as the start of a frozen screen, and mid-window the same gap is nothing. So it voids the marker KPIs only when the usable tail is >= max(`STALL_TAIL_MIN_MS` (10 s), `STALL_TAIL_GAP_FACTOR` (3) × the longest gap between consecutive primary frames earlier in the window — only gaps with no excluded span and no primary capture loss between the two frames count (a loss placed by wall clock counts within 1 s of either frame), so a minimized stretch or a lost capture does not raise the bar) and >= 20 markers were emitted in the tail (`voids: true`). Otherwise it is a `pane_stalled` incident and a note on `output_latency_ms`, and the KPIs stay measured.

  Per session the longest hitch and the wedge (a voiding one first) are both reported.

**Detection limit.** On a host that reads its PTY on the UI thread, a whole-screen freeze blocks the generator too, so fewer than 20 markers are emitted in the tail and the freeze can go undetected.

**The restore check** (focus arm, runner only, gates nothing): after the focus arm, `restore-zone` is called, its `changed` must be `true`, the layout settles 500 ms and `get-zone-view-state` must read `maximizedZone: null`; then a watch of `restoreCheckMs` (5 s) runs the same rule over the sessions the arm owns (not the probe's `sessions_seen`, so a pane that came back blank is judged). The outcome is the row diagnostic `focus.restore_check`:

- `measured`, with `data.outcome` `clean`, `recovered from a hitch` or `not repainting` (a voiding wedge) and a note listing each session's kept frames. A clean verdict needs the tiled V target of sessions judged, **including the session the focus arm had maximized** (the one restore-zone restored; at N >= 10 the V target is 9, fewer than the arm's sessions, so the count alone could pass without it). A session is judged when it is seen in >= 10 kept frames spanning >= 2000 ms, reaching within 2000 ms of the watch's end, showing the marker ADVANCING within the final 2000 ms of those frames (its final still run — the trailing frames across which it never advanced; a backwards step is not an advance — is seen for under 2000 ms outside excluded spans: observed repainting at the end — a pane that froze while its generator was blocked writes too few markers to be a stall, and is not clean either; the reason then says how many markers the generator wrote meanwhile, or that there is no generator log), with no primary capture loss (of either kind) after its last kept frame (the read that could have shown its repaint failed or never came), and not still at the end on a tail-dependent stall. The note summarizes the window's primary losses in one bounded line, and `data.losses` holds their count and per-effect groups. The note marks each session judged or not, and why; when a session not needed for the verdict is unresolved, the outcome reads `clean (k session(s) unresolved: …)` (`data.unresolved`).

  In the restore check a tail-dependent stall can never void: the 5 s watch is under the 10 s tail floor. So only a seen wedge yields `not repainting`; a whole-screen freeze there is a `pane_stalled` incident and leaves the frozen session unjudged.
- `unknown(harness_error)` — the check could not look, which is never a clean result — with a matching `harness_error` incident saying why: an interrupted run, a failed focus arm, an unaligned clock, `changed` not `true`, a zone still maximized or unreadable, a watch with no summary, window or primary source, too few sessions judged, the restored session not judged, or an exception.

## Results files (`termhost-bench/v1`)

One file = one run on one machine in one environment (the report refuses to mix environments, D6): `{schema, run, machine, capabilities, rows[], incidents[]}`; each row is one setup × variant × N × arm × run: `{setup, scenario, kpis, diagnostics, labels, incidents}`. `scenario.profile_calibrated` is on every row. A field that may be unknowable (GPU, display, build, settings digest, `v_actual`, …) holds its value or the honest absence `{state: "unknown" | "not_applicable", reason}` — never `null`. Diagnostics are runner-only and must say `comparable: false` (D1).

Validate with `node scripts/termhost-bench/validate.mjs <file>...` (exit 0 valid, 1 invalid, 2 usage/read error) or `validateResults(doc)`. On top of the schema it rejects percentile disorder, mean outside [min, max], coverage above expectation, `run_index >= run_count`, `v_target`/`v_actual > n`, duplicate rows, a measured `degradation_*` whose own base KPI is not measured, or that is not backed by an N = 1 row of the same setup, variant, build role and arm whose base KPI is measured (or that sits on an N = 1 row itself), and a slope over the N ladder counting more N levels than the file holds for its subject (`subjectKey` exports the grouping).

## Marker codec (v2: `marker.mjs`, `marker-raster.mjs`, `marker-golden.json`)

A marker is 20 solid-background cells (a space under `ESC[48;5;<index>m`) encoding (kind, session, seq).

**Palette** — the 8 corners of the xterm 256-colour cube, every channel 0 or 255 (indices 16-231 are fixed in xterm-compatible hosts; schemes remap only 0-15):

| value  | 0     | 1    | 2     | 3    | 4   | 5       | 6      | 7     |
| ------ | ----- | ---- | ----- | ---- | --- | ------- | ------ | ----- |
| colour | black | blue | green | cyan | red | magenta | yellow | white |
| index  | 16    | 21   | 46    | 51   | 196 | 201     | 226    | 231   |

`value = 4R + 2G + B` with each channel bit = (channel is 255).

**Digits** — 20 base-7 digits, left to right: start `[3, 6]` · kind (1: 0 output, 1 echo, 2 calibration; 3-6 reserved) · session (3, big-endian, 0..342) · seq (6, big-endian, the counter mod 117649) · checksum (5) · end `[5, 1, 3]`. Cell i's value is `(value[i-1] + 1 + digit[i]) mod 8` with `value[-1] = 0`, so adjacent cells always differ and the first two cells are always red, cyan. Checksum: `h = 0; for d in [kind, session digits, seq digits]: h = (h * 31 + d + 1) mod 16787` (the largest prime below 7^5), written as 5 base-7 digits. Every single-cell error is detected (tested exhaustively). Version 2 changed no cell or byte of a marker — it added the pixel rules, raster vectors and an off-palette vector a pixel decoder can reproduce.

**Regions and cadence** — output markers occupy row 0, columns 0-19 (0-based); echo markers row 1, columns 0-19. Every write the generator makes re-draws the current marker(s), so a repaint that paints row 0 never hides one; a marker is superseded only by the next sequence, at the profile's `marker_hz` (<= 10). The overlay bytes are `ESC 7`, `ESC [ row+1 ; col+1 H`, the 20 cells, `ESC [ 0 m`, `ESC 8` — inserted before `ESC[?2026l` in a synchronized frame, appended otherwise. The generator hides the cursor and parks it below row 1, so a block cursor never recolours a marker cell.

**Pixel rules** — `codec.pixel.rules` in the golden file is the authority: nine steps written as an exact algorithm, with the numbers beside them (`channel_low_max` **63**, `channel_high_min` **192**, `width_tolerance` **0.25**, `boundary_gap_max` **0.5**). `marker-raster.mjs` is the reference implementation, and a decoder written from the rule text alone reproduces it on every golden row. In short:

1. The rules decode ONE pixel row; which pixel rows of a captured frame to decode is Phase 2's to define. Positions are pixel edges: pixel i spans [i, i + 1), a segment [start, end); gap(A, B) = B.start − A.end, mid(A, B) = (A.end + B.start) / 2; round is the nearest integer with ties toward +∞ (JS `Math.round`; Rust `f64::round` is the same for x >= 0, and every rounded x is >= 0).
2. A channel <= 63 is 0, >= 192 is 1, anything else makes the pixel impure; a pixel is pure when all three channels are.
3. Segments are maximal runs of one pure value; impure pixels lie in the gaps.
4. The first cell width w runs from b1 = mid(red, cyan) to the middle of the gap after cyan — measured between gap middles because anti-aliasing eats pure pixels from both sides — or to cyan's end when that gap is wide.
5. A candidate starts where a red segment is followed by cyan across a gap < 0.5 × w (nothing after cyan: `wrong_length`).
6. A gap >= 0.5 × w holds max(1, round(gap / w)) IMPURE cells (→ `off_palette`).
7. A segment is k = round(span / w) cells of its value — **a run k cells wide is k equal cells** (→ `adjacent_cells_equal`) — with |span / w − k| <= 0.25, else `irregular_cells`. **The marker is exactly 20 cells**: the segment that reaches cell 19 contributes only the cells still needed, unchecked beyond, so a last cell that merges with same-coloured content (a black or white pane background) still decodes. That bound was chosen over giving cell 19 a fixed colour, because a fixed colour can still equal whatever content sits to its right. After each step w becomes the running mean (right boundary − b1) / (cells − 1), so a fractional width never accumulates error.
8. The 20 cells pass the cell checks in `decode_order`: `wrong_length` (also: the row ended first), `off_palette`, `adjacent_cells_equal`, `bad_start_sentinel`, `bad_end_sentinel`, `reserved_kind`, `checksum_mismatch`.
9. Scanning resumes after the segment that gave an accepted marker's cell 19, or at the segment after a rejected candidate's red one.

Tested: every valid marker decodes at every cell width from 6 to 30 px with 0-2 px blur; a palette pushed into the impure band is rejected, never misdecoded.

**Golden file shape** — `{schema: "termhost-bench/marker-golden/v2", description, codec, valid[], invalid[], rows[]}`:

- `codec`: `version` (2), `cells`, `region`, `echo_region` (`{row, col, cells}`), `max_hz`, `palette` (`[{value, index, rgb: [r,g,b], name}]`), `base`, `virtual_predecessor`, `digit_rule`, `layout` (`[{name, digits, fixed?}]`), `kinds`, `session_limit`, `seq_modulus`, `checksum` (`{modulus, multiplier, digits, rule}`), `decode_order`, `overlay`, `pixel` (`{channel_low_max, channel_high_min, width_tolerance, boundary_gap_max, row_reasons, rules[]}`).
- `valid[]`: `{name, kind, session, seq, digits[20], values[20], indices[20], rgb[20][3], cells_hex, overlay_hex, echo_overlay_hex}` — every one must decode to its (kind, session, seq).
- `invalid[]`: `{name, reason, note, indices[], rgb[][3]}` — each must be rejected with exactly `reason` (one per rejection reason; the off-palette cell is index 88 = rgb(135,0,0)).
- `rows[]`: `{name, note, cell_width_px, blur_px, width_px, rle: [[r,g,b,count], ...], expected: [...]}` — decoding the row by `codec.pixel` must give exactly `expected`, left to right: `{ok: true, kind, session, seq}` or `{ok: false, reason}`; `[]` means no candidate. There are 40. The first 17 are captures a decoder must handle (scaling, blur, colour management, black or white backgrounds, tiled panes, each rejection reason). The other 23 each pin a misreading of one rule, which their note names. Each misreading below was applied to `marker-raster.mjs` and gets at least one row wrong:
  - rules 1-2: 63 or 192 read as impure; 64 read as 0, or 191 as 1; half-even rounding of a run or of an impure gap; floor for an impure gap's cells.
  - rule 4: b1 at red's end instead of the gap middle; b2 at cyan's pure end whatever the gap; `cyan_wide` judged against cyan's pure width, or with `>` instead of `>=`; b2 at the row end when nothing follows cyan.
  - rule 5: the start gap compared with `<=`; the start measured to the gap middle when `cyan_wide`; cyan never opening an impure gap.
  - rule 6: no `max(1, …)`; a gap's cells counted with the w from before the preceding segment; no w update after a gap; a gap that runs to the row end read as `wrong_length`.
  - rule 7: a wide gap judged with `>`; the last segment's right boundary at its own end instead of the row end, or any right boundary at the next segment's start instead of the gap middle; the width check applied to the segment that reaches cell 19; `width_tolerance` exclusive; k = 0 accepted; a fixed w instead of the running mean, or a running mean over cells instead of cells − 1; a segment before a wide gap ending at the gap's middle instead of at its own end.
  - rule 9: resuming at s + 1 after an accepted marker, or, after a rejection, past the cells the candidate read instead of at s + 1.

  A misreading not on this list may still pass every row: the rule text is the authority, and the rows check a decoder against it. Three edits change no result, so no row can pin them: resuming at s + 2 instead of s + 1 after a rejection (segment s + 1 is always cyan); the explicit `wrong_length` for a candidate with nothing after cyan (rule 7 reaches the same result); and skipping the w update after a gap cut short at cell 19 (w is never read again).

## The AI-session generator (`generator/ai-session.mjs`)

Every setup's session is the platform's default shell running one identical command line (D13). Every session runs with `--echo`: only the focused pane ever receives input.

```
node scripts/termhost-bench/generator/ai-session.mjs --run-dir <dir> --profile <id|path> --seed <0..4294967295> --echo --termhost-bench-generator <run-id>
```

- **Slots.** It claims the first free `<run-dir>/slot-<k>` with an exclusive create; k is its session index (the marker's session). The slot file is one JSON line: `{schema: "termhost-bench/slot/v1", slot, run_id, pid, ppid, host_terminal_id, claimed_at_ns, argv}`; `host_terminal_id` holds only allowlisted variables (`TERMHOST_BENCH_TERMINAL_ID`, `QONTINUI_TERMINAL_ID`, `WT_SESSION`, `TMUX_PANE`, `TERM_PROGRAM`, `TERM_PROGRAM_VERSION`). An adapter that can set `TERMHOST_BENCH_TERMINAL_ID` per session gets an exact slot ↔ session map.
- **Output.** The profile's deterministic byte stream (a pure function of the canonical profile, seed and slot — sessions are decorrelated). The first write hides the cursor (`ESC[?25l`), clears, and parks the cursor on row 2; the last write shows it again. Every write is one blocking `fs.writeSync(1, …)` timed on both sides. Falling behind schedule never skips bytes: the same stream is written late.
- **Backlog.** While writes are queued the driver yields to the event loop at least every 50 ms, so stdin, EOF and signals are served. A generator blocked INSIDE one write (its host stopped reading altogether) runs no JavaScript until that write returns — no stop file, EOF or signal handler can end it; only killing its PID (from its slot file) does.
- **Records** — `<run-dir>/gen-<slot>.jsonl`, one JSON object per line; absolute `process.hrtime` timestamps (`*_ns`: QPC-based on Windows, CLOCK_MONOTONIC on Linux — Phase 2's selftest verifies this shares the probe's timebase) are DECIMAL STRINGS because they exceed 2^53:
  - `start`: `schema` (`termhost-bench/gen/v1`), `run_id`, `session`, `mode`, `profile`, `profile_digest`, `profile_calibrated`, `seed`, `geometry`, `marker_hz`, `echo`, `t0_ns`, `pid`, `ppid`, `terminal_size`, `console`, …
  - `marker`: `{kind, seq, phase, t_sched_ns, t_emit_ns, t_written_ns, bytes}` per new sequence. `t_emit_ns` is taken just before the write (output latency's start); `t_written_ns - t_emit_ns` is time blocked in it; `seq` is the full counter (the marker carries it mod 117649).
  - `echo`: `{seq, byte, t_read_ns, t_emit_ns, t_written_ns, bytes}` per received byte; `t_read_ns` is the string `"unknown"` when the read time is not known — never the emit time.
  - `progress` every 500 ms: cumulative `bytes_scheduled`, `bytes_written`, `writes`, `blocked_ns`, `echo_bytes`, `sched_pos_us`, `queued` — difference two records to window any interval (`producer_stall_pct` = Δblocked_ns / Δt; `delivered_ratio` = Δbytes_written / Δbytes_scheduled).
  - `exit`: `reason` (`stop_file`, `stdin_eof`, `signal:<name>`, `stdout_closed`, `interrupt`, `duration`, `error`) and the final counters, `queued` and `lag_us`.
- **Stopping.** Exit 0 on the stop file `<run-dir>/stop`, stdin EOF, a signal, the reader closing stdout, Ctrl+C (0x03, since raw mode disables SIGINT), or `--duration-ms`; 1 on a runtime error; 2 on a usage error.
- **`--echo`** puts stdin in raw mode and answers each received byte with an echo marker (kind 1, seq = byte index) in row 1, re-drawn by every later write. **`--calibration`** paints a static calibration marker (kind 2, seq 0) and no load.
- **Windows console.** A raw `writeSync` to a console is decoded in the console code page, so — once the slot and log exist — the generator switches the output code page to UTF-8 (`chcp 65001`) and lets libuv enable VT processing, restoring the page in a `finally`. When the original page cannot be read it changes nothing.
- **Virtual clock.** `runVirtual({profile, seed, session, durationMs, writeDelayNs, echo, inputs})` runs the real driver on simulated time and returns every write; `inputs` are `{tNs, bytes}`, `{tNs, eof: true}` or `{tNs, signal}`, delivered as a real event loop would — on a sleep at their time, or at the next yield when the driver is busy.

## Profiles

`termhost-bench/profile/v1` (`profile.mjs` has the schema): a Markov chain over phases — `idle` (markers only), `spinner` (runner#1856 spinner frames), `repaint` (runner#1856 synchronized repaints, fixed `rows` or row counts fitted to a `frame_bytes` distribution), `stream` (prose chunks), `burst` (tool-output chunks) — each with a `duration_ms` quantile table and, except idle, a frame rate. Quantile tables `[[p, value], …]` run p = 0 → 1 and are sampled by linear inverse CDF. The digest (`profileDigest`) is SHA-256 over canonical JSON (sorted keys), and the generator walks transitions in sorted key order, so key order never changes a digest or a stream.

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

Both timing formats (classic, advanced) and both typescript layouts (`--log-out`, or `--log-io` with input interleaved) are read; the layout is detected from byte totals and anything that fits neither — or both — is refused. Input bytes are skipped, never analysed. The advanced format carries the geometry; with the classic one pass `--cols/--rows`. Keep the terminal one size (a resize is refused), and a recording with no usable timed output is refused rather than fitted to zeros. The fitted profile holds only numbers and fixed vocabulary — `assertNumbersOnly` refuses anything else — and its `expected` block (the recording's own bytes/s, synchronized-byte share, phase shares, frame-size quantiles) is what the generator is held to. Analysis thresholds are `ANALYSIS_DEFAULTS`.

## Interfaces for later phases

- **Phase 2 (`crates/termhost-probe`)** reproduces `marker-golden.json`: port `codec.pixel` (`marker-raster.mjs` is the reference), decode every `rows[]` row to exactly its `expected` list, decode every `valid` vector and reject every `invalid` one with its `reason`, in `codec.decode_order`. Markers sit at `codec.region` / `codec.echo_region` of each pane.
- **Phase 3 (orchestrator)** imports `KPI_CATALOG` / `getKpi` / vocabularies (`kpi-catalog.mjs`), `validateResults` (`validate.mjs`, run before writing any results file), `measureKpi` / `unknownKpi` / `notApplicableKpi` / `degradationKpi` / `slope` (`stats.mjs`), `loadProfile` / `profileDigest` (`profile.mjs`) and `parseOverlays` (`marker.mjs`, display-free checks); it launches the generator with the D13 command line above (`--echo` included), maps `slot-<k>` files to host sessions, starts measuring once every `gen-<k>.jsonl` has its first `marker`, stops with `<run-dir>/stop`, and kills any generator still alive by its slot-file PID.

## Post-land items (deferred, not missing)

Two Phase 3 outputs can only come from a scheduled run of the landed
`linux-xvfb` job, so they arrive after this code lands rather than with it:

- `scripts/termhost-bench/baselines/linux-xvfb.json`, taken from the first
  scheduled run's results file. It must hold no row labelled `unknown_build`:
  a baseline from a build nobody proved is no baseline (D12), so drop those
  rows (or take a later run) before committing it;
- the `linux-xvfb` report in `qontinui-dev-notes`, rendered from that same run.

Until then there is no Linux baseline to compare against, and `compare` has
nothing to read for that environment.
