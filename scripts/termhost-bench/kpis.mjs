/**
 * termhost-bench KPI computation: the probe's records (`<gen-dir>/probe.jsonl`,
 * Phase 2's `termhost-probe/v1`) and the generators' records (`gen-<k>.jsonl`,
 * Phase 1's `termhost-bench/gen/v1`) in, one catalog value per KPI out — every
 * number through `stats.mjs`, so a statistic below its minimum sample count is
 * `unknown`, never a plausible-looking 0, and every unknown carries its reason.
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 3 ("compute
 * KPIs from probe.jsonl plus the generator logs with stats.mjs"). Pure: no files,
 * clocks or processes — the run loop reads the records and hands them over, and the
 * tests feed fixtures.
 *
 * Why the harness recomputes rather than copying the probe's own `summary`: the
 * summary covers the whole `watch`, while a KPI covers exactly its measurement
 * window, and the generators' records span their whole life (markers emitted before
 * the window, echo markers re-drawn from an earlier input phase). Every KPI here is
 * restricted to the window `[summary.t_start_ns, summary.t_end_ns]` of the watch
 * that measured it. The rules are the probe's (`crates/termhost-probe/src/model/`):
 * nearest-rank statistics, first appearance on the PRIMARY source, the
 * loss-versus-coalescing split at two refresh intervals, interval exclusion.
 *
 * Phases a row is computed from:
 *   soak    one `watch` over the steady state — latency, cadence, loss, hangs,
 *           process accounting (host / workload / compositor groups);
 *   input   a `watch` beside an `inject` — keystroke-to-echo latency (D9);
 *   switch  a `watch` while the adapter shows session after session (focus arm);
 *   spawn   a `watch` while the adapter adds and closes sessions.
 */

import { KPI_CATALOG, getKpi } from "./kpi-catalog.mjs";
import { SEQ_MODULUS } from "./marker.mjs";
import { measureKpi, notApplicableKpi, percentile, share, slope, unknownKpi } from "./stats.mjs";

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

/**
 * Classify one `watch`'s records (the last `start`..`end` span when several are
 * present). Records of another schema are ignored.
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
 * What the segment's capture looked like: the sources opened, the primary one,
 * and whether the refresh rate is KNOWN (the probe reports an assumed 60 Hz on a
 * server whose RandR names no rate — `platform/linux/x11.rs` `refresh` — which is
 * not a known refresh rate).
 */
export function captureInfo(seg) {
  const opened = seg.captures.filter((c) => c.state === "started").map((c) => c.source);
  const failed = seg.captures
    .filter((c) => c.state === "failed")
    .map((c) => ({ source: c.source, detail: c.detail }));
  const primary = seg.summary?.primary ?? opened[0] ?? null;
  const ref = seg.captures.find((c) => c.refresh_hz !== undefined) ?? null;
  const refreshHz = typeof ref?.refresh_hz === "number" ? ref.refresh_hz : null;
  const refreshSource = typeof ref?.refresh_source === "string" ? ref.refresh_source : null;
  // An assumed rate is not a known one, however it is spelled: a null rate, an
  // explicit flag (the probe's planned `refresh_assumed`), or a source that says so.
  const assumed =
    ref?.refresh_assumed === true ||
    ref?.refresh_known === false ||
    (refreshSource !== null && /^assumed/i.test(refreshSource));
  const refreshKnown = refreshHz !== null && refreshHz > 0 && refreshSource !== null && !assumed;
  return { opened, failed, primary, refreshHz, refreshSource, refreshKnown };
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

/** Intervals (ms) between consecutive primary frames. */
export function frameIntervalsMs(frames) {
  const out = [];
  for (let i = 1; i < frames.length; i += 1) out.push(nsToMs(frames[i].t - frames[i - 1].t));
  return out;
}

/** Sessions whose output marker any primary frame carried. */
export function sessionsSeen(frames) {
  const s = new Set();
  for (const f of frames) for (const m of f.markers ?? []) if (m.kind === "output") s.add(m.session);
  return s;
}

/** How long a session counts as displayed after its marker was last observed. */
export const DISPLAYED_HORIZON_MS = 500;

/**
 * V measured: the median, over the window's primary frames, of the number of
 * sessions whose output marker was observed within the last
 * {@link DISPLAYED_HORIZON_MS} — the "currently displayed" state accumulated across
 * frames rather than the markers inside one frame. On X11 a "frame" is one damaged
 * band of the window, so a per-frame count under-reports V in a tiled arm whose
 * panes repaint at different times (Phase 2 review S5); every visible session
 * redraws its marker at the profile's marker rate (>= 2 Hz), so a displayed session
 * is re-observed well inside the horizon. On a full-window source (WGC) every
 * frame carries every visible marker and the two counts agree. The first horizon of
 * the window is skipped while that state fills.
 *
 * @param {{t: bigint, markers?: {kind: string, session: number}[]}[]} frames sorted by t
 * @returns {number | null}
 */
export function vActual(frames, { horizonMs = DISPLAYED_HORIZON_MS } = {}) {
  if (frames.length === 0) return null;
  const horizon = BigInt(Math.round(horizonMs * 1e6));
  const lastSeen = new Map();
  const warm = frames[0].t + horizon;
  const vs = [];
  for (const f of frames) {
    for (const m of f.markers ?? []) if (m.kind === "output") lastSeen.set(m.session, f.t);
    if (f.t < warm) continue;
    let v = 0;
    for (const t of lastSeen.values()) if (f.t - t <= horizon) v += 1;
    vs.push(v);
  }
  if (vs.length === 0) return lastSeen.size;
  const r = percentile(vs, 50);
  return r.state === "measured" ? r.value : vs[0];
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
 * Output latencies: for every output marker a visible session emitted inside the
 * window, its first appearance minus its emission. A first appearance BEFORE the
 * emission is impossible on a shared timebase: counted as an anomaly and excluded,
 * exactly as the probe does (`model/analysis.rs` `latencies`).
 */
export function outputLatencies(seg, gens, visible, primary, win) {
  const seen = firstSeenIndex(seg, primary);
  const samples = [];
  let negative = 0;
  const perSession = new Map();
  for (const session of visible) {
    const g = gens.get(session);
    if (!g) continue;
    let count = 0;
    for (const m of g.markers) {
      if (m.kind !== "output" || m.tEmit < win.t0 || m.tEmit > win.t1) continue;
      const t = seen.get(`output:${session}:${m.seq % SEQ_MODULUS}`);
      if (t === undefined) continue;
      if (t < m.tEmit) {
        negative += 1;
        continue;
      }
      samples.push(nsToMs(t - m.tEmit));
      count += 1;
    }
    perSession.set(session, count);
  }
  return { samples, negative, perSession };
}

/**
 * The interval grid of a watch (`model/intervals.rs`): validity per 500 ms
 * interval and whether a span touched an invalid or partial one.
 */
export function intervalModel(seg) {
  const recs = seg.intervals
    .map((r) => ({ ...r, a: toNs(r.t_start_ns), b: toNs(r.t_end_ns) }))
    .filter((r) => r.a !== null && r.b !== null && Number.isInteger(r.index));
  const full = recs.find((r) => !r.partial);
  const lenNs = full ? full.b - full.a : 500n * NS_PER_MS;
  const start =
    toNs(seg.summary?.t_start_ns) ??
    (recs.length > 0 ? recs[0].a - BigInt(recs[0].index) * lenNs : null);
  const byIndex = new Map(recs.map((r) => [r.index, r]));
  let valid = 0;
  let occluded = 0;
  let operator = 0;
  let harness = 0;
  for (const r of recs) {
    if (r.partial) continue;
    if (r.valid && !r.exclusion_class) valid += 1;
    else if (r.exclusion_class === "occluded") occluded += 1;
    else if (r.exclusion_class === "operator_activity") operator += 1;
    // Any other class (the probe's planned third class for its OWN failures — a
    // listener that never started, no window inspector) is the harness's fault,
    // never the operator's or an occluder's (Phase 2 review S14).
    else harness += 1;
  }
  const spanValid = (a, b) => {
    if (start === null || lenNs <= 0n || b < start) return false;
    const first = a < start ? 0n : (a - start) / lenNs;
    const last = (b - start) / lenNs;
    for (let i = first; i <= last; i += 1n) {
      const r = byIndex.get(Number(i));
      if (!r || !r.valid || r.partial) return false;
    }
    return true;
  };
  return { start, lenNs, byIndex, tally: { valid, occluded, operator, harness }, spanValid };
}

/**
 * Why too few valid units remain (`Model::exclusion_reason`, with two corrections
 * from the Phase 2 review): when no full interval was actually excluded the cause
 * is too few samples, not occlusion (S4); and the probe's own failures read as
 * `harness_error`, never as operator activity or occlusion (S14). Ties go to the
 * harness, then to the operator.
 */
export function exclusionReason(tally, valid, excluded, minN) {
  if (valid + excluded < minN) return "insufficient_samples";
  const harness = tally.harness ?? 0;
  if (tally.occluded + tally.operator + harness === 0) return "insufficient_samples";
  if (harness > 0 && harness >= tally.operator && harness >= tally.occluded) return "harness_error";
  return tally.operator > tally.occluded ? "operator_activity" : "occluded";
}

/**
 * Marker loss versus coalescing per visible session (`model/analysis.rs` `loss`).
 * A sequence is current from its emission until the next one's (the last until
 * the window's end); eligible when current >= 2 refresh intervals.
 */
export function lossTally(seg, gens, visible, primary, win, refreshNs, model) {
  const seen = firstSeenIndex(seg, primary);
  const t = { eligible: 0, lost: 0, outOfOrder: 0, counted: 0, skipped: 0, excluded: 0 };
  for (const session of visible) {
    const g = gens.get(session);
    if (!g) continue;
    const seqs = g.markers.filter((m) => m.kind === "output" && m.tEmit >= win.t0 && m.tEmit <= win.t1);
    const seenAt = seqs.map((m) => seen.get(`output:${session}:${m.seq % SEQ_MODULUS}`) ?? null);
    const suffixMin = new Array(seqs.length + 1).fill(null);
    for (let j = seqs.length - 1; j >= 0; j -= 1) {
      const nxt = suffixMin[j + 1];
      const s = seenAt[j];
      suffixMin[j] = s === null ? nxt : nxt === null || s < nxt ? s : nxt;
    }
    for (let j = 0; j < seqs.length; j += 1) {
      const begin = seqs[j].tEmit;
      const end = j + 1 < seqs.length ? seqs[j + 1].tEmit : win.t1;
      if (!model.spanValid(begin, end)) {
        t.excluded += 1;
        continue;
      }
      t.counted += 1;
      const eligible = end - begin >= 2n * refreshNs;
      const ooo = seenAt[j] !== null && suffixMin[j + 1] !== null && suffixMin[j + 1] < seenAt[j];
      if (eligible) {
        t.eligible += 1;
        if (seenAt[j] === null || ooo) t.lost += 1;
      } else if (seenAt[j] === null) {
        t.skipped += 1;
      }
      if (ooo) t.outOfOrder += 1;
    }
  }
  return t;
}

/** Is marker seq `m` (mod M) at or after the full counter `newest`? */
export function seqAtOrAfter(m, newest) {
  if (newest === null || newest === undefined) return true;
  const base = newest % SEQ_MODULUS;
  return (m - base + SEQ_MODULUS) % SEQ_MODULUS < SEQ_MODULUS / 2;
}

/**
 * Time from a command to the first primary frame showing `session`'s output
 * marker at or after `newestSeq` (null = any), within `timeoutNs`.
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

/**
 * Per session over the window: producer stall (share of wall time blocked inside
 * `write`), delivered ratio and bytes written — each counter interpolated at the
 * window's ends between the records that bracket them. A session with no record
 * after the window (a generator killed while blocked in one write, which then runs
 * no JavaScript at all) has no reading and is named in the notes, not dropped
 * silently.
 */
export function generatorWindow(gens, win) {
  const stall = [];
  const delivered = [];
  let bytes = 0;
  const notes = [];
  const silent = [];
  let clamped = 0;
  for (const [session, g] of gens) {
    const b0 = interpolate(g.points, win.t0, "blocked");
    const b1 = interpolate(g.points, win.t1, "blocked");
    const w0 = interpolate(g.points, win.t0, "written");
    const w1 = interpolate(g.points, win.t1, "written");
    const s0 = interpolate(g.points, win.t0, "scheduled");
    const s1 = interpolate(g.points, win.t1, "scheduled");
    if (b0 === null || b1 === null || w0 === null || w1 === null) {
      silent.push(session);
      continue;
    }
    let pct = ((b1 - b0) / Number(win.t1 - win.t0)) * 100;
    if (pct > 100) {
      pct = 100;
      clamped += 1;
    }
    stall.push(Math.max(0, pct));
    bytes += Math.max(0, w1 - w0);
    if (s0 !== null && s1 !== null && s1 - s0 > 0) delivered.push((w1 - w0) / (s1 - s0));
  }
  if (silent.length > 0) {
    notes.push(
      `producer_stall_pct: ${silent.length} session(s) have no generator record bracketing the window (${silent.join(", ")}): ` +
        "a generator blocked inside one write writes nothing until it returns",
    );
  }
  if (clamped > 0) notes.push(`producer_stall_pct: ${clamped} session stall reading(s) above 100% were clamped (a write straddling a record)`);
  return { stall, delivered, bytes, notes, silent };
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
  const clockOk = stateOf(p.capabilities, "clock.aligned") === "OPERATIVE";

  // --- the capture gate --------------------------------------------------
  let frames = [];
  let va = null;
  let visible = new Set();
  let captureGate = null;
  if (!seg || !win) {
    captureGate = { reason: "harness_error", detail: "no complete soak watch segment was recorded" };
  } else if (!cap.primary || cap.opened.length === 0) {
    captureGate = {
      reason: "capture_unavailable",
      detail:
        cap.failed.length > 0
          ? `no capture source opened: ${cap.failed.map((f) => `${f.source}: ${f.detail}`).join("; ")}`
          : "no capture source opened",
    };
  } else {
    frames = primaryFrames(seg, cap.primary, win);
    va = vActual(frames);
    visible = sessionsSeen(frames);
    if (p.armFailure) {
      captureGate = p.armFailure;
    } else if (frames.length === 0) {
      captureGate = { reason: "sessions_not_rendered", detail: "the window produced no captured frame in the soak" };
    } else if (p.arm === "focus" ? va !== 1 : va < p.vTarget) {
      captureGate = {
        reason: p.arm === "focus" && p.setupId === "runner" && va > 1 ? "zone_unresolved" : "sessions_not_rendered",
        detail: `V measured ${va} per frame (median), the ${p.arm} arm needs ${p.arm === "focus" ? "exactly 1" : `>= ${p.vTarget}`}; sessions seen: ${[...visible].sort((a, b) => a - b).join(",") || "none"}`,
      };
    }
  }
  const gated = (id) => captureGate && CAPTURE_KPIS.includes(id);
  const gateValue = (id) => unknownKpi(id, captureGate.reason, { detail: captureGate.detail });
  const disagreement =
    seg && (seg.summary?.crosscheck?.verdict === "disagree" || seg.crosschecks.length > 0)
      ? seg.summary?.crosscheck?.detail ?? `${seg.crosschecks.length} cross-check disagreement(s)`
      : null;

  // --- output latency -------------------------------------------------------
  if (gated("output_latency_ms")) set("output_latency_ms", gateValue("output_latency_ms"));
  else if (!clockOk) {
    set(
      "output_latency_ms",
      unknownKpi("output_latency_ms", "clock_unaligned", {
        detail: `clock.aligned is ${stateOf(p.capabilities, "clock.aligned")}: ${p.capabilities?.["clock.aligned"]?.reason ?? "no doctor verdict"}`,
      }),
    );
  } else if (disagreement) {
    set("output_latency_ms", unknownKpi("output_latency_ms", "capture_disagreement", { detail: disagreement }));
  } else {
    const lat = outputLatencies(seg, p.gens, visible, cap.primary, win);
    if (lat.negative > 0) notes.push(`output_latency_ms: ${lat.negative} first appearance(s) before emission excluded`);
    const observed = [...lat.perSession.values()].filter((c) => c > 0).length;
    set(
      "output_latency_ms",
      measureKpi("output_latency_ms", lat.samples, {
        coverage: { observed, expected: Math.max(p.vTarget, visible.size, observed) },
      }),
    );
  }

  // --- frame cadence and jank ----------------------------------------------
  const intervals = frameIntervalsMs(frames);
  if (gated("frame_interval_ms")) set("frame_interval_ms", gateValue("frame_interval_ms"));
  else set("frame_interval_ms", measureKpi("frame_interval_ms", intervals));
  if (gated("jank_share")) set("jank_share", gateValue("jank_share"));
  else if (!cap.refreshKnown) {
    set(
      "jank_share",
      unknownKpi("jank_share", "refresh_rate_unknown", {
        n: intervals.length,
        detail: `refresh ${cap.refreshHz ?? "?"} Hz from "${cap.refreshSource ?? "nothing"}" is not a measured rate`,
      }),
    );
  } else {
    const refreshMs = 1000 / cap.refreshHz;
    set("jank_share", measureKpi("jank_share", share(intervals, (d) => d > 2 * refreshMs, { minN: 100 })));
  }

  // --- hangs, marker loss and coalescing (interval exclusion) --------------
  const model = seg ? intervalModel(seg) : null;
  if (gated("hangs")) set("hangs", gateValue("hangs"));
  else {
    const fullTotal = model.tally.valid + model.tally.occluded + model.tally.operator + model.tally.harness;
    const minN = getKpi("hangs").min_n;
    if (model.tally.valid >= minN) {
      const count = seg.hangs.filter((h) => h.valid === true).length;
      const excluded = seg.hangs.length - count;
      set(
        "hangs",
        measureKpi("hangs", { value: count, n: model.tally.valid }, excluded > 0 ? { notes: [`${excluded} hang episode(s) excluded: they touched an invalid interval`] } : {}),
      );
    } else {
      set(
        "hangs",
        unknownKpi("hangs", exclusionReason(model.tally, model.tally.valid, fullTotal - model.tally.valid, minN), {
          n: model.tally.valid,
          detail: `${model.tally.valid} valid of ${fullTotal} intervals (need ${minN}); excluded ${model.tally.occluded} occluded-class, ${model.tally.operator} operator-activity, ${model.tally.harness} probe-failure`,
        }),
      );
    }
  }
  for (const id of ["marker_loss", "marker_skipped"]) {
    if (gated(id)) {
      set(id, gateValue(id));
      continue;
    }
    if (!cap.refreshKnown) {
      set(id, unknownKpi(id, "refresh_rate_unknown", { detail: `refresh source "${cap.refreshSource ?? "nothing"}"` }));
      continue;
    }
    const refreshNs = BigInt(Math.round(1e9 / cap.refreshHz));
    const t = lossTally(seg, p.gens, visible, cap.primary, win, refreshNs, model);
    const minN = getKpi(id).min_n;
    const [hits, denom] = id === "marker_loss" ? [t.lost, t.eligible] : [t.skipped, t.counted];
    if (denom >= minN) {
      const extra = id === "marker_loss" && t.outOfOrder > 0 ? { notes: [`${t.outOfOrder} sequence(s) observed out of order`] } : {};
      set(id, measureKpi(id, { value: (hits / denom) * 100, n: denom }, extra));
    } else {
      set(
        id,
        unknownKpi(id, exclusionReason(model.tally, denom, t.excluded, minN), {
          n: denom,
          detail: `${denom} countable sequences, ${t.excluded} excluded by invalid intervals`,
        }),
      );
    }
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
    set("producer_stall_pct", measureKpi("producer_stall_pct", gw.stall, { coverage }));
    if (gw.delivered.length > 0) {
      set(
        "delivered_ratio",
        measureKpi("delivered_ratio", { value: Math.min(...gw.delivered), n: gw.delivered.length }, {
          coverage: { observed: gw.delivered.length, expected: Math.max(p.n, gw.delivered.length) },
        }),
      );
    } else {
      set("delivered_ratio", unknownKpi("delivered_ratio", "insufficient_samples", { n: 0, detail: "no session scheduled bytes in the window" }));
    }
    // cpu_ms_per_mb needs the window's bytes; computed here once both are known.
    const cpuMs = seg ? groupSeries(seg, "host", "cpu_ms") : [];
    const mb = gw.bytes / MIB;
    if (cpuMs.length < getKpi("cpu_ms_per_mb").min_n) {
      set("cpu_ms_per_mb", unknownKpi("cpu_ms_per_mb", "insufficient_samples", { n: cpuMs.length }));
    } else if (!(mb > 0)) {
      set("cpu_ms_per_mb", unknownKpi("cpu_ms_per_mb", "degenerate_input", { n: cpuMs.length, detail: "the generators wrote no bytes in the window" }));
    } else {
      const total = cpuMs.reduce((a, s) => a + s.v, 0);
      set("cpu_ms_per_mb", measureKpi("cpu_ms_per_mb", { value: total / mb, n: cpuMs.length }));
    }
  }

  // --- input, switch and spawn phases -------------------------------------
  set("input_latency_ms", inputLatencyKpi(p, clockOk, captureGate));
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
  return { kpis: ordered, vActual: va, notes: unattached, sessionsSeen: [...visible].sort((a, b) => a - b) };
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

function computeProcessKpis(p, seg, win, set, notes) {
  const noSeg = !seg || !win;
  const series = (group, field) => (noSeg ? [] : groupSeries(seg, group, field));
  const statsKpi = (id, group) => {
    const s = series(group, "cpu_pct");
    const unreadable = s.filter((x) => x.unreadable > 0).length;
    const extra = unreadable > 0 ? { notes: [`${unreadable} sample(s) had unreadable ${group} PIDs: a lower bound`] } : {};
    return measureKpi(id, s.map((x) => x.v), extra);
  };
  const endValue = (id, group) => {
    const s = series(group, "memory_mb");
    if (s.length === 0) return unknownKpi(id, "insufficient_samples", { n: 0 });
    return measureKpi(id, { value: s[s.length - 1].v, n: s.length });
  };
  const hostPids = noSeg ? 0 : Math.max(0, ...seg.samples.map((s) => s.groups?.host?.pids?.length ?? 0));
  if (!noSeg && seg.samples.length > 0 && hostPids === 0) {
    const detail = "the host process tree was empty in every sample: the probe was given no live root";
    for (const id of ["cpu_pct", "memory_mb", "memory_growth_mb_per_min", "crashes", "gpu_pct"]) {
      set(id, unknownKpi(id, "harness_error", { detail }));
    }
  } else {
    set("cpu_pct", statsKpi("cpu_pct", "host"));
    set("memory_mb", endValue("memory_mb", "host"));
    const mem = series("host", "memory_mb");
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
    } else {
      const g = series("host", "gpu_pct");
      set("gpu_pct", measureKpi("gpu_pct", g.map((x) => x.v)));
    }
  }
  set("workload_cpu_pct", statsKpi("workload_cpu_pct", "workload"));
  set("workload_memory_mb", endValue("workload_memory_mb", "workload"));
  set("compositor_cpu_pct", statsKpi("compositor_cpu_pct", "compositor"));
  set("compositor_memory_mb", endValue("compositor_memory_mb", "compositor"));
}

/**
 * Keystroke-to-echo latency from one input phase: key i of the `inject` arm is the
 * focused generator's echo with sequence `echoBase + i` (its echo counter is
 * cumulative over its life), checked byte for byte; the echo marker's first
 * appearance in the input phase's watch minus the key's `t_inject`.
 */
export function inputLatencyKpi(p, clockOk, captureGate) {
  const id = "input_latency_ms";
  const inp = p.input;
  if (!inp) return unknownKpi(id, "input_unsafe", { detail: "the input arm did not run" });
  if (inp.unavailable) return unknownKpi(id, inp.unavailable.reason, inp.unavailable.detail ? { detail: inp.unavailable.detail } : {});
  if (captureGate) return unknownKpi(id, captureGate.reason, { detail: captureGate.detail });
  if (!clockOk) return unknownKpi(id, "clock_unaligned");
  const seg = indexProbeSegment(inp.records ?? []);
  const cap = captureInfo(seg);
  if (!cap.primary) return unknownKpi(id, "capture_unavailable", { detail: "the input phase's watch opened no capture source" });
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
  return measureKpi(id, samples, notes.length ? { notes } : {});
}

/** Switch latency over the switch phase (focus arm, N > 1). */
export function switchLatencyKpi(p, notes) {
  const id = "switch_latency_ms";
  const sw = p.switches;
  if (p.arm === "tiled") return notApplicableKpi(id, "not_applicable_to_arm");
  if (p.n === 1) return notApplicableKpi(id, "single_session");
  if (!sw) return unknownKpi(id, "insufficient_samples", { n: 0, detail: "the switch phase did not run" });
  if (sw.unavailable) return unknownKpi(id, sw.unavailable.reason, sw.unavailable.detail ? { detail: sw.unavailable.detail } : {});
  const seg = indexProbeSegment(sw.records ?? []);
  const win = segmentWindow(seg);
  const cap = captureInfo(seg);
  if (!cap.primary || !win) return unknownKpi(id, "capture_unavailable", { detail: "the switch phase's watch captured nothing" });
  const frames = primaryFrames(seg, cap.primary, win);
  const timeoutNs = BigInt(Math.round((p.switchTimeoutMs ?? 5000) * 1e6));
  const samples = [];
  let missed = 0;
  for (const s of sw.list ?? []) {
    const g = p.gens.get(s.k);
    let newest = null;
    for (const m of g?.markers ?? []) if (m.kind === "output" && m.tEmit <= s.tCmdNs) newest = m.seq;
    const ms = timeToMarker(frames, s.k, s.tCmdNs, newest, timeoutNs);
    if (ms === null) missed += 1;
    else samples.push(ms);
  }
  if (missed > 0) notes.push(`switch_latency_ms: ${missed} switch(es) never showed the session within the timeout`);
  return measureKpi(id, samples);
}

/** Spawn latency over the spawn phase: command to the new session's first marker. */
export function spawnLatencyKpi(p, notes) {
  const id = "spawn_latency_ms";
  const sp = p.spawns;
  if (!sp) return unknownKpi(id, "insufficient_samples", { n: 0, detail: "the spawn phase did not run" });
  if (sp.unavailable) return unknownKpi(id, sp.unavailable.reason, sp.unavailable.detail ? { detail: sp.unavailable.detail } : {});
  const seg = indexProbeSegment(sp.records ?? []);
  const win = segmentWindow(seg);
  const cap = captureInfo(seg);
  if (!cap.primary || !win) return unknownKpi(id, "capture_unavailable", { detail: "the spawn phase's watch captured nothing" });
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
