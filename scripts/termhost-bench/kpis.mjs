/**
 * termhost-bench KPI computation: the probe's records (one `probe.jsonl` per
 * `watch`, Phase 2's `termhost-probe/v1`) and the generators' records
 * (`gen-<k>.jsonl`, Phase 1's `termhost-bench/gen/v1`) in, one catalog value per
 * KPI out. A statistic below its minimum sample count is `unknown`, never a
 * plausible-looking 0, and every unknown carries its reason.
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 3 ("compute
 * KPIs from probe.jsonl plus the generator logs with stats.mjs"). Pure: no files,
 * clocks or processes — the run loop reads the records and hands them over, and the
 * tests feed fixtures.
 *
 * Who computes what. Every phase is its own `watch` with its own run file, so a
 * watch's `summary` covers exactly that phase's window:
 *   - The capture KPIs of the soak — output_latency_ms, frame_interval_ms,
 *     jank_share, hangs, marker_loss, marker_skipped — are the soak watch's
 *     summary values, taken as they are. They are catalog values already (the
 *     probe's `stats.rs` copies the catalog's units and minimums, and a probe test
 *     fails on drift), and the probe owns the rules behind them. Those rules are
 *     first appearance on the PRIMARY source, the loss-versus-coalescing split,
 *     interval exclusion with amended verdicts, markers before the first frame
 *     left out, a blocked generator pending by its schedule, a hang still open at
 *     the end counted, and a lagging probe or an assumed refresh rate making what
 *     they could distort unknown. Re-deriving those rules here would be a second
 *     implementation drifting from the first. Per-host CPU and GPU come from the
 *     same summary's process groups.
 *   - The harness gates them with what the probe cannot know: whether the arm put
 *     the expected number of sessions on screen (V, D4), whether the arm could be
 *     arranged at all, and the doctor's clock verdict.
 *   - The harness computes what only it can see: keystroke-to-echo latency (the
 *     probe's is not_instrumented; inject.jsonl's t_inject joined with the echo
 *     markers' first appearance), switch and spawn latency (from commands the
 *     harness timed), the generators' own stall and delivery counters, memory, its
 *     growth, crashes and CPU per MB from the samples, and the ladder KPIs
 *     (results.mjs).
 *
 * Phases a row is computed from:
 *   soak    one `watch` over the steady state — capture KPIs and process accounting
 *           (host / workload / compositor groups);
 *   input   a `watch` beside an `inject` — keystroke-to-echo latency (D9);
 *   switch  a `watch` while the adapter shows session after session (focus arm);
 *   spawn   a `watch` while the adapter adds and closes sessions.
 */

import { KPI_CATALOG, REASON_CODES, getKpi, minSamplesForStatistic } from "./kpi-catalog.mjs";
import { SEQ_MODULUS } from "./marker.mjs";
import { measureKpi, notApplicableKpi, slope, unknownKpi } from "./stats.mjs";

const NS_PER_MS = 1_000_000n;
const MIB = 1024 * 1024;

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/**
 * Parse JSON lines. A last line without its newline is a record still being
 * written and is not consumed; any other unparseable line is counted.
 *
 * @param {string} text
 * @returns {{records: object[], malformed: number, partial: boolean}}
 */
export function parseJsonl(text) {
  const records = [];
  let malformed = 0;
  const lines = String(text).split("\n");
  const partial = lines.length > 0 && lines[lines.length - 1] !== "";
  const complete = partial ? lines.slice(0, -1) : lines;
  for (const raw of complete) {
    const line = raw.trim();
    if (!line) continue;
    try {
      records.push(JSON.parse(line));
    } catch {
      malformed += 1;
    }
  }
  return { records, malformed, partial };
}

/** An absolute timestamp (decimal string or number) as BigInt ns, or null. */
export function toNs(v) {
  if (typeof v === "bigint") return v;
  if (typeof v === "string" && /^\d+$/.test(v)) return BigInt(v);
  if (typeof v === "number" && Number.isFinite(v) && v >= 0) return BigInt(Math.round(v));
  return null;
}

/** BigInt ns span as float ms. */
export function nsToMs(ns) {
  return Number(ns) / 1e6;
}

/** The last record per key, in the order keys first appeared. */
function lastBy(records, key) {
  const m = new Map();
  for (const r of records) m.set(key(r), r);
  return [...m.values()];
}

/**
 * Classify one `watch`'s records (the last `start`..`end` span when several are
 * present). Records of another schema are ignored. Late input re-emits an
 * `interval` (and any `hang` it invalidates) with `amended: true`; the LAST record
 * per interval index and per hang start is the verdict, so only that one is kept.
 *
 * @param {object[]} records
 */
export function indexProbeSegment(records) {
  let from = 0;
  records.forEach((r, i) => {
    if (r?.type === "start" && (r.subcommand === "watch" || r.subcommand === undefined)) from = i;
  });
  const seg = {
    start: null,
    captures: [],
    frames: [],
    firstSeen: [],
    intervals: [],
    samples: [],
    hangs: [],
    crosschecks: [],
    incidents: [],
    summary: null,
    end: null,
  };
  for (const r of records.slice(from)) {
    if (r?.schema !== undefined && r.schema !== "termhost-probe/v1") continue;
    switch (r?.type) {
      case "start":
        seg.start = r;
        break;
      case "capture":
        seg.captures.push(r);
        break;
      case "frame":
        seg.frames.push(r);
        break;
      case "first_seen":
        seg.firstSeen.push(r);
        break;
      case "interval":
        seg.intervals.push(r);
        break;
      case "sample":
        seg.samples.push(r);
        break;
      case "hang":
        seg.hangs.push(r);
        break;
      case "crosscheck":
        seg.crosschecks.push(r);
        break;
      case "incident":
        seg.incidents.push(r);
        break;
      case "summary":
        seg.summary = r;
        break;
      case "end":
        seg.end = r;
        break;
      default:
        break;
    }
  }
  seg.intervals = lastBy(seg.intervals, (r) => r.index);
  seg.hangs = lastBy(seg.hangs, (r) => r.t_start_ns);
  return seg;
}

/**
 * A segment's measurement window: the engine's span from the summary, else the
 * start record to the last timestamp seen.
 *
 * @returns {{t0: bigint, t1: bigint} | null}
 */
export function segmentWindow(seg) {
  const t0 = toNs(seg.summary?.t_start_ns) ?? toNs(seg.start?.t0_ns);
  let t1 = toNs(seg.summary?.t_end_ns) ?? toNs(seg.end?.t_ns);
  if (t1 === null) {
    for (const r of [...seg.frames, ...seg.samples, ...seg.intervals]) {
      const t = toNs(r.t_ns ?? r.t_end_ns);
      if (t !== null && (t1 === null || t > t1)) t1 = t;
    }
  }
  if (t0 === null || t1 === null || t1 <= t0) return null;
  return { t0, t1 };
}

/**
 * What the segment's capture looked like: the sources opened, the primary one
 * (only a source that opened and can be primary — Desktop Duplication is the
 * cross-check, never the primary), and whether the refresh rate is KNOWN (the
 * probe marks a rate it assumed with `refresh_assumed`).
 */
export function captureInfo(seg) {
  const opened = seg.captures.filter((c) => c.state === "started").map((c) => c.source);
  const failed = seg.captures
    .filter((c) => c.state === "failed")
    .map((c) => ({ source: c.source, detail: c.detail }));
  const named = seg.summary?.primary ?? opened.find((s) => s !== "dda") ?? null;
  const primary = named !== null && named !== "dda" && opened.includes(named) ? named : null;
  const ref = seg.captures.find((c) => c.state === "started" && c.refresh_hz !== undefined) ?? seg.captures.find((c) => c.refresh_hz !== undefined) ?? null;
  const refreshHz = typeof ref?.refresh_hz === "number" ? ref.refresh_hz : null;
  const refreshSource = typeof ref?.refresh_source === "string" ? ref.refresh_source : null;
  const assumed = ref?.refresh_assumed === true || seg.summary?.refresh_assumed === true;
  const refreshKnown = refreshHz !== null && refreshHz > 0 && !assumed;
  return { opened, failed, primary, refreshHz, refreshSource, refreshKnown };
}

/** Below this many frames with a known lag the probe gives no `behind` verdict (`model::LAG_VERDICT_MIN_FRAMES`). */
export const LAG_VERDICT_MIN_FRAMES = 10;

/**
 * The probe's verdict on its own health over a watch: when it fell behind its
 * source, every first appearance may be late, so latency, loss and cadence it
 * could have distorted are unknown(harness_error) — the probe's rule
 * (`records.rs` `ProbeHealth`), applied here to the KPIs the harness times itself.
 *
 * `behind` is null when frames arrived but fewer than {@link LAG_VERDICT_MIN_FRAMES}
 * of them had a known presentation time (X11's first frame has none, nor does any
 * X11 frame while the server clock is not established): whether the probe kept up
 * was never established, so that is harness_error too — the probe applies the same
 * rule to its own KPIs. A null is never read as false. `lag_statistic` (`p95` or
 * `second_largest`) is null then as well.
 *
 * @returns {{reason: string, detail: string} | null}
 */
export function probeHealthGate(seg) {
  const p = seg?.summary?.probe;
  if (!p) return null;
  if (p.behind === true) {
    return {
      reason: "harness_error",
      detail: `the probe fell behind its capture source (frame lag ${p.lag_ms_judged ?? "?"} ms judged${p.lag_statistic ? ` as the ${p.lag_statistic}` : ""}, bound ${p.lag_bound_ms ?? "?"} ms${Number.isInteger(p.frames_over_bound) ? `, ${p.frames_over_bound} frame(s) over it` : ""}), so first appearances may be late`,
    };
  }
  if (p.behind !== false && Number(p.frames ?? 0) > 0) {
    return {
      reason: "harness_error",
      detail: `the probe could not show it kept up with its capture source: only ${
        Number.isInteger(p.frames_lag_unknown) ? p.frames - p.frames_lag_unknown : "?"
      } of ${p.frames} primary frame(s) had a known presentation time (a verdict needs ${LAG_VERDICT_MIN_FRAMES})`,
    };
  }
  return null;
}

/**
 * Index the generators' records by session (the marker's session id = the slot).
 *
 * @param {Map<number, object[]> | Record<string, object[]>} bySlot slot -> records
 * @returns {Map<number, {slot: number, start: object|null, markers: object[], echoes: object[],
 *   points: {t: bigint, blocked: number, written: number, scheduled: number}[], exit: object|null}>}
 */
export function indexGenerators(bySlot) {
  const entries = bySlot instanceof Map ? [...bySlot.entries()] : Object.entries(bySlot);
  const out = new Map();
  for (const [slotKey, records] of entries) {
    const slot = Number(slotKey);
    const g = { slot, start: null, markers: [], echoes: [], points: [], exit: null };
    for (const r of records) {
      switch (r?.type) {
        case "start": {
          g.start = r;
          const t = toNs(r.t0_ns);
          if (t !== null) g.points.push({ t, blocked: 0, written: 0, scheduled: 0, kind: "start" });
          break;
        }
        case "marker": {
          const t = toNs(r.t_emit_ns);
          if (t !== null) g.markers.push({ kind: r.kind, seq: r.seq, tEmit: t });
          break;
        }
        case "echo": {
          const t = toNs(r.t_emit_ns);
          if (t !== null) g.echoes.push({ seq: r.seq, byte: r.byte, tEmit: t });
          break;
        }
        case "progress":
        case "exit": {
          const t = toNs(r.t_ns);
          if (t !== null && Number.isFinite(r.blocked_ns)) {
            g.points.push({
              t,
              blocked: r.blocked_ns,
              written: r.bytes_written ?? 0,
              scheduled: r.bytes_scheduled ?? 0,
              kind: r.type,
            });
          }
          if (r.type === "exit") g.exit = r;
          break;
        }
        default:
          break;
      }
    }
    g.points.sort((a, b) => (a.t < b.t ? -1 : a.t > b.t ? 1 : 0));
    g.markers.sort((a, b) => (a.tEmit < b.tEmit ? -1 : a.tEmit > b.tEmit ? 1 : 0));
    g.echoes.sort((a, b) => a.seq - b.seq);
    out.set(Number.isInteger(g.start?.session) ? g.start.session : slot, g);
  }
  return out;
}

// ---------------------------------------------------------------------------
// Capture-derived observations
// ---------------------------------------------------------------------------

/** Primary-source frames inside the window, sorted by time. */
export function primaryFrames(seg, primary, win) {
  return seg.frames
    .filter((f) => f.source === primary)
    .map((f) => ({ ...f, t: toNs(f.t_ns) }))
    .filter((f) => f.t !== null && f.t >= win.t0 && f.t <= win.t1)
    .sort((a, b) => (a.t < b.t ? -1 : a.t > b.t ? 1 : 0));
}

/** First appearance (primary source) by `kind:session:seq`. */
export function firstSeenIndex(seg, primary) {
  const idx = new Map();
  for (const fs of seg.firstSeen) {
    if (fs.source !== primary) continue;
    const key = `${fs.kind}:${fs.session}:${fs.seq}`;
    const t = toNs(fs.t_ns);
    if (t !== null && !idx.has(key)) idx.set(key, t);
  }
  return idx;
}

/**
 * Time from a switch command to the first primary frame that shows `session`'s
 * marker (at or after `newestSeq`) as the ONLY session displayed: the frame's
 * accumulated `v` is 1, or, on a record without `v`, no other session's output
 * marker is in it. A frame that shows the session among others — a grid
 * repainted on the way, before the maximize took — is not the switch.
 *
 * @returns {number | null} ms
 */
export function timeToSoleMarker(frames, session, tCmd, newestSeq, timeoutNs) {
  for (const f of frames) {
    if (f.t < tCmd) continue;
    if (f.t > tCmd + timeoutNs) return null;
    const outputs = (f.markers ?? []).filter((m) => m.kind === "output");
    const shown = outputs.some((m) => m.session === session && seqAtOrAfter(m.seq, newestSeq));
    const alone = Number.isInteger(f.v) ? f.v === 1 : outputs.every((m) => m.session === session);
    if (shown && alone) return nsToMs(f.t - tCmd);
  }
  return null;
}

/** Is marker seq `m` (mod M) at or after the full counter `newest`? */
export function seqAtOrAfter(m, newest) {
  if (newest === null || newest === undefined) return true;
  const base = newest % SEQ_MODULUS;
  return (m - base + SEQ_MODULUS) % SEQ_MODULUS < SEQ_MODULUS / 2;
}

/**
 * Time from a command to the first primary frame showing `session`'s output
 * marker at or after `newestSeq` (null = any), within `timeoutNs`. A frame before
 * the command never counts.
 *
 * @returns {number | null} ms
 */
export function timeToMarker(frames, session, tCmd, newestSeq, timeoutNs) {
  for (const f of frames) {
    if (f.t < tCmd) continue;
    if (f.t > tCmd + timeoutNs) return null;
    for (const m of f.markers ?? []) {
      if (m.kind === "output" && m.session === session && seqAtOrAfter(m.seq, newestSeq)) {
        return nsToMs(f.t - tCmd);
      }
    }
  }
  return null;
}

// ---------------------------------------------------------------------------
// Generator-derived observations
// ---------------------------------------------------------------------------

/** A counter's value at `t`, linearly interpolated between the bracketing records. */
export function interpolate(points, t, field) {
  if (points.length === 0) return null;
  let before = null;
  let after = null;
  for (const p of points) {
    if (p.t <= t) before = p;
    if (p.t >= t) {
      after = p;
      break;
    }
  }
  if (!before || !after) return null;
  if (after.t === before.t) return before[field];
  const frac = Number(t - before.t) / Number(after.t - before.t);
  return before[field] + (after[field] - before[field]) * frac;
}

/** The generators' progress cadence (`generator/ai-session.mjs` `PROGRESS_INTERVAL_MS`). */
export const PROGRESS_INTERVAL_MS = 500;

/**
 * A gap between two of a generator's records longer than this means it ran no
 * JavaScript in between. Its loop writes a `progress` record every 500 ms unless
 * a `write` blocks it, and the time spent inside a write is added to `blocked_ns`
 * only when the write RETURNS. A write that never returns, or fails when teardown
 * closes the PTY, leaves no trace in the counters at all (`timedWrite`).
 *
 * The rule is a heuristic and a lower bound on what it detects: only stretches
 * over this threshold are seen at all, and a stretch it does see is attributed to
 * a write the host was not draining although a GC pause or CPU starvation of the
 * generator's process looks the same from its records.
 */
export const RECORD_GAP_MS = 2 * PROGRESS_INTERVAL_MS;

/**
 * Per session over the window: producer stall (share of wall time blocked inside
 * `write`), delivered ratio and bytes written. Each counter is integrated over the
 * record-to-record segments that overlap the window, linearly within a segment.
 *
 * A segment longer than {@link RECORD_GAP_MS} is a stretch in which the generator
 * ran no JavaScript: it was inside a write the host was not draining. That stretch
 * counts as blocked (to within one progress interval) whatever the counters say,
 * because a write that failed at teardown never credits its time to `blocked_ns`.
 *
 * A session with no record at or before the window's start, or none at or after
 * its end, has no reading at all. It is returned in `silent`, and the caller makes
 * the KPIs unknown: dropping it would shrink the sample to the sessions that kept
 * running, which is the opposite of what happened.
 */
export function generatorWindow(gens, win) {
  const stall = [];
  const delivered = [];
  let bytes = 0;
  const notes = [];
  const silent = [];
  const gaps = [];
  let clamped = 0;
  const gapNs = BigInt(RECORD_GAP_MS) * NS_PER_MS;
  const spanNs = Number(win.t1 - win.t0);
  for (const [session, g] of gens) {
    const pts = g.points;
    if (pts.length === 0 || pts[0].t > win.t0 || pts[pts.length - 1].t < win.t1) {
      silent.push(session);
      continue;
    }
    let blocked = 0;
    let written = 0;
    let scheduled = 0;
    let gapInWindow = 0n;
    for (let i = 0; i + 1 < pts.length; i += 1) {
      const a = pts[i];
      const b = pts[i + 1];
      const lo = a.t > win.t0 ? a.t : win.t0;
      const hi = b.t < win.t1 ? b.t : win.t1;
      if (hi <= lo) continue;
      const seg = b.t - a.t;
      const frac = Number(hi - lo) / Number(seg);
      if (seg > gapNs) {
        blocked += Number(hi - lo);
        gapInWindow += hi - lo;
      } else {
        blocked += (b.blocked - a.blocked) * frac;
      }
      written += (b.written - a.written) * frac;
      scheduled += (b.scheduled - a.scheduled) * frac;
    }
    if (gapInWindow > 0n) gaps.push({ session, ms: nsToMs(gapInWindow) });
    let pct = (blocked / spanNs) * 100;
    if (pct > 100) {
      pct = 100;
      clamped += 1;
    }
    stall.push(Math.max(0, pct));
    bytes += Math.max(0, written);
    if (scheduled > 0) delivered.push(written / scheduled);
  }
  if (gaps.length > 0) {
    notes.push(
      `producer_stall_pct: ${gaps.length} session(s) ran no JavaScript for part of the window ` +
        `(${gaps.map((x) => `session ${x.session}: ${Math.round(x.ms)} ms`).join(", ")}): counted as blocked. ` +
        `Only gaps over ${RECORD_GAP_MS} ms are seen, so this is a lower bound on such stretches; each is most likely a write ` +
        "the host was not draining, but a GC pause or CPU starvation of the generator looks the same in its records",
    );
  }
  if (clamped > 0) notes.push(`producer_stall_pct: ${clamped} session stall reading(s) above 100% were clamped (a write straddling a record)`);
  return { stall, delivered, bytes, notes, silent, gaps };
}

// ---------------------------------------------------------------------------
// Process accounting
// ---------------------------------------------------------------------------

/** Values of `field` for process group `group` across a segment's samples. */
export function groupSeries(seg, group, field) {
  const out = [];
  for (const s of seg.samples) {
    const g = s.groups?.[group];
    const v = g?.[field];
    const t = toNs(s.t_ns);
    if (typeof v === "number" && Number.isFinite(v) && t !== null) out.push({ t, v, unreadable: g.unreadable ?? 0 });
  }
  return out;
}

/**
 * Sessions whose generator process the probe saw exit (`sample.exited[]` `session`,
 * named from the generator's slot file), in ascending order.
 */
export function generatorExits(seg) {
  const out = new Set();
  for (const s of seg?.samples ?? []) {
    for (const e of s.exited ?? []) {
      if (Number.isInteger(e?.session)) out.add(e.session);
    }
  }
  return [...out].sort((a, b) => a - b);
}

/** The KPIs read off the soak's markers, which a pane wedged to the end of the window makes unmeasurable. */
export const STALL_VOIDED_KPIS = Object.freeze(["output_latency_ms", "marker_loss", "marker_skipped"]);

/** A stall: a displayed marker that did not advance for at least this long ... */
export const STALL_MIN_MS = 2000;
/** ... while its generator wrote at least this many output markers. */
export const STALL_MIN_EMITTED = 20;
/**
 * A run of a still marker ends where the session's marker was absent from the
 * primary frames for longer than this: frames kept arriving without it (the pane
 * was hidden, scrolled away or covered), so the same sequence shown after the gap
 * is a redisplay of old content, not evidence the pane kept failing to repaint.
 * Fixed rather than one marker period: at `marker_hz` 10 a period is 100 ms, less
 * than a partial (band) read takes to come round a 3x3 grid again, which would
 * split every run. 1 s is under half {@link STALL_MIN_MS}, so a hidden stretch
 * can never be bridged into a stall. Stretches with no frame at all do not end a
 * run: a frozen screen produces none.
 */
export const STALL_HIDDEN_GAP_MS = 1000;
/** Probe interval classes that rule a stretch out (`model/intervals.rs`): not foreground / visible / cloaked / minimized / occluded, or real input. */
export const STALL_EXCLUDING_CLASSES = Object.freeze(["occluded", "operator_activity"]);

/**
 * The spans of the probe's closed intervals whose own verdict excludes them for
 * visibility (not foreground, not visible, cloaked, minimized, occluded) or for
 * operator activity (real or foreign input). This reads the probe's
 * `exclusion_class`, the verdict its hang and marker-loss rules apply through
 * `span_valid`; it does not re-derive it from the counters.
 *
 * @returns {{a: bigint, b: bigint, cls: string}[]}
 */
export function excludedSpans(seg) {
  const out = [];
  for (const r of seg?.intervals ?? []) {
    if (r.valid !== false || !STALL_EXCLUDING_CLASSES.includes(r.exclusion_class)) continue;
    const a = toNs(r.t_start_ns);
    const b = toNs(r.t_end_ns);
    if (a !== null && b !== null) out.push({ a, b, cls: r.exclusion_class });
  }
  return out;
}

/**
 * Panes that did not repaint: a run of primary frames in which a displayed
 * session's output marker stayed at one sequence while its generator kept writing
 * — at least {@link STALL_MIN_MS} between the run's first and last frame, with at
 * least {@link STALL_MIN_EMITTED} output markers emitted in (first, last]. A run
 * does not qualify when it overlaps an interval the probe excluded
 * ({@link excludedSpans}), and it ends where the marker was hidden for longer than
 * {@link STALL_HIDDEN_GAP_MS}.
 *
 * One entry per session:
 * - `recovered: false` — the WEDGE: a qualifying run that reaches the last primary
 *   frame in the window showing that session's marker (frozen for the whole soak,
 *   or from part-way to its end). Its marker KPIs are unknown(pane_stalled):
 *   measured from the live part, they would hide the wedge. Reported even when an
 *   earlier recovered hitch was longer.
 * - `recovered: true` — otherwise the LONGEST qualifying run, after which the
 *   marker advanced again: a hitch. The KPIs stay measured (a hitch is exactly
 *   what they must show); the hitch is reported beside them.
 *
 * Neither the window-level hang rule (other content keeps producing frames) nor
 * marker loss (counted in refresh intervals, unknown on Xvfb) sees a wedge. Found
 * in CI run 38351621760: a tiled soak showed seq 641 for 60 s while the generator
 * wrote 1890 markers.
 *
 * The caller runs this only while clock.aligned is OPERATIVE: it compares the
 * generators' emission times with the probe's frame times.
 *
 * @returns {{session: number, seq: number, ms: number, frames: number, emitted: number, recovered: boolean}[]}
 */
export function paneStalls(seg, gens, win, visible) {
  if (!seg || !win) return [];
  const primary = captureInfo(seg).primary;
  if (!primary) return [];
  const frames = primaryFrames(seg, primary, win);
  const excluded = excludedSpans(seg);
  const gapNs = BigInt(STALL_HIDDEN_GAP_MS) * NS_PER_MS;
  const out = [];
  for (const session of [...visible].sort((a, b) => a - b)) {
    const emits = (gens?.get?.(session)?.markers ?? [])
      .filter((m) => m.kind === "output" && typeof m.tEmit === "bigint")
      .map((m) => m.tEmit);
    // Each frame showing the marker, and whether it follows a hidden stretch.
    const shown = [];
    let missedSince = false;
    for (const f of frames) {
      const m = (f.markers ?? []).find((x) => x.kind === "output" && x.session === session);
      if (!m || !Number.isInteger(m.seq)) {
        missedSince = true;
        continue;
      }
      const prev = shown.at(-1);
      shown.push({ t: f.t, seq: m.seq, afterGap: Boolean(prev) && missedSince && f.t - prev.t > gapNs });
      missedSince = false;
    }
    let longest = null;
    let wedge = null;
    for (let i = 0; i < shown.length; ) {
      let j = i;
      while (j + 1 < shown.length && shown[j + 1].seq === shown[i].seq && !shown[j + 1].afterGap) j += 1;
      const t0 = shown[i].t;
      const t1 = shown[j].t;
      const ms = Number(t1 - t0) / 1e6;
      const emitted = emits.filter((t) => t > t0 && t <= t1).length;
      const clear = !excluded.some((x) => x.a < t1 && x.b > t0);
      if (ms >= STALL_MIN_MS && emitted >= STALL_MIN_EMITTED && clear) {
        const run = { session, seq: shown[i].seq, ms: Math.round(ms), frames: j - i + 1, emitted, recovered: j < shown.length - 1, rawMs: ms };
        if (!run.recovered) wedge = run;
        else if (!longest || ms > longest.rawMs) longest = run;
      }
      i = j + 1;
    }
    const pick = wedge ?? longest;
    if (pick) {
      const { rawMs: _raw, ...rest } = pick;
      out.push(rest);
    }
  }
  return out;
}

/** "session S stayed at seq Q for D ms (F frame(s)) while its generator wrote M marker(s)[, then advanced again]" */
export function describeStall(x) {
  return (
    `session ${x.session} stayed at seq ${x.seq ?? "?"} for ${x.ms} ms (${x.frames} frame(s)) while its generator wrote ${x.emitted} marker(s)` +
    (x.recovered ? ", then advanced again" : "")
  );
}

/** Host processes alive at the first sample that exited during the window. */
export function hostExits(seg) {
  if (seg.samples.length === 0) return { monitored: 0, crashed: [], transient: 0 };
  const first = new Set(seg.samples[0].groups?.host?.pids ?? []);
  const crashed = new Set();
  let transient = 0;
  for (const s of seg.samples) {
    for (const e of s.exited ?? []) {
      if (e.group !== "host") continue;
      if (first.has(e.pid)) crashed.add(e.pid);
      else transient += 1;
    }
  }
  return { monitored: first.size, crashed: [...crashed], transient };
}

// ---------------------------------------------------------------------------
// Row computation
// ---------------------------------------------------------------------------

/** Catalog ids whose instrument needs a capture source. */
export const CAPTURE_KPIS = Object.freeze(
  KPI_CATALOG.filter((k) => k.needs.includes("capture")).map((k) => k.id),
);

/** Every KPI unknown (or not applicable where the catalog says so) for one reason. */
export function allUnknown(reason, { detail, setupId } = {}) {
  const kpis = {};
  for (const k of KPI_CATALOG) {
    if (k.id.startsWith("wsl_vm_") && setupId !== "tmux-wsl") {
      kpis[k.id] = notApplicableKpi(k.id, "not_applicable_to_setup");
    } else {
      kpis[k.id] = unknownKpi(k.id, reason, detail ? { detail } : {});
    }
  }
  return kpis;
}

function stateOf(capabilities, id) {
  return capabilities?.[id]?.state ?? "UNKNOWN";
}

/**
 * unknown(clock_unaligned) for a KPI that compares a time node took (a generator's
 * emission, a harness command) with a time the probe took, unless the doctor
 * showed the two share a timebase; null when the KPI may be computed.
 */
export function clockGate(p, id) {
  if (stateOf(p.capabilities, "clock.aligned") === "OPERATIVE") return null;
  return unknownKpi(id, "clock_unaligned", {
    detail: `clock.aligned is ${stateOf(p.capabilities, "clock.aligned")}: ${p.capabilities?.["clock.aligned"]?.reason ?? "no doctor verdict"}`,
  });
}

/** The soak KPIs the probe computes over its own window (`records.rs` `WatchSummary`). */
export const PROBE_CAPTURE_KPIS = Object.freeze([
  "output_latency_ms",
  "frame_interval_ms",
  "jank_share",
  "hangs",
  "marker_loss",
  "marker_skipped",
]);

/**
 * A KPI value the probe wrote (a summary field, or a process group's), as this
 * catalog's value. The schema's fields are copied, the unit and reason are checked
 * against the catalog, and a measured value under the catalog's minimum becomes
 * unknown(insufficient_samples). Anything the catalog cannot hold is the
 * harness's error, and the value says so; it is never passed on as a number.
 *
 * @param {string} id catalog id
 * @param {object | undefined} v the probe's value
 * @param {{what?: string}} [o] how to name the value in a detail
 */
export function fromProbe(id, v, { what = `the probe's ${id}` } = {}) {
  const k = getKpi(id);
  if (!v || typeof v !== "object" || !["measured", "unknown", "not_applicable"].includes(v.state)) {
    return unknownKpi(id, "harness_error", { detail: `${what} is missing` });
  }
  if (v.unit !== k.unit) {
    return unknownKpi(id, "harness_error", { detail: `${what} is in "${v.unit}", the catalog's ${id} in "${k.unit}"` });
  }
  const n = Number.isInteger(v.n) && v.n >= 0 ? v.n : 0;
  if (v.state !== "measured") {
    if (!Object.hasOwn(REASON_CODES, v.reason)) {
      return unknownKpi(id, "harness_error", { n, detail: `${what} gave the reason "${v.reason}", which the catalog does not define` });
    }
    const detail = v.detail ? String(v.detail) : undefined;
    return v.state === "unknown" ? unknownKpi(id, v.reason, { n, detail }) : notApplicableKpi(id, v.reason, { detail });
  }
  const minN = Math.max(k.min_n, ...k.statistics.map(minSamplesForStatistic));
  if (n < minN) {
    return unknownKpi(id, "insufficient_samples", { n, detail: `${what} was measured over ${n} samples; the catalog needs ${minN}` });
  }
  // Outside the catalog's range the value would make the whole results file
  // invalid. The detail names the bound, never the number (it can reach a public
  // log, and a third-party reading never leaves the job: D7).
  const range = k.value_range ?? {};
  const outOfRange = (x) => (range.minimum !== undefined && x < range.minimum) || (range.maximum !== undefined && x > range.maximum);
  const rangeText = `[${range.minimum ?? "-inf"}, ${range.maximum ?? "+inf"}]`;
  if (k.shape === "value") {
    if (typeof v.value !== "number" || !Number.isFinite(v.value)) {
      return unknownKpi(id, "harness_error", { n, detail: `${what} is measured without a finite value` });
    }
    if (outOfRange(v.value)) return unknownKpi(id, "harness_error", { n, detail: `${what} lies outside the catalog's range ${rangeText}` });
    if (k.integer && !Number.isInteger(v.value)) return unknownKpi(id, "harness_error", { n, detail: `${what} is a count, but not an integer` });
    return measureKpi(id, { value: v.value, n });
  }
  const stats = {};
  for (const s of k.statistics) {
    const x = v.stats?.[s];
    if (typeof x !== "number" || !Number.isFinite(x)) {
      return unknownKpi(id, "harness_error", { n, detail: `${what} is measured without its ${s}` });
    }
    if (outOfRange(x)) return unknownKpi(id, "harness_error", { n, detail: `${what}'s ${s} lies outside the catalog's range ${rangeText}` });
    stats[s] = x;
  }
  return { state: "measured", unit: k.unit, n, stats };
}

/**
 * Compute one row's KPIs.
 *
 * @param {{
 *   environment: string, setupId: string, n: number, arm: "focus" | "tiled", vTarget: number,
 *   capabilities: Record<string, {state: string, reason: string}>,
 *   gens: ReturnType<typeof indexGenerators>,
 *   soak: {records: object[]} | null,
 *   armFailure?: {reason: string, detail: string} | null,
 *   input?: {records?: object[], injects?: object[], focusSession?: number, echoBase?: number,
 *     unavailable?: {reason: string, detail?: string}} | null,
 *   switches?: {records?: object[], list?: {k: number, tCmdNs: bigint}[],
 *     unavailable?: {reason: string, detail?: string, notApplicable?: boolean}} | null,
 *   spawns?: {records?: object[], list?: {session: number|null, tCmdNs: bigint}[],
 *     unavailable?: {reason: string, detail?: string}} | null,
 *   switchTimeoutMs?: number, spawnTimeoutMs?: number,
 * }} p
 * @returns {{kpis: Record<string, object>, vActual: number|null, notes: string[], sessionsSeen: number[]}}
 */
export function computeRowKpis(p) {
  const notes = [];
  const kpis = {};
  const set = (id, value) => {
    kpis[id] = value;
  };
  const seg = p.soak ? indexProbeSegment(p.soak.records) : null;
  const win = seg ? segmentWindow(seg) : null;
  const cap = seg ? captureInfo(seg) : null;

  // --- the capture gate --------------------------------------------------
  // The soak watch's summary is the probe's verdict over exactly this window.
  // The harness decides only whether the arm put the right sessions on screen
  // (V, D4), whether the arm could be arranged, and whether the clock holds.
  const summary = seg?.summary ?? null;
  const va = Number.isInteger(summary?.v_actual_p50) ? summary.v_actual_p50 : null;
  const visible = new Set(Array.isArray(summary?.sessions_seen) ? summary.sessions_seen.filter(Number.isInteger) : []);
  const primaryFrameCount = cap?.primary ? Number(summary?.frames?.[cap.primary] ?? 0) : 0;
  let captureGate = null;
  if (!seg || !win || !summary) {
    captureGate = {
      reason: "harness_error",
      detail: seg?.end
        ? `the soak watch ended (${seg.end.reason ?? "?"}, exit ${seg.end.exit_code ?? "?"}) without writing its summary`
        : "no complete soak watch segment was recorded",
    };
  } else if (!cap.primary) {
    captureGate = {
      reason: "capture_unavailable",
      detail:
        cap.failed.length > 0
          ? `no primary capture source opened: ${cap.failed.map((f) => `${f.source}: ${f.detail}`).join("; ")}`
          : "no primary capture source opened",
    };
  } else if (p.armFailure) {
    captureGate = p.armFailure;
  } else if (primaryFrameCount === 0 || va === null) {
    captureGate = { reason: "sessions_not_rendered", detail: "the window produced no captured frame in the soak" };
  } else if (p.arm === "focus" ? va !== 1 : va < p.vTarget) {
    captureGate = {
      reason: p.arm === "focus" && p.setupId === "runner" && va > 1 ? "zone_unresolved" : "sessions_not_rendered",
      detail: `V measured ${va} (the median number of sessions displayed), the ${p.arm} arm needs ${p.arm === "focus" ? "exactly 1" : `>= ${p.vTarget}`}; sessions seen: ${[...visible].sort((a, b) => a - b).join(",") || "none"}`,
    };
  }
  const gated = (id) => captureGate && CAPTURE_KPIS.includes(id);
  const gateValue = (id) => unknownKpi(id, captureGate.reason, { detail: captureGate.detail });

  // --- the probe's capture KPIs -------------------------------------------
  for (const id of PROBE_CAPTURE_KPIS) {
    if (gated(id)) {
      set(id, gateValue(id));
    } else if (id === "output_latency_ms" && clockGate(p, id)) {
      // A generator's emission (node hrtime) against a first appearance (probe).
      set(id, clockGate(p, id));
    } else {
      set(id, fromProbe(id, summary[id], { what: `the soak watch's ${id}` }));
    }
  }
  // Panes that did not repaint (paneStalls). The rule compares the generators'
  // emission times with the probe's frame times, so it runs only on an aligned
  // clock (and output_latency_ms already reads clock_unaligned otherwise).
  const stalls = captureGate || clockGate(p, "output_latency_ms") ? [] : paneStalls(seg, p.gens, win, visible);
  // A wedge (frozen to the end of the soak) leaves the per-marker KPIs measured
  // from the live part only: they become unknown(pane_stalled). A reason the KPI
  // already had is kept — only a bare sample shortfall, which the wedge explains,
  // is replaced — and the wedge is noted on it instead.
  const wedged = stalls.filter((x) => !x.recovered);
  if (wedged.length > 0) {
    const what = wedged.map(describeStall).join("; ");
    notes.push(`pane stalled: ${what}`);
    for (const id of STALL_VOIDED_KPIS) {
      const cur = kpis[id];
      if (cur.state === "measured" || cur.reason === "insufficient_samples") {
        set(id, unknownKpi(id, "pane_stalled", { n: cur.n ?? 0, detail: `the pane stopped repainting: ${what}` }));
      } else {
        notes.push(`${id}: the pane stopped repainting: ${what}`);
      }
    }
  }
  // A hitch the pane recovered from is what these KPIs exist to show: they stay
  // measured, and the hitch is named beside them.
  const hitches = stalls.filter((x) => x.recovered);
  if (hitches.length > 0) {
    notes.push(`output_latency_ms: pane hitch, recovered (the marker KPIs stay measured): ${hitches.map(describeStall).join("; ")}`);
  }
  if (kpis.output_latency_ms.state === "measured") {
    // Which displayed sessions contributed: a session whose output marker never
    // first-appeared in the window gave no latency sample.
    const observed = new Set(
      seg.firstSeen
        .filter((fs) => fs.source === cap.primary && fs.kind === "output" && visible.has(fs.session))
        .filter((fs) => {
          const t = toNs(fs.t_ns);
          return t !== null && t >= win.t0 && t <= win.t1;
        })
        .map((fs) => fs.session),
    ).size;
    kpis.output_latency_ms.coverage = { observed, expected: Math.max(p.vTarget, visible.size, observed) };
  }
  if (Number(summary?.negative_latencies) > 0) {
    notes.push(`output_latency_ms: ${summary.negative_latencies} first appearance(s) before their emission excluded by the probe`);
  }
  if (kpis.hangs.state === "measured" && Number(summary?.hang_episodes_excluded) > 0) {
    notes.push(`hangs: ${summary.hang_episodes_excluded} hang episode(s) excluded: they touched an invalid interval`);
  }

  // --- process accounting -------------------------------------------------
  computeProcessKpis(p, seg, win, set, notes);

  // --- generator self-timing -----------------------------------------------
  if (!win) {
    set("producer_stall_pct", unknownKpi("producer_stall_pct", "harness_error", { detail: "no soak window" }));
    set("delivered_ratio", unknownKpi("delivered_ratio", "harness_error", { detail: "no soak window" }));
  } else {
    const gw = generatorWindow(p.gens, win);
    notes.push(...gw.notes);
    const coverage = { observed: gw.stall.length, expected: Math.max(p.n, gw.stall.length) };
    if (gw.silent.length > 0) {
      const died = generatorExits(seg).filter((k) => gw.silent.includes(k));
      const detail =
        `session(s) ${gw.silent.join(", ")} wrote no generator record bracketing the soak window ` +
        "(blocked inside one write, or gone): a silent session makes the reading unknown, never a smaller sample" +
        (died.length > 0 ? `; the probe saw the generator process of session(s) ${died.join(", ")} exit (a workload death, not a host hang)` : "");
      set("producer_stall_pct", unknownKpi("producer_stall_pct", "harness_error", { n: gw.stall.length, detail }));
      set("delivered_ratio", unknownKpi("delivered_ratio", "harness_error", { n: gw.delivered.length, detail }));
    } else {
      set("producer_stall_pct", measureKpi("producer_stall_pct", gw.stall, { coverage }));
      set(
        "delivered_ratio",
        gw.delivered.length > 0
          ? measureKpi("delivered_ratio", { value: Math.min(...gw.delivered), n: gw.delivered.length }, {
              coverage: { observed: gw.delivered.length, expected: Math.max(p.n, gw.delivered.length) },
            })
          : unknownKpi("delivered_ratio", "insufficient_samples", { n: 0, detail: "no session scheduled bytes in the window" }),
      );
    }
    // cpu_ms_per_mb needs the window's bytes; computed here once both are known.
    // It is a SUM of the host's CPU time over the window's sample intervals (the
    // first sample has no interval: dt_ms null), so one withheld sample — no live
    // process, or a member unreadable — leaves a sum that is not the host's.
    const intervalsSampled = seg ? seg.samples.filter((s) => s.dt_ms !== null && s.dt_ms !== undefined && s.groups?.host) : [];
    const cpuMs = intervalsSampled.map((s) => s.groups.host.cpu_ms);
    const withheld = cpuMs.filter((v) => !(typeof v === "number" && Number.isFinite(v))).length;
    const mb = gw.bytes / MIB;
    if (cpuMs.length < getKpi("cpu_ms_per_mb").min_n) {
      set("cpu_ms_per_mb", unknownKpi("cpu_ms_per_mb", "insufficient_samples", { n: cpuMs.length }));
    } else if (withheld > 0) {
      set(
        "cpu_ms_per_mb",
        unknownKpi("cpu_ms_per_mb", "harness_error", {
          n: cpuMs.length - withheld,
          detail: `${withheld} of ${cpuMs.length} host samples withheld their CPU time (no live process or an unreadable member), so a sum over the rest is not the host's`,
        }),
      );
    } else if (!(mb > 0)) {
      set("cpu_ms_per_mb", unknownKpi("cpu_ms_per_mb", "degenerate_input", { n: cpuMs.length, detail: "the generators wrote no bytes in the window" }));
    } else {
      const total = cpuMs.reduce((a, v) => a + v, 0);
      set("cpu_ms_per_mb", measureKpi("cpu_ms_per_mb", { value: total / mb, n: cpuMs.length }));
    }
  }

  // --- input, switch and spawn phases -------------------------------------
  set("input_latency_ms", inputLatencyKpi(p, captureGate));
  set("switch_latency_ms", switchLatencyKpi(p, notes));
  set("spawn_latency_ms", spawnLatencyKpi(p, notes));

  // --- shared and not-applicable ------------------------------------------
  for (const id of ["wsl_vm_cpu_pct", "wsl_vm_memory_mb"]) {
    set(
      id,
      p.setupId === "tmux-wsl"
        ? unknownKpi(id, "not_instrumented", { detail: "the WSL VM sampler arrives with the tmux-wsl adapter (Phase 5)" })
        : notApplicableKpi(id, "not_applicable_to_setup"),
    );
  }
  // Document-level KPIs (filled by results.mjs once every row exists).
  for (const k of KPI_CATALOG) {
    if (k.degradation_of) set(k.id, unknownKpi(k.id, "no_baseline_level"));
    if (k.needs.includes("n_ladder")) set(k.id, unknownKpi(k.id, "too_few_levels", { n: 0 }));
  }

  // Every catalog id present, in catalog order; each note on the KPI it names.
  const ordered = {};
  for (const k of KPI_CATALOG) {
    if (!kpis[k.id]) throw new Error(`computeRowKpis left ${k.id} unset`);
    ordered[k.id] = kpis[k.id];
  }
  const unattached = attachNotes(ordered, notes);
  return { kpis: ordered, vActual: va, notes: unattached, sessionsSeen: [...visible].sort((a, b) => a - b), stalls };
}

/**
 * Attach every `"<kpi_id>: text"` note to that KPI's `notes`; return the rest.
 *
 * @param {Record<string, object>} kpis
 * @param {string[]} notes
 */
export function attachNotes(kpis, notes) {
  const rest = [];
  for (const note of notes) {
    const m = /^([a-z][a-z0-9_]*): (.+)$/s.exec(note);
    if (m && kpis[m[1]]) kpis[m[1]].notes = [...(kpis[m[1]].notes ?? []), m[2]];
    else rest.push(note);
  }
  return rest;
}

/**
 * Why a process group's values were withheld in some samples: the probe reports
 * `null` with a note (`no_live_process`, `unreadable_members`) rather than a
 * partial sum (Phase 2 review M2).
 */
function withheldCounts(seg, group) {
  let present = 0;
  let empty = 0;
  let unreadable = 0;
  for (const s of seg.samples) {
    const g = s.groups?.[group];
    if (!g) continue;
    present += 1;
    if (g.note === "no_live_process") empty += 1;
    else if (g.note === "unreadable_members") unreadable += 1;
  }
  return { present, empty, unreadable };
}

function computeProcessKpis(p, seg, win, set, notes) {
  const noSeg = !seg || !win;
  const procs = seg?.summary?.processes ?? null;

  /** A group's CPU: the probe's own mean/p95 over the samples it could read whole. */
  const groupCpu = (id, group) => {
    if (noSeg) return unknownKpi(id, "harness_error", { detail: "no soak watch segment" });
    const ps = procs?.[group];
    if (!ps) return unknownKpi(id, "harness_error", { detail: `the soak watch's summary has no ${group} process group` });
    return fromProbe(id, ps.cpu_pct, { what: `the probe's ${group} cpu_pct` });
  };

  /**
   * A group's memory at the end of the window: the probe's `memory_mb_end`, the
   * group's LAST sample, null unless that sample was whole. An earlier readable
   * sample is not an end value — a host that crashed, or a launcher that handed
   * off and exited, has none — so a null is unknown, never the last good reading.
   */
  const groupMemory = (id, group) => {
    if (noSeg) return unknownKpi(id, "harness_error", { detail: "no soak watch segment" });
    const series = groupSeries(seg, group, "memory_mb");
    const w = withheldCounts(seg, group);
    if (w.present === 0) return unknownKpi(id, seg.samples.length === 0 ? "insufficient_samples" : "harness_error", { n: 0, detail: `no sample carried a ${group} group` });
    const ps = procs?.[group];
    if (!ps) return unknownKpi(id, "harness_error", { n: series.length, detail: `the soak watch's summary has no ${group} process group` });
    const end = ps.memory_mb_end;
    if (typeof end !== "number" || !Number.isFinite(end)) {
      return unknownKpi(id, "harness_error", {
        n: series.length,
        detail: `the last ${group} sample was not readable whole, so there is no end-of-window value (${w.empty} sample(s) with no live process, ${w.unreadable} with unreadable members)`,
      });
    }
    const extra = w.empty + w.unreadable > 0 ? { notes: [`${w.empty + w.unreadable} ${group} sample(s) withheld (${w.empty} with no live process, ${w.unreadable} with unreadable members)`] } : {};
    return measureKpi(id, { value: end, n: Math.max(1, series.length) }, extra);
  };

  const hostPresent = !noSeg && seg.samples.some((s) => s.groups?.host);
  const hostPids = noSeg ? 0 : Math.max(0, ...seg.samples.map((s) => s.groups?.host?.pids?.length ?? 0));
  if (!noSeg && seg.samples.length > 0 && (!hostPresent || hostPids === 0)) {
    const detail = hostPresent
      ? "the host group had no live process in any sample: the probe was given no live root"
      : "the probe sampled no host group: it was given no live host root (--host-pid)";
    for (const id of ["cpu_pct", "memory_mb", "memory_growth_mb_per_min", "crashes", "gpu_pct"]) {
      set(id, unknownKpi(id, "harness_error", { detail }));
    }
  } else {
    set("cpu_pct", groupCpu("cpu_pct", "host"));
    set("memory_mb", groupMemory("memory_mb", "host"));
    const mem = noSeg ? [] : groupSeries(seg, "host", "memory_mb");
    if (mem.length > 0 && win) {
      const xs = mem.map((m) => Number(m.t - win.t0) / 60e9);
      set("memory_growth_mb_per_min", measureKpi("memory_growth_mb_per_min", slope(xs, mem.map((m) => m.v), { minN: 20 })));
    } else {
      set("memory_growth_mb_per_min", unknownKpi("memory_growth_mb_per_min", "insufficient_samples", { n: 0 }));
    }
    const exits = noSeg ? { monitored: 0, crashed: [], transient: 0 } : hostExits(seg);
    if (exits.monitored === 0) set("crashes", unknownKpi("crashes", "insufficient_samples", { n: 0 }));
    else {
      set("crashes", measureKpi("crashes", { value: exits.crashed.length, n: exits.monitored }));
      if (exits.transient > 0) notes.push(`crashes: ${exits.transient} short-lived host process exit(s) not counted (spawned and exited inside the window)`);
    }
    const gpuStatus = noSeg ? [] : seg.samples.map((s) => s.gpu).filter((g) => typeof g === "string");
    if (gpuStatus.length > 0 && gpuStatus.every((g) => g !== "measured")) {
      set(
        "gpu_pct",
        p.environment === "linux-xvfb"
          ? notApplicableKpi("gpu_pct", "not_applicable_to_environment", { detail: `no per-process GPU engine counters on Linux (${gpuStatus[0]})` })
          : unknownKpi("gpu_pct", "gpu_counters_unavailable", { detail: gpuStatus[0] }),
      );
    } else if (noSeg || !procs?.host) {
      set("gpu_pct", unknownKpi("gpu_pct", "harness_error", { detail: "the soak watch's summary has no host process group" }));
    } else {
      set("gpu_pct", fromProbe("gpu_pct", procs.host.gpu_pct, { what: "the probe's host gpu_pct" }));
    }
  }
  set("workload_cpu_pct", groupCpu("workload_cpu_pct", "workload"));
  set("workload_memory_mb", groupMemory("workload_memory_mb", "workload"));
  set("compositor_cpu_pct", groupCpu("compositor_cpu_pct", "compositor"));
  set("compositor_memory_mb", groupMemory("compositor_memory_mb", "compositor"));
}

/**
 * The catalog reason an `inject_abort` stands for (`inject.rs` `AbortReason`).
 * Real input or another injector is the operator's activity; a key the platform
 * cannot type, a send that failed or a target check that errored is the
 * harness's; everything else — no foreground, no desktop, a listener that is down
 * or deaf, a foreground that changed mid-arm, a held modifier or CapsLock
 * (`keyboard_state`) — means the D9 safeguard could not be established.
 */
export const INJECT_ABORT_REASONS = Object.freeze({
  // Real input, or someone else's injection.
  operator_input: "operator_activity",
  foreign_injection: "operator_activity",
  // The harness's own failure.
  unsupported_key: "harness_error",
  send_failed: "harness_error",
  target_check_failed: "harness_error",
  stop_requested: "harness_error",
  // The D9 safeguard refused (or could not establish) the conditions to type.
  not_foreground: "input_unsafe",
  desktop_unavailable: "input_unsafe",
  monitor_down: "input_unsafe",
  monitor_deaf: "input_unsafe",
  foreground_changed_during_injection: "input_unsafe",
  // A modifier held, or CapsLock on: a key would not type what is meant, or would
  // become a shortcut, so the arm stops before it (Phase 2's keyboard safety).
  keyboard_state: "input_unsafe",
});

/**
 * The catalog reason for an `inject_abort` record's reason. Every reason the probe
 * writes (`inject.rs` `AbortReason::as_str`) is listed in INJECT_ABORT_REASONS (a
 * test reads the Rust source to keep the two equal); a reason the table does not
 * know is input_unsafe, the conservative reading.
 */
export function injectAbortReason(reason) {
  return Object.hasOwn(INJECT_ABORT_REASONS, reason) ? INJECT_ABORT_REASONS[reason] : "input_unsafe";
}

/**
 * Keystroke-to-echo latency from one input phase: key i of the `inject` arm is the
 * focused generator's echo with sequence `echoBase + i` (its echo counter is
 * cumulative over its life), checked byte for byte; the echo marker's first
 * appearance in the input phase's watch minus the key's `t_inject`. An arm that
 * aborted part-way keeps the keys sent before the abort; when they are too few,
 * the abort, not the count, is the reason.
 */
export function inputLatencyKpi(p, captureGate) {
  const id = "input_latency_ms";
  const inp = p.input;
  if (!inp) return unknownKpi(id, "input_unsafe", { detail: "the input arm did not run" });
  if (inp.unavailable) return unknownKpi(id, inp.unavailable.reason, inp.unavailable.detail ? { detail: inp.unavailable.detail } : {});
  if (captureGate) return unknownKpi(id, captureGate.reason, { detail: captureGate.detail });
  // No clock gate: `t_inject_ns` (inject) and the echo's first appearance (watch)
  // are both the probe's clock, so node's alignment with it does not enter.
  const seg = indexProbeSegment(inp.records ?? []);
  const cap = captureInfo(seg);
  if (!cap.primary) return unknownKpi(id, "capture_unavailable", { detail: "the input phase's watch opened no capture source" });
  const health = probeHealthGate(seg);
  if (health) return unknownKpi(id, health.reason, { detail: health.detail });
  if (seg.summary?.crosscheck?.verdict === "disagree") {
    return unknownKpi(id, "capture_disagreement", { detail: seg.summary.crosscheck.detail });
  }
  const g = p.gens.get(inp.focusSession);
  if (!g) return unknownKpi(id, "harness_error", { detail: `no generator records for focused session ${inp.focusSession}` });
  const seen = firstSeenIndex(seg, cap.primary);
  const bySeq = new Map(g.echoes.map((e) => [e.seq, e]));
  const samples = [];
  let unaligned = 0;
  let unseen = 0;
  let negative = 0;
  for (const k of inp.injects ?? []) {
    const e = bySeq.get((inp.echoBase ?? 0) + k.index);
    const tInject = toNs(k.t_inject_ns);
    if (!e || tInject === null || typeof k.key !== "string" || e.byte !== k.key.charCodeAt(0)) {
      unaligned += 1;
      continue;
    }
    const t = seen.get(`echo:${inp.focusSession}:${e.seq % SEQ_MODULUS}`);
    if (t === undefined) {
      unseen += 1;
      continue;
    }
    if (t < tInject) {
      negative += 1;
      continue;
    }
    samples.push(nsToMs(t - tInject));
  }
  const notes = [];
  if (unaligned > 0) notes.push(`${unaligned} key(s) had no matching echo (byte or sequence mismatch)`);
  if (unseen > 0) notes.push(`${unseen} echo marker(s) never appeared on the primary source`);
  if (negative > 0) notes.push(`${negative} echo(es) appeared before their key: excluded`);
  if (inp.abort) notes.push(`the arm aborted at key ${inp.abort.index} (${inp.abort.reason}): ${inp.abort.detail ?? ""}`.trim());
  const value = measureKpi(id, samples, notes.length ? { notes } : {});
  if (value.state !== "measured" && inp.abort) {
    return unknownKpi(id, injectAbortReason(inp.abort.reason), {
      n: samples.length,
      detail: `the arm aborted at key ${inp.abort.index} (${inp.abort.reason}) after ${samples.length} usable key(s): ${inp.abort.detail ?? ""}`.trim(),
    });
  }
  return value;
}

/** Switch latency over the switch phase (focus arm, N > 1). */
export function switchLatencyKpi(p, notes) {
  const id = "switch_latency_ms";
  const sw = p.switches;
  if (p.arm === "tiled") return notApplicableKpi(id, "not_applicable_to_arm");
  if (p.n === 1) return notApplicableKpi(id, "single_session");
  if (!sw) return unknownKpi(id, "insufficient_samples", { n: 0, detail: "the switch phase did not run" });
  if (sw.unavailable) return unknownKpi(id, sw.unavailable.reason, sw.unavailable.detail ? { detail: sw.unavailable.detail } : {});
  // The command time is the harness's (node hrtime) and the frame time the
  // probe's: comparing them needs the doctor's clock verdict.
  const clock = clockGate(p, id);
  if (clock) return clock;
  const seg = indexProbeSegment(sw.records ?? []);
  const win = segmentWindow(seg);
  const cap = captureInfo(seg);
  if (!cap.primary || !win) return unknownKpi(id, "capture_unavailable", { detail: "the switch phase's watch captured nothing" });
  const health = probeHealthGate(seg);
  if (health) return unknownKpi(id, health.reason, { detail: health.detail });
  const frames = primaryFrames(seg, cap.primary, win);
  const timeoutNs = BigInt(Math.round((p.switchTimeoutMs ?? 5000) * 1e6));
  const samples = [];
  let missed = 0;
  for (const s of sw.list ?? []) {
    const g = p.gens.get(s.k);
    let newest = null;
    for (const m of g?.markers ?? []) if (m.kind === "output" && m.tEmit <= s.tCmdNs) newest = m.seq;
    const ms = timeToSoleMarker(frames, s.k, s.tCmdNs, newest, timeoutNs);
    if (ms === null) missed += 1;
    else samples.push(ms);
  }
  if (missed > 0) notes.push(`switch_latency_ms: ${missed} switch(es) never showed the session alone within the timeout`);
  return measureKpi(id, samples);
}

/** Spawn latency over the spawn phase: command to the new session's first marker. */
export function spawnLatencyKpi(p, notes) {
  const id = "spawn_latency_ms";
  const sp = p.spawns;
  if (!sp) return unknownKpi(id, "insufficient_samples", { n: 0, detail: "the spawn phase did not run" });
  if (sp.unavailable) return unknownKpi(id, sp.unavailable.reason, sp.unavailable.detail ? { detail: sp.unavailable.detail } : {});
  const clock = clockGate(p, id);
  if (clock) return clock;
  const seg = indexProbeSegment(sp.records ?? []);
  const win = segmentWindow(seg);
  const cap = captureInfo(seg);
  if (!cap.primary || !win) return unknownKpi(id, "capture_unavailable", { detail: "the spawn phase's watch captured nothing" });
  const health = probeHealthGate(seg);
  if (health) return unknownKpi(id, health.reason, { detail: health.detail });
  const frames = primaryFrames(seg, cap.primary, win);
  const timeoutNs = BigInt(Math.round((p.spawnTimeoutMs ?? 30000) * 1e6));
  const samples = [];
  let unmapped = 0;
  let unseen = 0;
  for (const s of sp.list ?? []) {
    if (!Number.isInteger(s.session)) {
      unmapped += 1;
      continue;
    }
    const ms = timeToMarker(frames, s.session, s.tCmdNs, null, timeoutNs);
    if (ms === null) unseen += 1;
    else samples.push(ms);
  }
  if (unmapped > 0) notes.push(`spawn_latency_ms: ${unmapped} spawned session(s) could not be matched to a generator slot`);
  if (unseen > 0) notes.push(`spawn_latency_ms: ${unseen} spawned session(s) never became visible within the timeout`);
  if (samples.length === 0 && unseen > 0) {
    return unknownKpi(id, "sessions_not_rendered", {
      n: 0,
      detail: `none of ${unseen} spawned session(s) became visible: the setup does not show a new session in the ${p.arm} arm`,
    });
  }
  return measureKpi(id, samples);
}
