/**
 * termhost-bench sessions: the generators' slot files and logs as the run loop
 * reads them (Phase 1's `slot-<k>` / `gen-<k>.jsonl` contract, D13).
 *
 * Because every session runs one identical command line, a generator learns its
 * session index by claiming the first free `slot-<k>`, and writes its PID, its
 * parent PID (the session shell) and the host's own terminal id where the host
 * exports one (`QONTINUI_TERMINAL_ID` for the runner, `TMUX_PANE` for tmux). The
 * harness maps slots to host sessions afterwards — exactly, through that id.
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 3.
 */

import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

import { parseJsonl, toNs } from "./kpis.mjs";

/**
 * Every claimed slot in a generators' run directory.
 *
 * @param {string} genDir
 * @returns {Map<number, {slot: number, pid: number, ppid: number, run_id: string,
 *   host_terminal_id: Record<string, string>, claimed_at_ns: string}>}
 */
export function readSlots(genDir) {
  const out = new Map();
  let names = [];
  try {
    names = readdirSync(genDir);
  } catch {
    return out;
  }
  for (const name of names) {
    const m = /^slot-(\d+)$/.exec(name);
    if (!m) continue;
    try {
      const rec = JSON.parse(readFileSync(join(genDir, name), "utf8").trim());
      if (rec && Number.isInteger(rec.slot)) out.set(rec.slot, rec);
    } catch {
      // a slot file is written in one write; an unreadable one is still being created
    }
  }
  return out;
}

/** Every generator's records, by slot. */
export function readGeneratorLogs(genDir) {
  const out = new Map();
  let names = [];
  try {
    names = readdirSync(genDir);
  } catch {
    return out;
  }
  for (const name of names) {
    const m = /^gen-(\d+)\.jsonl$/.exec(name);
    if (!m) continue;
    try {
      out.set(Number(m[1]), parseJsonl(readFileSync(join(genDir, name), "utf8")).records);
    } catch {
      out.set(Number(m[1]), []);
    }
  }
  return out;
}

/** One generator's records (empty when its log does not exist yet). */
export function readGeneratorLog(genDir, slot) {
  try {
    return parseJsonl(readFileSync(join(genDir, `gen-${slot}.jsonl`), "utf8")).records;
  } catch {
    return [];
  }
}

/** The slot whose generator reported `hostId` under `key`, or null. */
export function slotForHostId(slots, key, hostId) {
  for (const [slot, rec] of slots) if (rec.host_terminal_id?.[key] === hostId) return slot;
  return null;
}

/** Slots whose generator has logged its first output marker. */
export function slotsWithFirstMarker(genDir) {
  const ready = new Set();
  for (const [slot, records] of readGeneratorLogs(genDir)) {
    if (records.some((r) => r?.type === "marker" && r.kind === "output")) ready.add(slot);
  }
  return ready;
}

/**
 * Wait until `n` generators have each painted their first marker: measurement
 * starts only then, so shell start-up is inside spawn latency and outside every
 * steady-state KPI (D13).
 *
 * @param {string} genDir
 * @param {number} n
 * @param {{timeoutMs?: number, sleepFn?: (ms: number) => Promise<void>, now?: () => number}} [o]
 * @returns {Promise<{ok: boolean, ready: number[], slots: number, detail: string}>}
 */
export async function waitForGenerators(
  genDir,
  n,
  { timeoutMs = 60000, sleepFn = (ms) => new Promise((r) => setTimeout(r, ms)), now = Date.now } = {},
) {
  const deadline = now() + timeoutMs;
  let ready = new Set();
  let slots = new Map();
  while (now() < deadline) {
    slots = readSlots(genDir);
    ready = slotsWithFirstMarker(genDir);
    if (ready.size >= n) break;
    await sleepFn(250);
  }
  const ok = ready.size >= n;
  return {
    ok,
    ready: [...ready].sort((a, b) => a - b),
    slots: slots.size,
    detail: ok
      ? `${ready.size} generator(s) painting`
      : `${ready.size} of ${n} generator(s) painted a first marker within ${Math.round(timeoutMs / 1000)} s (${slots.size} slot(s) claimed)`,
  };
}

/**
 * Wait for a NEW slot (beyond `known`) whose generator reported `hostId` — how a
 * session added by `spawnOne` is matched to its slot. Falls back to the first new
 * slot claimed after `tCmdNs` when the host exports no terminal id.
 *
 * @returns {Promise<number | null>}
 */
export async function waitForNewSlot(
  genDir,
  known,
  { key = null, hostId = null, tCmdNs = 0n, timeoutMs = 30000, sleepFn = (ms) => new Promise((r) => setTimeout(r, ms)) } = {},
) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const slots = readSlots(genDir);
    const fresh = [...slots.values()].filter((s) => !known.has(s.slot));
    const exact = key && hostId ? fresh.find((s) => s.host_terminal_id?.[key] === hostId) : null;
    if (exact) return exact.slot;
    if (!key || !hostId) {
      const after = fresh
        .filter((s) => (toNs(s.claimed_at_ns) ?? 0n) >= tCmdNs)
        .sort((a, b) => (toNs(a.claimed_at_ns) < toNs(b.claimed_at_ns) ? -1 : 1));
      if (after.length > 0) return after[0].slot;
    }
    await sleepFn(100);
  }
  return null;
}

/**
 * The newest output-marker sequence a generator logged at or before `tNs`.
 *
 * @param {object[]} records the generator's gen-<k>.jsonl records
 */
export function newestSeqAt(records, tNs) {
  let newest = null;
  for (const r of records) {
    if (r?.type !== "marker" || r.kind !== "output") continue;
    const t = toNs(r.t_emit_ns);
    if (t !== null && t <= tNs) newest = r.seq;
  }
  return newest;
}

/** How many echo records a generator has logged (its cumulative echo counter). */
export function echoCount(records) {
  return records.filter((r) => r?.type === "echo").length;
}
