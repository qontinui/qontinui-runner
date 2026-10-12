/**
 * termhost-bench's driver for `termhost-probe` (Phase 2), the outside-in
 * instrument: `doctor --json`, `watch` (one per measurement phase) and `inject`
 * (the D9 input arm). The probe's interface is its CLI (`crates/termhost-probe/src/cli.rs`)
 * and its `termhost-probe/v1` JSON-lines records (`records.rs`).
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 3.
 *
 * How the orchestrator uses the probe, and why:
 *   - Every MEASURING `watch` runs with `--run-dir <gen-dir>`: the generators'
 *     `gen-<k>.jsonl` (emitted markers) and `slot-<k>` files (which fix workload
 *     membership for the whole run — generator pid -> parent pid = session shell)
 *     live there, and on Linux `inject` records the keys it is about to send in
 *     `<gen-dir>/inject.jsonl`, which `watch` matches to tell the probe's own keys
 *     from an operator's. So the input phase's `watch` and `inject` share it.
 *   - Every measuring `watch` is given the host's root PIDs (`--host-pid`,
 *     repeatable) and the generators' run id (`--run-id`): without live roots the
 *     host group is unknown, never 0.
 *   - A `watch` ends when its stdin closes (`--stdin-stop`), never through the stop
 *     file — `<gen-dir>/stop` also stops the generators.
 *   - `probe.jsonl` is APPENDED by every watch in the same directory, so a phase's
 *     records are the file's bytes from the offset taken just before that watch
 *     started.
 *   - A short "look" (`watch --duration-ms`, frames on change, no process sampler)
 *     in its own scratch directory answers "which sessions are visible, where" for
 *     zone resolution and arrangement checks.
 *
 * Every process here is started with an argv array, never a shell string.
 */

import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { parseJsonl } from "./kpis.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = resolve(HERE, "..", "..");

/** The probe binary: `--probe`, then `$TERMHOST_PROBE`, then the workspace's target dirs. */
export function resolveProbeBinary({
  explicit = null,
  env = process.env,
  platform = process.platform,
  repoRoot = REPO_ROOT,
  exists = existsSync,
} = {}) {
  const exe = platform === "win32" ? "termhost-probe.exe" : "termhost-probe";
  const candidates = [
    explicit,
    env.TERMHOST_PROBE || null,
    join(repoRoot, "target", "debug", exe),
    join(repoRoot, "target", "release", exe),
    join(repoRoot, "target-agent", "debug", exe),
  ].filter(Boolean);
  for (const c of candidates) if (exists(c)) return resolve(c);
  return null;
}

/** `--hwnd` / `--pid` / `--title`, exactly one. */
export function targetArgs(target) {
  if (target && Number.isInteger(target.hwnd)) return ["--hwnd", String(target.hwnd)];
  if (target && Number.isInteger(target.pid)) return ["--pid", String(target.pid)];
  if (target && typeof target.title === "string" && target.title !== "") return ["--title", target.title];
  throw new TypeError(`a probe target needs hwnd, pid or a non-empty title: ${JSON.stringify(target)}`);
}

/**
 * argv for a measuring `watch`.
 *
 * @param {{runDir: string, target: object, hostPids?: number[], runId?: string,
 *   frames?: "all" | "changes" | "none", expectVisible?: number[], durationMs?: number,
 *   stdinStop?: boolean, compositor?: boolean, tag?: string, minValidIntervals?: number}} o
 */
export function watchArgs(o) {
  const args = ["watch", "--run-dir", o.runDir, ...targetArgs(o.target)];
  for (const pid of o.hostPids ?? []) args.push("--host-pid", String(pid));
  if (o.runId) args.push("--run-id", o.runId);
  args.push("--frames", o.frames ?? "all");
  if (o.expectVisible?.length) args.push("--expect-visible", o.expectVisible.join(","));
  if (o.durationMs) args.push("--duration-ms", String(Math.round(o.durationMs)));
  if (o.stdinStop) args.push("--stdin-stop");
  if (o.compositor === false) args.push("--no-compositor");
  if (o.tag) args.push("--tag", o.tag);
  if (o.minValidIntervals !== undefined) args.push("--min-valid-intervals", String(o.minValidIntervals));
  return args;
}

/**
 * argv for `inject` (D9). Keys are letters only (`--count`), so no injected key is
 * Ctrl+C or anything a shell would act on.
 *
 * @param {{runDir: string, target: object, count: number, intervalMs?: number,
 *   startDelayMs?: number, settleMs?: number, activate?: boolean, tag?: string}} o
 */
export function injectArgs(o) {
  if (!Number.isInteger(o.count) || o.count < 0) throw new RangeError(`inject count must be >= 0, got ${o.count}`);
  const args = ["inject", "--run-dir", o.runDir, ...targetArgs(o.target), "--count", String(o.count)];
  if (o.intervalMs !== undefined) args.push("--interval-ms", String(o.intervalMs));
  if (o.startDelayMs !== undefined) args.push("--start-delay-ms", String(o.startDelayMs));
  if (o.settleMs !== undefined) args.push("--settle-ms", String(o.settleMs));
  if (o.activate) args.push("--activate");
  if (o.tag) args.push("--tag", o.tag);
  return args;
}

/** Byte size of a file, 0 when absent. */
export function fileSize(path) {
  try {
    return statSync(path).size;
  } catch {
    return 0;
  }
}

/** The complete JSON lines of `path` from byte `offset`. */
export function readRecordsFrom(path, offset = 0) {
  let buf;
  try {
    buf = readFileSync(path);
  } catch {
    return { records: [], malformed: 0, partial: false };
  }
  return parseJsonl(buf.subarray(Math.min(offset, buf.length)).toString("utf8"));
}

/**
 * `termhost-probe doctor --json`: the one-line doctor record.
 *
 * @param {{probe: string, node?: string, env?: object, timeoutMs?: number, run?: typeof spawnSync}} o
 * @returns {{record: object | null, error: string | null, raw: string}}
 */
export function probeDoctor({ probe, node = process.execPath, env = process.env, timeoutMs = 60000, run = spawnSync }) {
  const res = run(probe, ["doctor", "--json", "--node", node], {
    encoding: "utf8",
    env,
    timeout: timeoutMs,
    windowsHide: true,
    maxBuffer: 16 * 1024 * 1024,
  });
  const raw = `${res.stdout ?? ""}`;
  if (res.error) return { record: null, error: `termhost-probe doctor failed: ${res.error.message}`, raw };
  const { records } = parseJsonl(raw.endsWith("\n") ? raw : `${raw}\n`);
  const record = records.find((r) => r?.type === "doctor") ?? null;
  if (!record) {
    return {
      record: null,
      error: `termhost-probe doctor printed no doctor record (exit ${res.status}): ${String(res.stderr ?? "").slice(0, 400)}`,
      raw,
    };
  }
  return { record, error: null, raw };
}

/**
 * A running measuring `watch`. Start it, run the phase, `stop()` it: stdin closes
 * (`--stdin-stop`), the probe writes its summary and exits, and the records the
 * phase produced are read back from `probe.jsonl`.
 */
export class WatchProcess {
  /**
   * @param {{probe: string, args: string[], probeJsonl: string, env?: object,
   *   spawnFn?: typeof spawn, log?: (s: string) => void}} o
   */
  constructor({ probe, args, probeJsonl, env = process.env, spawnFn = spawn, log = () => {} }) {
    if (!args.includes("--stdin-stop")) throw new Error("a WatchProcess must run with --stdin-stop");
    this.probeJsonl = probeJsonl;
    this.offset = fileSize(probeJsonl);
    this.log = log;
    this.stdout = "";
    this.stderr = "";
    this.exit = null;
    this.child = spawnFn(probe, args, { env, stdio: ["pipe", "pipe", "pipe"], windowsHide: true });
    this.pid = this.child.pid;
    this.exited = new Promise((resolveExit) => {
      this.child.on("exit", (code, signal) => {
        this.exit = { code, signal };
        resolveExit(this.exit);
      });
      this.child.on("error", (e) => {
        this.exit = { code: null, signal: null, error: e.message };
        resolveExit(this.exit);
      });
    });
    this.child.stdout?.on("data", (d) => {
      this.stdout = (this.stdout + d.toString("utf8")).slice(-64 * 1024);
    });
    this.child.stderr?.on("data", (d) => {
      this.stderr = (this.stderr + d.toString("utf8")).slice(-16 * 1024);
    });
  }

  /** Resolves once the probe has written its start record (or exited). */
  async ready({ timeoutMs = 15000, sleepFn = (ms) => new Promise((r) => setTimeout(r, ms)) } = {}) {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      if (this.exit) return false;
      if (readRecordsFrom(this.probeJsonl, this.offset).records.some((r) => r?.type === "start")) return true;
      await sleepFn(100);
    }
    return false;
  }

  /**
   * End the watch and collect its records.
   *
   * @returns {Promise<{records: object[], exit: object, stderr: string, timedOut: boolean}>}
   */
  async stop({ timeoutMs = 20000 } = {}) {
    try {
      this.child.stdin?.end();
    } catch {
      // already closed
    }
    let timedOut = false;
    const timer = new Promise((r) => setTimeout(() => r("timeout"), timeoutMs));
    if ((await Promise.race([this.exited, timer])) === "timeout") {
      timedOut = true;
      this.log(`watch pid ${this.pid} did not exit ${timeoutMs} ms after stdin closed; ending our child by its handle`);
      // Our own child, by its handle: Node signals exactly this PID.
      this.child.kill("SIGTERM");
      await Promise.race([this.exited, new Promise((r) => setTimeout(r, 5000))]);
    }
    const { records } = readRecordsFrom(this.probeJsonl, this.offset);
    return { records, exit: this.exit, stderr: this.stderr, timedOut };
  }
}

/**
 * Run a probe subcommand to completion (inject, a timed look).
 *
 * @returns {Promise<{code: number | null, stdout: string, stderr: string}>}
 */
export function runProbe({ probe, args, env = process.env, timeoutMs = 120000, spawnFn = spawn }) {
  return new Promise((resolveRun) => {
    const child = spawnFn(probe, args, { env, stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
    let stdout = "";
    let stderr = "";
    child.stdout?.on("data", (d) => {
      stdout = (stdout + d.toString("utf8")).slice(-64 * 1024);
    });
    child.stderr?.on("data", (d) => {
      stderr = (stderr + d.toString("utf8")).slice(-16 * 1024);
    });
    const timer = setTimeout(() => {
      stderr += `\n(termhost-bench: ${args[0]} exceeded ${timeoutMs} ms; ending our child by its handle)`;
      child.kill("SIGTERM");
    }, timeoutMs);
    child.on("exit", (code) => {
      clearTimeout(timer);
      resolveRun({ code, stdout, stderr });
    });
    child.on("error", (e) => {
      clearTimeout(timer);
      resolveRun({ code: null, stdout, stderr: `${stderr}\n${e.message}` });
    });
  });
}

/**
 * A short look at the window: which sessions' markers are visible and where.
 * Its records go to `<lookDir>/probe.jsonl`; no process sampler runs.
 *
 * @returns {Promise<{ok: boolean, sessions: Map<number, {x: number, y: number, w: number, h: number, frames: number}>,
 *   frames: number, captureOpened: boolean, detail: string}>}
 */
export async function look({ probe, lookDir, target, durationMs = 2500, env = process.env, run = runProbe }) {
  mkdirSync(lookDir, { recursive: true });
  const args = watchArgs({ runDir: lookDir, target, frames: "changes", durationMs, compositor: false });
  const res = await run({ probe, args, env, timeoutMs: durationMs + 30000 });
  const { records } = readRecordsFrom(join(lookDir, "probe.jsonl"));
  return summarizeLook(records, res);
}

/** Pure half of {@link look}. */
export function summarizeLook(records, res = { code: 0, stderr: "" }) {
  const captures = records.filter((r) => r?.type === "capture");
  const opened = captures.filter((c) => c.state === "started").map((c) => c.source);
  const primary = opened[0] ?? null;
  const sessions = new Map();
  let frames = 0;
  for (const f of records) {
    if (f?.type !== "frame" || f.source !== primary) continue;
    frames += 1;
    for (const m of f.markers ?? []) {
      if (m.kind !== "output") continue;
      const prev = sessions.get(m.session);
      sessions.set(m.session, {
        x: m.rect?.x ?? 0,
        y: m.rect?.y ?? 0,
        w: m.rect?.w ?? 0,
        h: m.rect?.h ?? 0,
        frames: (prev?.frames ?? 0) + 1,
      });
    }
  }
  const failed = captures.filter((c) => c.state === "failed").map((c) => `${c.source}: ${c.detail}`);
  return {
    ok: primary !== null,
    sessions,
    frames,
    captureOpened: primary !== null,
    detail:
      primary === null
        ? `no capture source opened${failed.length ? `: ${failed.join("; ")}` : ""} (exit ${res.code}; ${String(res.stderr).slice(-300)})`
        : `${frames} frame(s), ${sessions.size} session(s) visible`,
  };
}

/**
 * Make the measured window foreground without typing anything: `inject --count 0
 * --activate` (D9 still applies — the probe refuses when real input is heard).
 *
 * @returns {Promise<{ok: boolean, detail: string}>}
 */
export async function activate({ probe, runDir, target, env = process.env, run = runProbe }) {
  if (target?.pid === undefined && target?.hwnd === undefined) {
    return { ok: false, detail: "activation needs a pid or hwnd target" };
  }
  const args = injectArgs({ runDir, target, count: 0, settleMs: 0, activate: true });
  const res = await run({ probe, args, env, timeoutMs: 30000 });
  return {
    ok: res.code === 0,
    detail: res.code === 0 ? "target activated" : `inject --activate exited ${res.code}: ${`${res.stdout}${res.stderr}`.slice(-400)}`,
  };
}
