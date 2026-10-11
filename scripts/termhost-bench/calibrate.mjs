#!/usr/bin/env node
/**
 * termhost-bench calibrator — derives the `claude-tui` profile's statistics from
 * util-linux `script --log-timing` recordings of real `claude` sessions, and writes
 * NUMBERS ONLY: no byte of recorded content reaches the output (plan D3,
 * git-operations commit-hygiene — a recording carries code and possibly secrets, so
 * it is deleted after calibration and never committed).
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 1.
 *
 * Recording recipe (on a Linux box with `script` and `claude`; README.md has it):
 *   script -q --log-timing t1.tm --log-out t1.ts -c 'timeout 300 claude ...'
 *
 * Inputs: both util-linux timing formats —
 *   classic   "<delay-seconds> <bytes>" per output chunk (`--log-timing` with only
 *             output logged); geometry then comes from --cols/--rows;
 *   advanced  "<O|I|H|S> <delay-seconds> <data>" (`--log-io`, or `--logging-format
 *             advanced`); COLUMNS/LINES headers give the geometry.
 * The typescript's "Script started on ..." line, when present, is skipped.
 *
 * Analysis (thresholds in ANALYSIS_DEFAULTS):
 *   frames   a DEC 2026 synchronized block is one frame; other output chunks closer
 *            than `frameGapMs` merge into one write
 *   classes  synchronized frame <= `spinnerMaxBytes` -> thinking (spinner), larger ->
 *            repaint; unsynchronized runs -> tool_burst when fast or holding a big
 *            write, else stream; a silence >= `idleGapMs` -> idle
 *   runs     consecutive frames of one class form a phase run lasting until the next
 *            run starts (or, before an idle gap, one typical frame interval past its
 *            last frame)
 * and from those: per-phase frame rate, duration and frame/chunk-size quantiles,
 * transition counts, and the recording's own `expected` statistics (bytes/s, share
 * of bytes in synchronized blocks, phase time shares, frame-size quantiles) that the
 * generator's conformance test holds the fitted profile to.
 *
 * Usage:
 *   node scripts/termhost-bench/calibrate.mjs --timing t1.tm --typescript t1.ts
 *        [--timing t2.tm --typescript t2.ts ...] [--cols 120 --rows 40]
 *        [--id claude-tui] [--out scripts/termhost-bench/profiles/claude-tui.json]
 */

import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { SYNC_BEGIN, SYNC_END } from "../tui-repaint-generator.mjs";
import { PROFILE_SCHEMA_ID, assertValidProfile } from "./profile.mjs";

export const ANALYSIS_DEFAULTS = Object.freeze({
  idleGapMs: 1000,
  frameGapMs: 4,
  spinnerMaxBytes: 1024,
  burstMinBytesPerS: 32768,
  burstMinFrameBytes: 4096,
  runGapMs: 250,
});

/** Quantile probabilities written into every fitted table. */
export const QUANTILE_PS = Object.freeze([0, 0.1, 0.25, 0.5, 0.75, 0.9, 1]);

/** Phase classes -> profile phase ids and kinds (the same ids the uncalibrated profile uses). */
export const PHASE_CLASSES = Object.freeze({
  idle: "idle",
  thinking: "spinner",
  stream: "stream",
  repaint: "repaint",
  tool_burst: "burst",
});

const SYNC_BEGIN_BYTES = Buffer.from(SYNC_BEGIN, "latin1");
const SYNC_END_BYTES = Buffer.from(SYNC_END, "latin1");
const ALT_SCREEN_BYTES = Buffer.from("\x1b[?1049h", "latin1");
const SCRIPT_HEADER = Buffer.from("Script started on ", "latin1");

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/**
 * Parse a util-linux timing log (classic or advanced).
 *
 * @param {string} text
 * @returns {{format: "classic" | "advanced", entries: {type: string, delay: number, bytes?: number}[],
 *   geometry: {cols?: number, rows?: number}, resized: boolean}}
 */
export function parseTiming(text) {
  const lines = String(text)
    .split(/\r?\n/)
    .filter((l) => l.trim() !== "");
  if (lines.length === 0) throw new Error("timing log is empty");
  const format = /^[OIHS]\s/.test(lines[0]) ? "advanced" : "classic";
  const entries = [];
  const geometry = {};
  let resized = false;
  lines.forEach((line, i) => {
    if (format === "classic") {
      const m = /^(\d+(?:\.\d+)?)\s+(\d+)\s*$/.exec(line);
      if (!m) throw new Error(`timing line ${i + 1}: not "<delay> <bytes>"`);
      entries.push({ type: "O", delay: Number(m[1]), bytes: Number(m[2]) });
      return;
    }
    const m = /^([OIHS])\s+(\d+(?:\.\d+)?)(?:\s+(.*))?$/.exec(line);
    if (!m) throw new Error(`timing line ${i + 1}: not "<O|I|H|S> <delay> <data>"`);
    const [, type, delayText, rest = ""] = m;
    const delay = Number(delayText);
    if (type === "O" || type === "I") {
      if (!/^\d+$/.test(rest.trim()))
        throw new Error(`timing line ${i + 1}: ${type} entry needs a byte count`);
      entries.push({ type, delay, bytes: Number(rest.trim()) });
    } else if (type === "H") {
      const [name, value] = rest.split(/\s+/, 2);
      if (name === "COLUMNS" && /^\d+$/.test(value ?? "")) geometry.cols = Number(value);
      if (name === "LINES" && /^\d+$/.test(value ?? "")) geometry.rows = Number(value);
      entries.push({ type, delay });
    } else {
      if (/^SIGWINCH\b/.test(rest)) resized = true;
      entries.push({ type, delay });
    }
  });
  return { format, entries, geometry, resized };
}

/**
 * Drop the "Script started on ..." line util-linux writes at the top of a typescript.
 *
 * @param {Buffer} typescript
 * @returns {Buffer}
 */
export function stripScriptHeader(typescript) {
  if (typescript.subarray(0, SCRIPT_HEADER.length).equals(SCRIPT_HEADER)) {
    const nl = typescript.indexOf(0x0a);
    return nl < 0 ? Buffer.alloc(0) : typescript.subarray(nl + 1);
  }
  return typescript;
}

/**
 * Pair timing entries with typescript bytes: one chunk per output entry, timed at
 * the cumulative delay (every entry's delay counts in the advanced format).
 *
 * @param {ReturnType<typeof parseTiming>} timing
 * @param {Buffer} typescript
 * @returns {{t: number, bytes: Buffer}[]} t in seconds from the recording's start
 */
export function reconstructChunks(timing, typescript) {
  const body = stripScriptHeader(typescript);
  const chunks = [];
  let t = 0;
  let offset = 0;
  for (const e of timing.entries) {
    t += e.delay;
    if (e.type !== "O") continue;
    if (offset + e.bytes > body.length) {
      throw new Error(
        `timing claims ${offset + e.bytes} output bytes but the typescript holds ${body.length}`,
      );
    }
    if (e.bytes > 0) chunks.push({ t, bytes: body.subarray(offset, offset + e.bytes) });
    offset += e.bytes;
  }
  return chunks;
}

// ---------------------------------------------------------------------------
// Analysis
// ---------------------------------------------------------------------------

/**
 * Split recorded output into frames: synchronized blocks, and unsynchronized writes
 * (chunks closer than frameGapMs merged).
 *
 * @param {{t: number, bytes: Buffer}[]} chunks
 * @param {typeof ANALYSIS_DEFAULTS} [opts]
 * @returns {{t: number, bytes: number, sync: boolean}[]} t = time the frame completed
 */
export function segmentFrames(chunks, opts = ANALYSIS_DEFAULTS) {
  if (chunks.length === 0) return [];
  const all = Buffer.concat(chunks.map((c) => c.bytes));
  const starts = [];
  let acc = 0;
  for (const c of chunks) {
    starts.push(acc);
    acc += c.bytes.length;
  }
  const chunkAt = (offset) => {
    let lo = 0;
    let hi = starts.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (starts[mid] <= offset) lo = mid;
      else hi = mid - 1;
    }
    return lo;
  };
  const frames = [];
  const plain = (from, to) => {
    // Unsynchronized bytes [from, to): one piece per chunk, merged across short gaps.
    let pos = from;
    let current = null;
    while (pos < to) {
      const ci = chunkAt(pos);
      const end = Math.min(to, ci + 1 < starts.length ? starts[ci + 1] : all.length);
      const t = chunks[ci].t;
      if (current && (t - current.t) * 1000 < opts.frameGapMs) {
        current.bytes += end - pos;
        current.t = t;
      } else {
        if (current) frames.push(current);
        current = { t, bytes: end - pos, sync: false };
      }
      pos = end;
    }
    if (current) frames.push(current);
  };
  let pos = 0;
  while (pos < all.length) {
    const begin = all.indexOf(SYNC_BEGIN_BYTES, pos);
    if (begin < 0) {
      plain(pos, all.length);
      break;
    }
    if (begin > pos) plain(pos, begin);
    const endAt = all.indexOf(SYNC_END_BYTES, begin + SYNC_BEGIN_BYTES.length);
    const end = endAt < 0 ? all.length : endAt + SYNC_END_BYTES.length;
    frames.push({ t: chunks[chunkAt(end - 1)].t, bytes: end - begin, sync: true });
    pos = end;
  }
  return frames;
}

/**
 * Give every frame a phase class.
 *
 * @param {{t: number, bytes: number, sync: boolean}[]} frames
 * @param {typeof ANALYSIS_DEFAULTS} [opts]
 * @returns {{t: number, bytes: number, sync: boolean, cls: string}[]}
 */
export function classifyFrames(frames, opts = ANALYSIS_DEFAULTS) {
  const out = frames.map((f) => ({
    ...f,
    cls: f.sync ? (f.bytes <= opts.spinnerMaxBytes ? "thinking" : "repaint") : null,
  }));
  // Unsynchronized frames: classify each run (frames separated by < runGapMs, no synchronized frame between).
  let i = 0;
  while (i < out.length) {
    if (out[i].cls !== null) {
      i += 1;
      continue;
    }
    let j = i;
    while (
      j + 1 < out.length &&
      out[j + 1].cls === null &&
      (out[j + 1].t - out[j].t) * 1000 < opts.runGapMs
    )
      j += 1;
    const run = out.slice(i, j + 1);
    const bytes = run.reduce((a, f) => a + f.bytes, 0);
    const span = run[run.length - 1].t - run[0].t;
    const burst =
      run.some((f) => f.bytes >= opts.burstMinFrameBytes) ||
      (span > 0 && bytes / span >= opts.burstMinBytesPerS);
    for (const f of run) f.cls = burst ? "tool_burst" : "stream";
    i = j + 1;
  }
  return out;
}

function median(sorted) {
  return sorted.length === 0 ? null : sorted[Math.floor((sorted.length - 1) / 2)];
}

/**
 * Group classified frames into phase runs (idle runs for silences >= idleGapMs).
 *
 * @param {{t: number, bytes: number, cls: string}[]} frames
 * @param {typeof ANALYSIS_DEFAULTS} [opts]
 * @returns {{cls: string, start: number, end: number, frames: number, frameBytes: number[]}[]} times in seconds
 */
export function buildRuns(frames, opts = ANALYSIS_DEFAULTS) {
  const idleGap = opts.idleGapMs / 1000;
  const groups = [];
  for (const f of frames) {
    const last = groups[groups.length - 1];
    const prevT = last ? last.times[last.times.length - 1] : null;
    if (last && last.cls === f.cls && f.t - prevT < idleGap) {
      last.times.push(f.t);
      last.frameBytes.push(f.bytes);
    } else {
      groups.push({ cls: f.cls, times: [f.t], frameBytes: [f.bytes] });
    }
  }
  // A class's typical frame interval, for runs whose length a single frame cannot show.
  const intervals = {};
  for (const g of groups) {
    for (let k = 1; k < g.times.length; k += 1)
      (intervals[g.cls] ??= []).push(g.times[k] - g.times[k - 1]);
  }
  const typical = (cls) => median((intervals[cls] ?? []).sort((a, b) => a - b)) ?? 1 / 30;
  const runs = [];
  const firstT = frames.length ? frames[0].t : 0;
  if (firstT >= idleGap)
    runs.push({ cls: "idle", start: 0, end: firstT, frames: 0, frameBytes: [] });
  groups.forEach((g, k) => {
    const start = g.times[0];
    const lastT = g.times[g.times.length - 1];
    const next = groups[k + 1];
    const ownEnd = lastT + typical(g.cls);
    let end = ownEnd;
    if (next) {
      const gap = next.times[0] - lastT;
      end = gap >= idleGap ? Math.min(ownEnd, next.times[0]) : next.times[0];
    }
    runs.push({ cls: g.cls, start, end, frames: g.times.length, frameBytes: g.frameBytes });
    if (next && next.times[0] - lastT >= idleGap) {
      runs.push({ cls: "idle", start: end, end: next.times[0], frames: 0, frameBytes: [] });
    }
  });
  return runs;
}

/**
 * Analyse one recording.
 *
 * @param {{t: number, bytes: Buffer}[]} chunks
 * @param {typeof ANALYSIS_DEFAULTS} [opts]
 */
export function analyzeChunks(chunks, opts = ANALYSIS_DEFAULTS) {
  const frames = classifyFrames(segmentFrames(chunks, opts), opts);
  const runs = buildRuns(frames, opts);
  const totalBytes = chunks.reduce((a, c) => a + c.bytes.length, 0);
  const syncBytes = frames.filter((f) => f.sync).reduce((a, f) => a + f.bytes, 0);
  const altScreen = chunks.some((c) => c.bytes.includes(ALT_SCREEN_BYTES));
  return {
    durationS: chunks.length ? chunks[chunks.length - 1].t : 0,
    totalBytes,
    syncBytes,
    altScreen,
    frames,
    runs,
  };
}

/**
 * Nearest-rank quantile table over samples at QUANTILE_PS.
 *
 * @param {number[]} samples
 * @param {(v: number) => number} [round]
 * @returns {number[][]}
 */
export function quantileTable(samples, round = (v) => v) {
  if (samples.length === 0) throw new Error("a quantile table needs samples");
  const sorted = [...samples].sort((a, b) => a - b);
  return QUANTILE_PS.map((p) => {
    const rank = p === 0 ? 1 : Math.ceil(p * sorted.length);
    return [p, round(sorted[Math.min(sorted.length, rank) - 1])];
  });
}

const round3 = (v) => Math.round(v * 1000) / 1000;
const roundInt = (v) => Math.round(v);

/**
 * Fit a profile from one or more analysed recordings.
 *
 * @param {ReturnType<typeof analyzeChunks>[]} analyses
 * @param {{id?: string, geometry: {cols: number, rows: number}, calibratedAt?: string}} opts
 * @returns {object} a valid `termhost-bench/profile/v1` profile with calibrated: true
 */
export function fitProfile(
  analyses,
  { id = "claude-tui", geometry, calibratedAt = new Date().toISOString() },
) {
  if (!geometry || !Number.isInteger(geometry.cols) || !Number.isInteger(geometry.rows)) {
    throw new Error(
      "the recording's geometry is unknown: pass --cols and --rows (the classic timing format carries none)",
    );
  }
  const per = {};
  const transitions = {};
  const firsts = [];
  let totalBytes = 0;
  let syncBytes = 0;
  let durationS = 0;
  let altScreen = false;
  for (const a of analyses) {
    if (a.runs.length === 0) continue;
    totalBytes += a.totalBytes;
    syncBytes += a.syncBytes;
    durationS += a.durationS;
    altScreen ||= a.altScreen;
    firsts.push(a.runs[0].cls);
    a.runs.forEach((run, k) => {
      const p = (per[run.cls] ??= { durationsMs: [], frames: 0, seconds: 0, frameBytes: [] });
      const seconds = run.end - run.start;
      p.durationsMs.push(Math.max(1, seconds * 1000));
      p.frames += run.frames;
      p.seconds += seconds;
      p.frameBytes.push(...run.frameBytes);
      const next = a.runs[k + 1];
      if (next) {
        const row = (transitions[run.cls] ??= {});
        row[next.cls] = (row[next.cls] ?? 0) + 1;
      }
    });
  }
  const classes = Object.keys(PHASE_CLASSES).filter((c) => per[c]);
  if (classes.length === 0) throw new Error("the recordings contain no output to calibrate from");
  const initial = mostCommon(firsts);
  for (const cls of classes) if (!transitions[cls]) transitions[cls] = { [initial]: 1 };

  const phases = {};
  for (const cls of classes) {
    const p = per[cls];
    const phase = {
      kind: PHASE_CLASSES[cls],
      duration_ms: quantileTable(p.durationsMs, (v) => Math.max(1, roundInt(v))),
    };
    if (cls !== "idle") {
      phase.fps = round3(Math.max(0.001, p.frames / Math.max(p.seconds, 1e-6)));
      const sizes = quantileTable(p.frameBytes, (v) => Math.max(1, roundInt(v)));
      if (cls === "repaint") phase.frame_bytes = sizes;
      if (cls === "stream" || cls === "tool_burst") phase.chunk_bytes = sizes;
    }
    phases[cls] = phase;
  }
  const totalRunSeconds = classes.reduce((a, c) => a + per[c].seconds, 0);
  const allFrames = analyses.flatMap((a) => a.frames.map((f) => f.bytes));
  const profile = {
    schema: PROFILE_SCHEMA_ID,
    id,
    title: FITTED_TITLE,
    description: FITTED_DESCRIPTION,
    calibrated: true,
    provenance: {
      kind: "calibration",
      source: FITTED_SOURCE,
      recordings: analyses.length,
      duration_s: round3(Math.max(durationS, 0.001)),
      calibrated_at: calibratedAt,
    },
    geometry: { cols: geometry.cols, rows: geometry.rows },
    marker_hz: 10,
    alt_screen: altScreen,
    initial_phase: initial,
    phases,
    transitions: Object.fromEntries(classes.map((c) => [c, transitions[c]])),
    expected: {
      bytes_per_s: round3(durationS > 0 ? totalBytes / durationS : 0),
      sync_share: round3(totalBytes > 0 ? syncBytes / totalBytes : 0),
      phase_share: Object.fromEntries(
        classes.map((c) => [c, round3(totalRunSeconds > 0 ? per[c].seconds / totalRunSeconds : 0)]),
      ),
      frame_bytes: quantileTable(allFrames.length ? allFrames : [1], (v) =>
        Math.max(1, roundInt(v)),
      ),
    },
  };
  assertValidProfile(profile, "fitted by calibrate.mjs");
  assertNumbersOnly(profile, { allow: [id] });
  return profile;
}

function mostCommon(list) {
  const counts = new Map();
  for (const x of list) counts.set(x, (counts.get(x) ?? 0) + 1);
  let best = list[0];
  for (const [k, v] of counts) if (v > counts.get(best)) best = k;
  return best;
}

const FITTED_TITLE = "Claude-Code-shaped AI session (calibrated)";
const FITTED_DESCRIPTION =
  "Fitted by calibrate.mjs from util-linux script --log-timing recordings of real claude sessions. Numbers only: no recorded content.";
const FITTED_SOURCE =
  "util-linux script --log-timing recordings, analysed by scripts/termhost-bench/calibrate.mjs";

/** Every string (key or value) a fitted profile may contain besides its id and date. */
const VOCABULARY = new Set([
  PROFILE_SCHEMA_ID,
  FITTED_TITLE,
  FITTED_DESCRIPTION,
  FITTED_SOURCE,
  "calibration",
  ...Object.keys(PHASE_CLASSES),
  ...Object.values(PHASE_CLASSES),
  "schema",
  "id",
  "title",
  "description",
  "calibrated",
  "provenance",
  "kind",
  "source",
  "recordings",
  "duration_s",
  "calibrated_at",
  "geometry",
  "cols",
  "rows",
  "marker_hz",
  "alt_screen",
  "initial_phase",
  "phases",
  "transitions",
  "expected",
  "bytes_per_s",
  "sync_share",
  "phase_share",
  "frame_bytes",
  "chunk_bytes",
  "fps",
  "duration_ms",
]);
const ISO_UTC = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z$/;

/**
 * Throw if a fitted profile holds any string outside the fixed vocabulary (plus its
 * own id and an ISO date) — the mechanical half of "numbers only".
 *
 * @param {unknown} value
 * @param {{allow?: string[]}} [opts]
 * @param {string} [path]
 */
export function assertNumbersOnly(value, { allow = [] } = {}, path = "") {
  const check = (s, at) => {
    if (!VOCABULARY.has(s) && !allow.includes(s) && !ISO_UTC.test(s)) {
      throw new Error(`calibrate output holds a free-text string at ${at || "/"}`);
    }
  };
  if (typeof value === "string") {
    check(value, path);
  } else if (Array.isArray(value)) {
    value.forEach((v, i) => assertNumbersOnly(v, { allow }, `${path}/${i}`));
  } else if (value && typeof value === "object") {
    for (const [k, v] of Object.entries(value)) {
      check(k, `${path}/<key ${JSON.stringify(k).slice(0, 20)}>`);
      assertNumbersOnly(v, { allow }, `${path}/${k}`);
    }
  }
}

/**
 * Calibrate from recording file pairs.
 *
 * @param {{timing: string, typescript: string}[]} recordings paths
 * @param {{id?: string, geometry?: {cols?: number, rows?: number}, calibratedAt?: string, analysis?: object}} [opts]
 */
export function calibrateFiles(
  recordings,
  { id, geometry = {}, calibratedAt, analysis = ANALYSIS_DEFAULTS } = {},
) {
  if (recordings.length === 0) throw new Error("no recordings given");
  let cols = geometry.cols;
  let rows = geometry.rows;
  const analyses = recordings.map(({ timing, typescript }) => {
    const parsed = parseTiming(readFileSync(timing, "utf8"));
    if (parsed.resized)
      throw new Error(
        `${timing}: the terminal was resized mid-recording; record at one fixed size`,
      );
    cols ??= parsed.geometry.cols;
    rows ??= parsed.geometry.rows;
    if (
      (parsed.geometry.cols !== undefined && parsed.geometry.cols !== cols) ||
      (parsed.geometry.rows !== undefined && parsed.geometry.rows !== rows)
    ) {
      throw new Error(`${timing}: recorded at a different geometry from the others`);
    }
    return analyzeChunks(reconstructChunks(parsed, readFileSync(typescript)), analysis);
  });
  return fitProfile(analyses, { id, geometry: { cols, rows }, calibratedAt });
}

/**
 * @param {string[]} argv
 */
export function parseCalibrateArgs(argv) {
  const timings = [];
  const scripts = [];
  const opts = { id: "claude-tui", geometry: {}, out: null };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    const value = () => {
      const v = argv[i + 1];
      if (v === undefined || v.startsWith("--")) throw new Error(`${a} needs a value`);
      i += 1;
      return v;
    };
    const int = (name) => {
      const v = value();
      if (!/^\d+$/.test(v)) throw new Error(`${name} needs a positive integer`);
      return Number(v);
    };
    if (a === "--timing") timings.push(value());
    else if (a === "--typescript") scripts.push(value());
    else if (a === "--cols") opts.geometry.cols = int("--cols");
    else if (a === "--rows") opts.geometry.rows = int("--rows");
    else if (a === "--id") opts.id = value();
    else if (a === "--out") opts.out = value();
    else throw new Error(`unknown argument: ${a}`);
  }
  if (timings.length === 0 || timings.length !== scripts.length) {
    throw new Error("give one --typescript per --timing (at least one pair)");
  }
  opts.recordings = timings.map((timing, k) => ({ timing, typescript: scripts[k] }));
  return opts;
}

const invokedDirectly =
  process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (invokedDirectly) {
  try {
    const opts = parseCalibrateArgs(process.argv.slice(2));
    const profile = calibrateFiles(opts.recordings, opts);
    const text = `${JSON.stringify(profile, null, 2)}\n`;
    if (opts.out) {
      writeFileSync(opts.out, text);
      process.stderr.write(`wrote ${opts.out}\n`);
    } else {
      process.stdout.write(text);
    }
  } catch (e) {
    process.stderr.write(`calibrate: ${e.message}\n`);
    process.exitCode = 2;
  }
}
