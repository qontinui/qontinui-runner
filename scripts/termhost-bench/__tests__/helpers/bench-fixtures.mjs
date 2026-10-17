/**
 * Synthetic, mutually consistent probe (`termhost-probe/v1`) and generator
 * (`termhost-bench/gen/v1`) records for the orchestrator's tests: a generator log
 * whose markers advance at `markerHz`, and a `watch` segment whose frames show each
 * session's marker a known `latencyMs` after it was emitted — so every KPI has an
 * answer the test can state in advance.
 *
 * The segment ends in a `summary` shaped like the probe's (`records.rs`
 * `WatchSummary`), its KPI values computed here from the segment's own records with
 * the probe's rules for these simple cases (first appearance on the primary, the
 * window from the first frame, interval exclusion, the capture / lag / refresh
 * gates). The harness takes the soak's capture KPIs from that summary, so a test can
 * also hand it a summary the probe would write in a case the fixture cannot draw.
 */

import { getKpi } from "../../kpi-catalog.mjs";
import { SEQ_MODULUS } from "../../marker.mjs";
import { measureKpi, percentile, share, unknownKpi } from "../../stats.mjs";

export const MS = 1_000_000n;

/**
 * One generator's records.
 *
 * @param {{session: number, t0: bigint, durationMs: number, markerHz?: number,
 *   blockedNsPerWrite?: number, bytesPerWrite?: number, scheduledPerWrite?: number,
 *   echoes?: {seq: number, byte: number, tEmit: bigint}[], terminalSize?: object, exit?: boolean}} o
 */
export function genLog({
  session,
  t0,
  durationMs,
  markerHz = 10,
  blockedNsPerWrite = 0,
  bytesPerWrite = 2048,
  scheduledPerWrite = bytesPerWrite,
  echoes = [],
  terminalSize = { cols: 120, rows: 40 },
  exit = true,
}) {
  const recs = [
    {
      type: "start",
      schema: "termhost-bench/gen/v1",
      run_id: "fixture",
      session,
      mode: "load",
      profile: "claude-tui",
      t0_ns: String(t0),
      geometry: { cols: 120, rows: 40 },
      marker_hz: markerHz,
      echo: true,
      terminal_size: terminalSize,
    },
  ];
  const period = BigInt(Math.round(1e9 / markerHz));
  const end = t0 + BigInt(durationMs) * MS;
  let seq = 0;
  let blocked = 0;
  let written = 0;
  let scheduled = 0;
  let nextProgress = t0 + 500n * MS;
  for (let t = t0; t < end; t += period) {
    while (nextProgress <= t) {
      recs.push({ type: "progress", t_ns: String(nextProgress), blocked_ns: blocked, bytes_written: written, bytes_scheduled: scheduled, writes: seq, echo_bytes: 0, queued: 0, sched_pos_us: 0 });
      nextProgress += 500n * MS;
    }
    recs.push({ type: "marker", kind: "output", seq, phase: "idle", t_sched_ns: String(t), t_emit_ns: String(t), t_written_ns: String(t + BigInt(blockedNsPerWrite)), bytes: bytesPerWrite });
    blocked += blockedNsPerWrite;
    written += bytesPerWrite;
    scheduled += scheduledPerWrite;
    seq += 1;
  }
  for (const e of echoes) {
    recs.push({ type: "echo", seq: e.seq, byte: e.byte, t_read_ns: String(e.tEmit - MS), t_emit_ns: String(e.tEmit), t_written_ns: String(e.tEmit), bytes: 60 });
  }
  if (exit) {
    recs.push({ type: "exit", reason: "stop_file", t_ns: String(end + 200n * MS), blocked_ns: blocked, bytes_written: written, bytes_scheduled: scheduled, writes: seq });
  }
  return recs;
}

/**
 * One `watch` segment over `[t0, t0 + durationMs]`.
 *
 * @param {{
 *   t0: bigint, durationMs: number, primary?: string, refreshHz?: number, refreshSource?: string,
 *   refreshAssumed?: boolean,
 *   sessions?: {session: number, gen: object[], latencyMs: number, rect?: object, hidden?: boolean}[],
 *   frameHz?: number, bands?: boolean, intervalClass?: (i: number) => string | null,
 *   hostPids?: number[], cpuPct?: number, memMb?: number, gpu?: string, hostExits?: object[],
 *   crosscheck?: string, captureFailed?: boolean, echoSeen?: {session: number, seq: number, t: bigint}[],
 *   behind?: boolean | null, hangs?: number, groupNote?: (group: string, k: number) => string | null,
 * }} o
 */
export function watchSegment({
  t0,
  durationMs,
  primary = "x11",
  refreshHz = 60,
  refreshSource = "RandR screen info",
  refreshAssumed = false,
  sessions = [],
  frameHz = 60,
  bands = false,
  intervalClass = () => null,
  hostPids = [100, 101],
  cpuPct = 25,
  memMb = 300,
  gpu = "unsupported: no PDH on Linux",
  hostExits = [],
  crosscheck = "not_performed",
  captureFailed = false,
  echoSeen = [],
  behind = false,
  hangs = 0,
  groupNote = () => null,
}) {
  const t1 = t0 + BigInt(durationMs) * MS;
  const recs = [{ schema: "termhost-probe/v1", type: "start", subcommand: "watch", t0_ns: String(t0 - 5n * MS), tool: "termhost-probe" }];
  const capture = { type: "capture", source: primary, refresh_hz: refreshHz, refresh_source: refreshSource, refresh_assumed: refreshAssumed };
  recs.push(captureFailed ? { ...capture, state: "failed", detail: "no display" } : { ...capture, state: "started", detail: "fixture" });
  const firstSeen = new Map();
  const displayed = new Set();
  if (!captureFailed) {
    const period = BigInt(Math.round(1e9 / frameHz));
    let index = 0;
    const visible = sessions.filter((s) => !s.hidden);
    // Each session's markers in emission order, walked with a pointer as frames advance.
    const tracks = visible.map((s) => ({
      list: s.gen
        .filter((r) => r.type === "marker" && r.kind === "output")
        .map((r) => ({ t: BigInt(r.t_emit_ns), seq: r.seq }))
        .sort((a, b) => (a.t < b.t ? -1 : a.t > b.t ? 1 : 0)),
      at: -1,
    }));
    for (let t = t0 + period; t <= t1; t += period) {
      index += 1;
      const markers = [];
      visible.forEach((s, i) => {
        const shownAt = t - BigInt(Math.round(s.latencyMs * 1e6));
        const track = tracks[i];
        while (track.at + 1 < track.list.length && track.list[track.at + 1].t <= shownAt) track.at += 1;
        if (bands && index % visible.length !== i) return; // one damaged band per frame
        const cur = track.at >= 0 ? track.list[track.at] : null;
        if (!cur) return;
        const seq = cur.seq % SEQ_MODULUS;
        markers.push({ kind: "output", session: s.session, seq, rect: s.rect ?? { x: 10 + 400 * i, y: 40, w: 160, h: 16 } });
        const key = `output:${s.session}:${seq}`;
        if (!firstSeen.has(key)) firstSeen.set(key, { t, session: s.session, seq, frame: index });
      });
      for (const e of echoSeen) {
        if (e.t <= t && e.t > t - period) {
          markers.push({ kind: "echo", session: e.session, seq: e.seq % SEQ_MODULUS, rect: { x: 10, y: 56, w: 160, h: 16 } });
          const key = `echo:${e.session}:${e.seq % SEQ_MODULUS}`;
          if (!firstSeen.has(key)) firstSeen.set(key, { t, session: e.session, seq: e.seq % SEQ_MODULUS, frame: index, kind: "echo" });
        }
      }
      for (const m of markers) if (m.kind === "output") displayed.add(m.session);
      // `v` accumulates across partial (band) reads, as the probe's does.
      recs.push({ type: "frame", source: primary, index, t_ns: String(t), w: 1920, h: 1080, missed: null, v: displayed.size, markers, rejected: 0, scan_ms: 0.4, lag_ms: behind === null ? null : behind ? 250 : 2 });
    }
    for (const [key, fs] of firstSeen) {
      recs.push({ type: "first_seen", source: primary, kind: key.split(":")[0], session: fs.session, seq: fs.seq, t_ns: String(fs.t), frame: fs.frame, rect: { x: 0, y: 0, w: 1, h: 1 } });
    }
  }
  const full = Math.floor(durationMs / 500);
  for (let i = 0; i < full; i += 1) {
    const cls = intervalClass(i);
    recs.push({
      type: "interval",
      index: i,
      t_start_ns: String(t0 + BigInt(i) * 500n * MS),
      t_end_ns: String(t0 + BigInt(i + 1) * 500n * MS),
      partial: false,
      valid: cls === null,
      exclusion_class: cls,
    });
  }
  recs.push({ type: "interval", index: full, t_start_ns: String(t0 + BigInt(full) * 500n * MS), t_end_ns: String(t1), partial: true, valid: false, exclusion_class: null });
  // Process samples. A group the probe could not read whole carries null values
  // and a note (`no_live_process`, `unreadable_members`); there is no host group
  // without host roots.
  const groupAcc = {};
  const group = (name, k, first, pids, cpu, mem) => {
    const note = groupNote(name, k);
    const acc = (groupAcc[name] ??= { cpu: [], memEnd: null, samples: 0, empty: 0, unreadable: 0 });
    acc.samples += 1;
    if (note) {
      if (note === "no_live_process") acc.empty += 1;
      else acc.unreadable += 1;
      acc.memEnd = null; // the probe's memory_mb_end: the LAST sample, whole or null
      return { pids: note === "no_live_process" ? [] : pids, cpu_pct: null, cpu_ms: null, memory_mb: null, rss_mb: null, gpu_pct: null, gpu_by_engine: {}, unreadable: note === "unreadable_members" ? 1 : 0, note };
    }
    if (!first) acc.cpu.push(cpu);
    acc.memEnd = mem;
    return { pids, cpu_pct: first ? null : cpu, cpu_ms: first ? null : (cpu / 100) * 500, memory_mb: mem, rss_mb: mem, gpu_pct: null, gpu_by_engine: {}, unreadable: 0 };
  };
  let crashes = 0;
  for (let t = t0, k = 0; t <= t1; t += 500n * MS, k += 1) {
    const first = k === 0;
    const groups = {};
    if (hostPids.length > 0) groups.host = group("host", k, first, hostPids, cpuPct + (k % 3), memMb + k * 0.1);
    groups.workload = group("workload", k, first, [200, 201], 40, 90);
    groups.compositor = group("compositor", k, first, [50], 5, 40);
    const exited = hostPids.length > 0 ? hostExits.filter((e) => e.k === k).map((e) => ({ pid: e.pid, group: "host", name: e.name ?? "x" })) : [];
    crashes += exited.length;
    recs.push({ type: "sample", t_ns: String(t), dt_ms: first ? null : 500, gpu, groups, exited });
  }
  recs.push({
    type: "summary",
    ...probeSummary({
      t0,
      t1,
      primary,
      recs,
      sessions,
      firstSeen,
      refreshMs: 1000 / refreshHz,
      refreshAssumed,
      captureFailed,
      crosscheck,
      behind,
      hangs,
      groupAcc,
      crashes: hostPids.length > 0 ? crashes : null,
    }),
  });
  recs.push({ type: "end", t_ns: String(t1 + MS), exit_code: 0, reason: "stdin closed" });
  return recs;
}

/**
 * The summary the probe would write for a fixture segment (`model/mod.rs`
 * `finish`, for the cases a fixture draws): the capture KPIs, V, the sessions
 * seen, interval counts, process groups and the probe's own health.
 */
function probeSummary({ t0, t1, primary, recs, sessions, firstSeen, refreshMs, refreshAssumed, captureFailed, crosscheck, behind, hangs, groupAcc, crashes }) {
  const frames = recs.filter((r) => r.type === "frame" && r.source === primary);
  const deltas = [];
  for (let i = 1; i < frames.length; i += 1) deltas.push(Number(BigInt(frames[i].t_ns) - BigInt(frames[i - 1].t_ns)) / 1e6);
  const firstFrame = frames.length > 0 ? BigInt(frames[0].t_ns) : null;
  const intervals = recs.filter((r) => r.type === "interval" && !r.partial);
  const tally = { valid: 0, occluded: 0, operator: 0, harness: 0 };
  for (const r of intervals) {
    if (r.valid) tally.valid += 1;
    else if (r.exclusion_class === "occluded") tally.occluded += 1;
    else if (r.exclusion_class === "operator_activity") tally.operator += 1;
    else tally.harness += 1;
  }
  const excludedCount = tally.occluded + tally.operator + tally.harness;
  const exclusionReason = (valid, excluded, minN) => {
    if (excluded === 0 || valid + excluded < minN) return "insufficient_samples";
    const classes = [["occluded", tally.occluded], ["operator_activity", tally.operator], ["harness_error", tally.harness]];
    return classes.sort((a, b) => b[1] - a[1])[0][0];
  };
  const intervalValidAt = (t) => {
    const i = Number((t - t0) / (500n * MS));
    const r = intervals.find((x) => x.index === i);
    return Boolean(r?.valid);
  };
  // Latency and loss over the markers emitted from the first frame on.
  const samples = [];
  let counted = 0;
  let lost = 0;
  let excludedMarkers = 0;
  for (const s of sessions.filter((x) => !x.hidden)) {
    for (const r of s.gen) {
      if (r.type !== "marker" || r.kind !== "output") continue;
      const tEmit = BigInt(r.t_emit_ns);
      if (firstFrame === null || tEmit < firstFrame || tEmit > t1) continue;
      const fs = firstSeen.get(`output:${s.session}:${r.seq % SEQ_MODULUS}`);
      if (fs && fs.t >= tEmit) samples.push(Number(fs.t - tEmit) / 1e6);
      if (!intervalValidAt(tEmit)) {
        excludedMarkers += 1;
        continue;
      }
      counted += 1;
      if (!fs) lost += 1;
    }
  }
  let blocked = null;
  if (captureFailed) blocked = ["capture_unavailable", "no capture source able to be primary was opened"];
  else if (behind) blocked = ["harness_error", "the probe fell behind: frames reached it up to 250.0 ms after presentation (bound 100.0 ms)"];
  else if (behind === null && frames.length > 0) blocked = ["harness_error", `only 0 of ${frames.length} primary frames had a known presentation time (a verdict needs 10), so the probe cannot show it kept up with them`];
  const gate = (id, refreshDependent, value) => {
    if (blocked) return unknownKpi(id, blocked[0], { n: value.n ?? 0, detail: blocked[1] });
    if (refreshDependent && refreshAssumed) return unknownKpi(id, "refresh_rate_unknown", { n: value.n ?? 0, detail: "the display reported no refresh rate (16.67 ms assumed)" });
    return value;
  };
  const shareKpi = (id, hits, n) =>
    n >= getKpi(id).min_n ? measureKpi(id, { value: (100 * hits) / n, n }) : unknownKpi(id, exclusionReason(n, excludedMarkers, getKpi(id).min_n), { n });
  let output = measureKpi("output_latency_ms", samples);
  if (crosscheck === "disagree" && samples.length > 0) output = unknownKpi("output_latency_ms", "capture_disagreement", { n: samples.length, detail: "3 of 40 markers disagreed" });
  const minValid = getKpi("hangs").min_n;
  const hangsKpi = captureFailed
    ? unknownKpi("hangs", "capture_unavailable", { n: tally.valid, detail: "no primary capture source" })
    : behind === null && frames.length > 0
      ? unknownKpi("hangs", "harness_error", { n: tally.valid, detail: blocked[1] })
      : tally.valid >= minValid
      ? measureKpi("hangs", { value: hangs, n: tally.valid })
      : unknownKpi("hangs", exclusionReason(tally.valid, excludedCount, minValid), { n: tally.valid });
  const vs = frames.map((f) => f.v).sort((a, b) => a - b);
  const v50 = vs.length > 0 ? percentile(vs, 50) : null;
  const processSummary = (acc) => {
    const noneWhole = acc.samples > 0 && acc.empty + acc.unreadable === acc.samples;
    const cpu = acc.cpu.length === 0 && noneWhole
      ? unknownKpi("cpu_pct", "harness_error", { n: 0, detail: `no sample of ${acc.samples} was readable whole (${acc.empty} empty, ${acc.unreadable} with unreadable members)` })
      : measureKpi("cpu_pct", acc.cpu);
    return {
      cpu_pct: cpu,
      gpu_pct: unknownKpi("gpu_pct", "gpu_counters_unavailable", { n: 0 }),
      memory_mb_end: acc.memEnd,
      samples: acc.samples,
      samples_empty: acc.empty,
      samples_unreadable: acc.unreadable,
    };
  };
  return {
    t_start_ns: String(t0),
    t_end_ns: String(t1),
    primary,
    secondary: null,
    refresh_ms: refreshMs,
    refresh_assumed: refreshAssumed,
    frames: { [primary]: frames.length },
    frame_interval_ms: gate("frame_interval_ms", false, measureKpi("frame_interval_ms", deltas)),
    jank_share: gate("jank_share", true, measureKpi("jank_share", share(deltas, (d) => d > 2 * refreshMs, { minN: 100 }))),
    v_actual_max: vs.length > 0 ? vs[vs.length - 1] : null,
    v_actual_p50: v50 && v50.state === "measured" ? v50.value : vs.length > 0 ? vs[0] : null,
    sessions_seen: [...new Set(frames.flatMap((f) => f.markers.filter((m) => m.kind === "output").map((m) => m.session)))].sort((a, b) => a - b),
    intervals_total: intervals.length,
    intervals_valid: tally.valid,
    intervals_excluded_occluded: tally.occluded,
    intervals_excluded_operator_activity: tally.operator,
    intervals_excluded_harness_error: tally.harness,
    hangs: hangsKpi,
    hang_episodes_excluded: 0,
    emitted_markers: counted + excludedMarkers,
    output_latency_ms: gate("output_latency_ms", false, output),
    echo_latency_ms: unknownKpi("output_latency_ms", "insufficient_samples", { n: 0 }),
    input_latency_ms: unknownKpi("input_latency_ms", "not_instrumented", { n: 0, detail: "watch receives no injection times" }),
    marker_loss: gate("marker_loss", true, shareKpi("marker_loss", lost, counted)),
    marker_skipped: gate("marker_skipped", true, shareKpi("marker_skipped", 0, counted)),
    crosscheck: { verdict: crosscheck, compared: 0, agreed: 0, disagreed: 0, detail: crosscheck === "disagree" ? "3 of 40 markers disagreed" : "n/a" },
    negative_latencies: 0,
    crashes,
    processes: Object.fromEntries(Object.entries(groupAcc).map(([name, acc]) => [name, processSummary(acc)])),
    probe: {
      frames: frames.length,
      frames_lag_unknown: behind === null ? frames.length : 0,
      scan_ms_p50: frames.length ? 0.4 : null,
      scan_ms_max: frames.length ? 0.4 : null,
      lag_ms_p50: frames.length && behind !== null ? (behind ? 250 : 2) : null,
      lag_ms_judged: frames.length && behind !== null ? (behind ? 250 : 2) : null,
      lag_ms_max: frames.length && behind !== null ? (behind ? 250 : 2) : null,
      lag_bound_ms: 100,
      // The probe's floor: no verdict below 10 frames with a known lag.
      lag_statistic: frames.length >= 10 && behind !== null ? (frames.length >= 20 ? "p95" : "second_largest") : null,
      frames_over_bound: behind === true ? frames.length : 0,
      behind: frames.length ? behind : null,
    },
  };
}

/** Doctor capabilities with everything OPERATIVE unless overridden. */
export function operativeCapabilities(overrides = {}) {
  const ids = ["session.interactive", "capture.x11", "input.safe", "clock.aligned", "counters.gpu", "probe.binary", "env.platform"];
  const caps = Object.fromEntries(ids.map((id) => [id, { state: "OPERATIVE", reason: "fixture", checked_at: "2026-10-12T00:00:00Z" }]));
  caps["counters.gpu"] = { state: "INOPERATIVE-ON-THIS-MACHINE", reason: "Linux", checked_at: "2026-10-12T00:00:00Z" };
  return { ...caps, ...overrides };
}

/**
 * A watch segment whose pane stopped repainting, as CI run 38351621760 recorded
 * it: every frame still shows each session's output marker, but frozen at the
 * first frame's sequence; one first appearance per session; and the probe's own
 * output latency is then unknown(insufficient_samples).
 */
export function stallSegment(records) {
  const frozen = new Map();
  const out = [];
  const firstKept = new Set();
  for (const r of records) {
    if (r.type === "frame") {
      const markers = (r.markers ?? []).map((m) => {
        if (m.kind !== "output") return m;
        if (!frozen.has(m.session)) frozen.set(m.session, m.seq);
        return { ...m, seq: frozen.get(m.session) };
      });
      out.push({ ...r, markers });
    } else if (r.type === "first_seen") {
      if (r.kind === "output" && !firstKept.has(r.session)) {
        firstKept.add(r.session);
        out.push(r);
      }
    } else if (r.type === "summary") {
      out.push({ ...r, output_latency_ms: { state: "unknown", unit: "ms", n: 0, reason: "insufficient_samples" } });
    } else {
      out.push(r);
    }
  }
  return out;
}
