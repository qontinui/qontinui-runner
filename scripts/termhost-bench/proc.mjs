/**
 * termhost-bench process inspection and PID-scoped termination.
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 3, D5 and
 * D12: teardown acts ONLY on PIDs this run proved it started, and never terminates
 * a process by image name (served policy `production-and-cost` `runner-lifecycle`;
 * `operating-rules` `what-makes-an-action-destructive`). A name-based kill on a
 * workstation reaches the operator's own runner, editor and agent sessions.
 *
 * This module is the ONLY place in termhost-bench that sends a terminating signal,
 * and its one door, {@link terminatePid}, takes a numeric PID and an identity
 * predicate checked against the CURRENT process table immediately before the
 * signal, so a recycled PID is never signalled. There is no name-based entry point
 * at all; `__tests__/no-image-kill.test.mjs` fails if one appears anywhere in the
 * harness. `process.kill(pid, 0)` (an existence probe that delivers nothing) is
 * the only other use of `process.kill`.
 *
 * Process tables are read, never mutated: `/proc` on Linux, `Win32_Process` through
 * Windows PowerShell on Windows.
 */

import { spawnSync } from "node:child_process";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

/**
 * @typedef {{pid: number, ppid: number, name: string, argv: string[] | null,
 *   cmdline: string, startKey: string}} ProcInfo
 *   `argv` is null where the OS gives only a command-line string (Windows);
 *   `startKey` orders process start times on one machine (Linux: clock ticks since
 *   boot; Windows: FILETIME UTC) and is what makes a PID reuse detectable.
 */

/**
 * Parse one `/proc/<pid>/stat` line. The command name is parenthesised and may
 * itself contain spaces and parentheses, so fields are read after the LAST `)`.
 *
 * @param {string} text
 * @returns {{pid: number, comm: string, ppid: number, starttime: string} | null}
 */
export function parseProcStat(text) {
  const open = text.indexOf("(");
  const close = text.lastIndexOf(")");
  if (open < 0 || close < open) return null;
  const pid = Number.parseInt(text.slice(0, open).trim(), 10);
  const comm = text.slice(open + 1, close);
  const rest = text
    .slice(close + 1)
    .trim()
    .split(/\s+/);
  // rest[0] is field 3 (state); ppid is field 4, starttime field 22.
  const ppid = Number.parseInt(rest[1], 10);
  const starttime = rest[19];
  if (!Number.isInteger(pid) || !Number.isInteger(ppid) || starttime === undefined) return null;
  return { pid, comm, ppid, starttime };
}

/**
 * The Linux process table from `procRoot` (`/proc`, or a fixture tree in tests).
 * A process that exits mid-read is skipped, never reported half-read.
 *
 * @param {string} [procRoot]
 * @returns {Map<number, ProcInfo>}
 */
export function readLinuxProcessTable(procRoot = "/proc") {
  const table = new Map();
  let entries;
  try {
    entries = readdirSync(procRoot);
  } catch (e) {
    throw new Error(`cannot read ${procRoot}: ${e.message}`);
  }
  for (const entry of entries) {
    if (!/^\d+$/.test(entry)) continue;
    let stat;
    let cmd;
    try {
      stat = parseProcStat(readFileSync(join(procRoot, entry, "stat"), "utf8"));
      cmd = readFileSync(join(procRoot, entry, "cmdline"));
    } catch {
      continue;
    }
    if (!stat) continue;
    const argv = cmd
      .toString("utf8")
      .split("\0")
      .filter((a, i, all) => !(a === "" && i === all.length - 1));
    table.set(stat.pid, {
      pid: stat.pid,
      ppid: stat.ppid,
      name: stat.comm,
      argv,
      cmdline: argv.join(" "),
      startKey: stat.starttime,
    });
  }
  return table;
}

/** The PowerShell that lists `Win32_Process` as compact JSON (read-only). */
export const WINDOWS_PROCESS_QUERY =
  "$ErrorActionPreference='Stop'; " +
  "Get-CimInstance -ClassName Win32_Process | ForEach-Object { " +
  "[pscustomobject]@{ p=[int]$_.ProcessId; pp=[int]$_.ParentProcessId; n=[string]$_.Name; " +
  "c=[string]$_.CommandLine; t=$(if ($_.CreationDate) { [string]$_.CreationDate.ToFileTimeUtc() } else { '0' }) } " +
  "} | ConvertTo-Json -Compress -Depth 2";

/**
 * The Windows process table, through Windows PowerShell (always present).
 *
 * @param {{run?: typeof spawnSync}} [deps]
 * @returns {Map<number, ProcInfo>}
 */
export function readWindowsProcessTable({ run = spawnSync } = {}) {
  const res = run(
    "powershell.exe",
    ["-NoProfile", "-NonInteractive", "-Command", WINDOWS_PROCESS_QUERY],
    { encoding: "utf8", maxBuffer: 256 * 1024 * 1024, windowsHide: true },
  );
  if (res.error) throw new Error(`process query failed: ${res.error.message}`);
  if (res.status !== 0) {
    throw new Error(`process query exited ${res.status}: ${String(res.stderr).slice(0, 400)}`);
  }
  return parseWindowsProcessJson(res.stdout);
}

/**
 * @param {string} text the query's stdout
 * @returns {Map<number, ProcInfo>}
 */
export function parseWindowsProcessJson(text) {
  const parsed = JSON.parse(String(text).trim() || "[]");
  const rows = Array.isArray(parsed) ? parsed : [parsed];
  const table = new Map();
  for (const r of rows) {
    if (!Number.isInteger(r?.p)) continue;
    table.set(r.p, {
      pid: r.p,
      ppid: Number.isInteger(r.pp) ? r.pp : 0,
      name: r.n ?? "",
      argv: null,
      cmdline: r.c ?? "",
      startKey: String(r.t ?? "0"),
    });
  }
  return table;
}

/**
 * The current process table.
 *
 * @param {{platform?: string, procRoot?: string, run?: typeof spawnSync}} [opts]
 * @returns {Map<number, ProcInfo>}
 */
export function listProcesses({ platform = process.platform, procRoot, run } = {}) {
  if (platform === "linux") return readLinuxProcessTable(procRoot);
  if (platform === "win32") return readWindowsProcessTable({ run });
  throw new Error(`process inspection is not implemented for ${platform}`);
}

/**
 * Whether `pid` exists. `process.kill(pid, 0)` delivers no signal; EPERM means the
 * process exists but belongs to someone else.
 *
 * @param {number} pid
 * @param {{kill?: typeof process.kill}} [deps]
 */
export function processAlive(pid, { kill = process.kill } = {}) {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try {
    kill(pid, 0);
    return true;
  } catch (e) {
    return e?.code === "EPERM";
  }
}

/** Compare two start keys numerically (they exceed 2^53 on Windows). */
export function startKeyCompare(a, b) {
  const x = BigInt(a || "0");
  const y = BigInt(b || "0");
  return x < y ? -1 : x > y ? 1 : 0;
}

/**
 * The descendants of `rootPid`, deepest last (so the REVERSE is a safe kill order:
 * a child dies before the parent supervising it). A "child" that started before the
 * root is a stale parent link to a recycled PID, not ours, and is skipped — the
 * same guard `scripts/contract-smoke.ps1` `Stop-ProcessTree` applies.
 *
 * @param {Map<number, ProcInfo>} table
 * @param {number} rootPid
 * @returns {ProcInfo[]}
 */
export function descendantsOf(table, rootPid) {
  const root = table.get(rootPid);
  const byParent = new Map();
  for (const p of table.values()) {
    if (p.pid === p.ppid) continue;
    if (!byParent.has(p.ppid)) byParent.set(p.ppid, []);
    byParent.get(p.ppid).push(p);
  }
  const out = [];
  const seen = new Set([rootPid]);
  const queue = [rootPid];
  while (queue.length > 0) {
    const cur = queue.shift();
    for (const child of byParent.get(cur) ?? []) {
      if (child.pid <= 4 || child.pid === process.pid || seen.has(child.pid)) continue;
      if (root && startKeyCompare(child.startKey, root.startKey) < 0) continue;
      seen.add(child.pid);
      out.push(child);
      queue.push(child.pid);
    }
  }
  return out;
}

/** True when `argv` (or `cmdline`) carries `flag value` as adjacent arguments. */
export function carriesFlag(proc, flag, value) {
  if (Array.isArray(proc?.argv)) {
    const i = proc.argv.indexOf(flag);
    return i >= 0 && proc.argv[i + 1] === value;
  }
  const escape = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(^|\\s)${escape(flag)}\\s+"?${escape(value)}"?(\\s|$)`).test(proc?.cmdline ?? "");
}

/**
 * Signal ONE process by PID — the only terminating door in termhost-bench.
 *
 * `expect(proc)` is evaluated against the process as it stands in `table` (read
 * fresh by the caller) immediately before the signal; a PID that no longer passes
 * it (exited, or recycled by something else) is left alone. Process groups (a
 * negative or zero PID) and PIDs 0-1 are refused outright.
 *
 * @param {number} pid
 * @param {{signal?: NodeJS.Signals, table?: Map<number, ProcInfo>, expect?: (p: ProcInfo) => boolean,
 *   kill?: typeof process.kill}} [opts]
 * @returns {{pid: number, outcome: "signalled" | "gone" | "identity_mismatch" | "error", detail?: string}}
 */
export function terminatePid(pid, { signal = "SIGTERM", table, expect, kill = process.kill } = {}) {
  if (!Number.isInteger(pid) || pid <= 1) {
    throw new RangeError(`terminatePid needs a single positive PID above 1, got ${pid}`);
  }
  if (table) {
    const proc = table.get(pid);
    if (!proc) return { pid, outcome: "gone" };
    if (expect && !expect(proc)) {
      return {
        pid,
        outcome: "identity_mismatch",
        detail: `pid ${pid} (${proc.name}) no longer matches what this run started`,
      };
    }
  } else if (expect) {
    throw new TypeError("an identity check needs the current process table");
  }
  try {
    kill(pid, signal);
    return { pid, outcome: "signalled" };
  } catch (e) {
    if (e?.code === "ESRCH") return { pid, outcome: "gone" };
    return { pid, outcome: "error", detail: e?.message ?? String(e) };
  }
}

/**
 * End a process this run started, and every descendant of it, children first:
 * SIGTERM, a grace period, then SIGKILL for whatever is left — each signal gated on
 * the process still being the one we started (same PID AND same start key, so a
 * recycled PID is never touched). Returns what was signalled and what survived.
 *
 * The root's start key is REQUIRED: it is the only proof that the PID is still the
 * process the run started. Without one (a process table read that missed a child
 * which died at startup), the root and its tree are left alone and the result says
 * so (`refused`); a root that is gone, or whose start key no longer matches, is not
 * walked either. `alsoPids` — processes proven ours by their own start keys — are
 * handled in every case.
 *
 * @param {{pid: number, startKey?: string | null} | null} root the process this run spawned (null: none to walk)
 * @param {{graceMs?: number, list?: () => Map<number, ProcInfo>, kill?: typeof process.kill,
 *   sleepFn?: (ms: number) => Promise<void>, now?: () => number,
 *   alsoPids?: {pid: number, startKey: string}[]}} [opts]
 *   `alsoPids` are further processes the run proved it owns (e.g. a daemonised
 *   postmaster named by its own pid file) that are not descendants of the root.
 * @returns {Promise<{signalled: number[], survivors: number[], refused?: string, note?: string}>}
 */
export async function terminateTree(
  root,
  {
    graceMs = 8000,
    list = () => listProcesses(),
    kill = process.kill,
    sleepFn = (ms) => new Promise((r) => setTimeout(r, ms)),
    now = Date.now,
    alsoPids = [],
  } = {},
) {
  const table = list();
  let refused;
  let note;
  let walk = [];
  if (root && !root.startKey) {
    refused = `pid ${root.pid}: no start key was captured, so its identity cannot be proven; its tree was not signalled`;
  } else if (root) {
    const rootProc = table.get(root.pid);
    if (!rootProc || rootProc.startKey !== root.startKey) note = `root ${root.pid} already gone`;
    else walk = [...descendantsOf(table, root.pid).map((p) => ({ pid: p.pid, startKey: p.startKey }))];
  }
  const owned = alsoPids.filter((t) => t && Number.isInteger(t.pid) && t.startKey);
  const rootLive = Boolean(root) && !refused && !note;
  // Children first (deepest last in the walk, so iterate in reverse), then the root.
  const order = [...walk, ...owned].reverse().concat(rootLive ? [{ pid: root.pid, startKey: root.startKey }] : []);
  const extra = { ...(refused ? { refused } : {}), ...(note ? { note } : {}) };
  if (order.length === 0) return { signalled: [], survivors: [], ...extra };
  const same = (want) => (p) => p.startKey === want.startKey;
  const signalled = [];
  for (const t of order) {
    const r = terminatePid(t.pid, { signal: "SIGTERM", table, expect: same(t), kill });
    if (r.outcome === "signalled") signalled.push(t.pid);
  }
  const deadline = now() + graceMs;
  let after = list();
  while (now() < deadline && order.some((t) => after.get(t.pid)?.startKey === t.startKey)) {
    await sleepFn(250);
    after = list();
  }
  const survivors = [];
  for (const t of order) {
    if (after.get(t.pid)?.startKey !== t.startKey) continue;
    terminatePid(t.pid, { signal: "SIGKILL", table: after, expect: same(t), kill });
  }
  await sleepFn(250);
  const final = list();
  for (const t of order) {
    if (final.get(t.pid)?.startKey === t.startKey) survivors.push(t.pid);
  }
  return { signalled, survivors, ...extra };
}
