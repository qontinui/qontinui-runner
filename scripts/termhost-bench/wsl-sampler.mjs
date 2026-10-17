#!/usr/bin/env node
/**
 * The WSL-side process sampler of the `tmux-wsl` setup (plan
 * `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 5): the probe
 * runs on Windows and cannot see processes inside the WSL VM, so this script
 * runs INSIDE the distro (`wsl.exe -d <distro> --exec node wsl-sampler.mjs …`),
 * reads `/proc` every interval, and writes one JSON line per sample:
 *
 *   {type: "sample", t_ns, dt_ms, host: {pids, cpu_ms, cpu_pct, pss_mb, unreadable},
 *    workload: {…same…}}
 *
 * host      the private tmux server (`--server-pid`, start key re-checked every
 *           sample), its descendants that are not workload, and every tmux
 *           client carrying `-L <socket>` — what hosts the sessions;
 * workload  every generator carrying `--termhost-bench-generator <run-id>` and
 *           its parent session shell (D13: identical across setups, so excluded
 *           from the host, as the probe excludes them on Windows).
 *
 * `t_ns` is the Linux CLOCK_MONOTONIC (`process.hrtime`) shifted by
 * `--clock-offset-ns` into the Windows timebase the probe and the harness read,
 * so the harness windows the samples to the soak exactly as it windows the
 * probe's. Read-only: it signals nothing. It exits on `--stop-file`, on stdin
 * EOF, or when the server is gone.
 */

import { closeSync, existsSync, openSync, readFileSync, writeSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { parseProcStat, readLinuxProcessTable } from "./proc.mjs";

export const GENERATOR_FLAG = "--termhost-bench-generator";

/**
 * utime + stime (clock ticks) from one `/proc/<pid>/stat` line: fields 14 and 15,
 * read after the LAST `)` as `parseProcStat` does.
 *
 * @returns {number | null}
 */
export function cpuTicks(statText) {
  const close = String(statText).lastIndexOf(")");
  if (close < 0) return null;
  const rest = String(statText)
    .slice(close + 1)
    .trim()
    .split(/\s+/);
  // rest[0] is field 3; utime is field 14, stime field 15.
  const u = Number(rest[11]);
  const s = Number(rest[12]);
  return Number.isFinite(u) && Number.isFinite(s) ? u + s : null;
}

/** Pss (kB) from `/proc/<pid>/smaps_rollup`, or null. */
export function pssKb(text) {
  const m = /^Pss:\s+(\d+)\s+kB/m.exec(String(text ?? ""));
  return m ? Number(m[1]) : null;
}

/** True when argv carries `flag value` as adjacent arguments. */
function carries(argv, flag, value) {
  const i = argv.indexOf(flag);
  return i >= 0 && argv[i + 1] === value;
}

/**
 * Split the process table into host and workload PIDs.
 *
 * @param {Map<number, {pid: number, ppid: number, argv: string[] | null, startKey: string}>} table
 * @param {{serverPid: number, serverStartKey?: string | null, socket: string, runId: string}} o
 * @returns {{host: number[], workload: number[], serverAlive: boolean}}
 */
export function classify(table, { serverPid, serverStartKey = null, socket, runId }) {
  const server = table.get(serverPid);
  const serverAlive = Boolean(server) && (serverStartKey === null || server.startKey === serverStartKey);
  const workload = new Set();
  for (const p of table.values()) {
    if (Array.isArray(p.argv) && carries(p.argv, GENERATOR_FLAG, runId)) {
      workload.add(p.pid);
      if (p.ppid > 1 && table.get(p.ppid)?.ppid === serverPid) workload.add(p.ppid);
    }
  }
  const host = new Set();
  if (serverAlive) {
    host.add(serverPid);
    const queue = [serverPid];
    while (queue.length > 0) {
      const cur = queue.shift();
      for (const p of table.values()) {
        if (p.ppid !== cur || host.has(p.pid) || workload.has(p.pid)) continue;
        if (BigInt(p.startKey || "0") < BigInt(server.startKey || "0")) continue;
        host.add(p.pid);
        queue.push(p.pid);
      }
    }
  }
  for (const p of table.values()) {
    if (p.pid === serverPid || workload.has(p.pid)) continue;
    if (Array.isArray(p.argv) && /(^|\/)tmux(: client)?$/.test(p.argv[0] ?? "") && carries(p.argv, "-L", socket)) host.add(p.pid);
  }
  return { host: [...host].sort((a, b) => a - b), workload: [...workload].sort((a, b) => a - b), serverAlive };
}

/**
 * One group's reading over an interval, as `sampler.rs` reads its own: CPU from
 * the tick deltas of processes seen in BOTH samples, keyed by (pid, start time) —
 * a process seen for the first time contributes nothing (its lifetime CPU is not
 * this interval's), a reused PID is a new process, and each delta is clamped at
 * 0 on its own — and PSS summed now.
 *
 * @param {number[]} pids
 * @param {Map<string | number, number>} prevTicks ticks by `keyOf(pid)` at the previous sample
 * @param {Map<string | number, number>} nowTicks ticks by `keyOf(pid)` now
 * @param {Map<number, number | null>} pss kB by pid
 * @param {{dtMs: number | null, clkTck: number, keyOf?: (pid: number) => string | number}} o
 *   `keyOf`: the process identity, `${pid}:${startKey}` in the sampler (default: the pid)
 */
export function groupReading(pids, prevTicks, nowTicks, pss, { dtMs, clkTck, keyOf = (pid) => pid }) {
  let ticks = 0;
  let unreadable = 0;
  let pssTotal = 0;
  for (const pid of pids) {
    const key = keyOf(pid);
    const t = nowTicks.get(key);
    if (t === undefined || t === null) {
      unreadable += 1;
      continue;
    }
    const before = prevTicks.get(key);
    if (typeof before === "number") ticks += Math.max(0, t - before);
    const kb = pss.get(pid);
    if (kb === null || kb === undefined) unreadable += 1;
    else pssTotal += kb;
  }
  const cpuMs = dtMs === null ? null : (ticks * 1000) / clkTck;
  return {
    pids,
    cpu_ms: unreadable > 0 ? null : cpuMs,
    cpu_pct: unreadable > 0 || cpuMs === null || !(dtMs > 0) ? null : (cpuMs / dtMs) * 100,
    pss_mb: unreadable > 0 ? null : pssTotal / 1024,
    unreadable,
  };
}

export function parseArgs(argv) {
  const o = { intervalMs: 500, clkTck: 100, clockOffsetNs: 0n, serverStartKey: null };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    const next = () => {
      if (i + 1 >= argv.length) throw new Error(`${a} needs a value`);
      return argv[++i];
    };
    if (a === "--server-pid") o.serverPid = Number(next());
    else if (a === "--server-start-key") o.serverStartKey = next();
    else if (a === "--socket") o.socket = next();
    else if (a === "--run-id") o.runId = next();
    else if (a === "--out") o.out = next();
    else if (a === "--stop-file") o.stopFile = next();
    else if (a === "--interval-ms") o.intervalMs = Number(next());
    else if (a === "--clk-tck") o.clkTck = Number(next());
    else if (a === "--clock-offset-ns") o.clockOffsetNs = BigInt(next());
    else throw new Error(`unknown argument ${a}`);
  }
  for (const k of ["serverPid", "socket", "runId", "out", "stopFile"]) if (o[k] === undefined) throw new Error(`--${k.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`)} is required`);
  if (!Number.isInteger(o.serverPid) || o.serverPid <= 1) throw new Error(`--server-pid must be a PID, got ${o.serverPid}`);
  if (!(o.clkTck > 0) || !(o.intervalMs >= 50)) throw new Error("--clk-tck must be positive and --interval-ms at least 50");
  return o;
}

/**
 * One `/proc/<pid>/stat` read's identity key and ticks: the start time and the
 * CPU come from the SAME read, so a PID reused between two reads can never pair
 * one process's start time with another's ticks.
 *
 * @returns {{key: string, ticks: number | null} | null}
 */
export function statReading(pid, text) {
  const st = parseProcStat(String(text));
  if (!st) return null;
  return { key: `${pid}:${st.starttime}`, ticks: cpuTicks(text) };
}

/**
 * Read ticks (by identity key) and PSS (by pid) for `pids` from `/proc`;
 * `keys` maps each pid to the identity its stat read gave.
 */
export function readReadings(pids, procRoot, { read = (p) => readFileSync(p, "utf8") } = {}) {
  const ticks = new Map();
  const pss = new Map();
  const keys = new Map();
  for (const pid of pids) {
    let r = null;
    try {
      r = statReading(pid, read(`${procRoot}/${pid}/stat`));
    } catch {
      r = null;
    }
    const key = r?.key ?? `${pid}:unreadable`;
    keys.set(pid, key);
    ticks.set(key, r?.ticks ?? null);
    try {
      pss.set(pid, pssKb(read(`${procRoot}/${pid}/smaps_rollup`)));
    } catch {
      pss.set(pid, null);
    }
  }
  return { ticks, pss, keys };
}

async function main(argv) {
  const o = parseArgs(argv);
  const fd = openSync(o.out, "a");
  const line = (rec) => writeSync(fd, `${JSON.stringify(rec)}\n`);
  let stdinEnded = false;
  process.stdin.on("end", () => (stdinEnded = true));
  process.stdin.on("error", () => (stdinEnded = true));
  process.stdin.resume();
  const now = () => process.hrtime.bigint() + o.clockOffsetNs;
  let serverStartKey = o.serverStartKey;
  if (serverStartKey === null) {
    try {
      serverStartKey = parseProcStat(readFileSync(`/proc/${o.serverPid}/stat`, "utf8"))?.starttime ?? null;
    } catch {
      serverStartKey = null;
    }
  }
  line({ type: "start", schema: "termhost-bench/wsl-sampler/v1", server_pid: o.serverPid, socket: o.socket, run_id: o.runId, clk_tck: o.clkTck, interval_ms: o.intervalMs, clock_offset_ns: String(o.clockOffsetNs), t_ns: String(now()) });
  let prev = null;
  let reason = "stop_file";
  for (;;) {
    if (existsSync(o.stopFile)) break;
    if (stdinEnded) {
      reason = "stdin_eof";
      break;
    }
    const t = now();
    let table;
    try {
      table = readLinuxProcessTable("/proc");
    } catch (e) {
      line({ type: "error", t_ns: String(t), detail: e.message });
      await new Promise((r) => setTimeout(r, o.intervalMs));
      continue;
    }
    const c = classify(table, { serverPid: o.serverPid, serverStartKey, socket: o.socket, runId: o.runId });
    if (!c.serverAlive) {
      reason = "server_gone";
      break;
    }
    const all = [...c.host, ...c.workload];
    const r = readReadings(all, "/proc");
    const keyOf = (pid) => r.keys.get(pid);
    const dtMs = prev ? Number(t - prev.t) / 1e6 : null;
    const prevTicks = prev?.ticks ?? new Map();
    line({
      type: "sample",
      t_ns: String(t),
      dt_ms: dtMs,
      host: groupReading(c.host, prevTicks, r.ticks, r.pss, { dtMs, clkTck: o.clkTck, keyOf }),
      workload: groupReading(c.workload, prevTicks, r.ticks, r.pss, { dtMs, clkTck: o.clkTck, keyOf }),
    });
    prev = { t, ticks: r.ticks };
    await new Promise((res) => setTimeout(res, o.intervalMs));
  }
  line({ type: "end", t_ns: String(now()), reason });
  closeSync(fd);
  process.stdin.pause();
  return 0;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main(process.argv.slice(2)).then(
    (code) => process.exit(code),
    (e) => {
      process.stderr.write(`wsl-sampler: ${e.message}\n`);
      process.exit(2);
    },
  );
}
