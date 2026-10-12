/**
 * Synthetic, mutually consistent probe (`termhost-probe/v1`) and generator
 * (`termhost-bench/gen/v1`) records for the orchestrator's tests: a generator log
 * whose markers advance at `markerHz`, and a `watch` segment whose frames show each
 * session's marker a known `latencyMs` after it was emitted — so every KPI the
 * harness computes from them has an answer the test can state in advance.
 */

import { SEQ_MODULUS } from "../../marker.mjs";

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
 *   t0: bigint, durationMs: number, primary?: string, refreshHz?: number | null, refreshSource?: string,
 *   sessions?: {session: number, gen: object[], latencyMs: number, rect?: object, hidden?: boolean}[],
 *   frameHz?: number, bands?: boolean, intervalClass?: (i: number) => string | null,
 *   hostPids?: number[], cpuPct?: number, memMb?: number, gpu?: string, hostExits?: object[],
 *   crosscheck?: string, captureFailed?: boolean, echoSeen?: {session: number, seq: number, t: bigint}[],
 * }} o
 */
export function watchSegment({
  t0,
  durationMs,
  primary = "x11",
  refreshHz = 60,
  refreshSource = "RandR screen info",
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
}) {
  const t1 = t0 + BigInt(durationMs) * MS;
  const recs = [{ schema: "termhost-probe/v1", type: "start", subcommand: "watch", t0_ns: String(t0 - 5n * MS), tool: "termhost-probe" }];
  recs.push(
    captureFailed
      ? { type: "capture", source: primary, state: "failed", detail: "no display", refresh_hz: refreshHz, refresh_source: refreshSource }
      : { type: "capture", source: primary, state: "started", detail: "fixture", refresh_hz: refreshHz, refresh_source: refreshSource },
  );
  const firstSeen = new Map();
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
      const v = new Set(markers.filter((m) => m.kind === "output").map((m) => m.session)).size;
      recs.push({ type: "frame", source: primary, index, t_ns: String(t), w: 1920, h: 1080, missed: null, v, markers, rejected: 0 });
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
  for (let t = t0, k = 0; t <= t1; t += 500n * MS, k += 1) {
    const first = k === 0;
    recs.push({
      type: "sample",
      t_ns: String(t),
      dt_ms: first ? null : 500,
      gpu,
      groups: {
        host: { pids: hostPids, cpu_pct: first ? null : cpuPct + (k % 3), cpu_ms: first ? null : ((cpuPct + (k % 3)) / 100) * 500, memory_mb: memMb + k * 0.1, rss_mb: memMb, gpu_pct: null, gpu_by_engine: {}, unreadable: 0 },
        workload: { pids: [200, 201], cpu_pct: first ? null : 40, cpu_ms: first ? null : 200, memory_mb: 90, rss_mb: 90, gpu_pct: null, gpu_by_engine: {}, unreadable: 0 },
        compositor: { pids: [50], cpu_pct: first ? null : 5, cpu_ms: first ? null : 25, memory_mb: 40, rss_mb: 40, gpu_pct: null, gpu_by_engine: {}, unreadable: 0 },
      },
      exited: hostExits.filter((e) => e.k === k).map((e) => ({ pid: e.pid, group: "host", name: e.name ?? "x" })),
    });
  }
  recs.push({ type: "summary", t_start_ns: String(t0), t_end_ns: String(t1), primary, secondary: null, crosscheck: { verdict: crosscheck, detail: crosscheck === "disagree" ? "3 of 40 markers disagreed" : "n/a" } });
  recs.push({ type: "end", t_ns: String(t1 + MS), exit_code: 0, reason: "stdin closed" });
  return recs;
}

/** Doctor capabilities with everything OPERATIVE unless overridden. */
export function operativeCapabilities(overrides = {}) {
  const ids = ["session.interactive", "capture.x11", "input.safe", "clock.aligned", "counters.gpu", "probe.binary", "env.platform"];
  const caps = Object.fromEntries(ids.map((id) => [id, { state: "OPERATIVE", reason: "fixture", checked_at: "2026-10-12T00:00:00Z" }]));
  caps["counters.gpu"] = { state: "INOPERATIVE-ON-THIS-MACHINE", reason: "Linux", checked_at: "2026-10-12T00:00:00Z" };
  return { ...caps, ...overrides };
}
