#!/usr/bin/env node
/**
 * Reproducible many-sessions load harness for the runner.
 *
 * Phase 0 of `qontinui-dev-notes/plans/2026-07-28-runner-many-sessions-performance.md`:
 * the yardstick every later phase proves its win against. One command produces
 * a before/after table.
 *
 * What it measures, at each session level S (default 10 / 20 / 40):
 *   (a) p50/p95/max `terminal_create` wall time — timed client-side over
 *       `POST /terminals`, per spawn, so the distribution is real rather than
 *       an average;
 *   (b) frontend long-task count and frame time — **via the UI Bridge only**
 *       (`POST /ui-bridge/control/page/evaluate`). Playwright is a standing
 *       project prohibition and this harness never reaches for it;
 *   (c) chunk→paint rates — backend bytes/s from `totalBytesProduced` deltas
 *       against in-page paint (rAF) and webview `terminal-output` delivery
 *       counts;
 *   (d) the spawn-segment breakdown, parsed out of the `terminal_spawn.*`
 *       tracing spans in `qontinui-runner.log.<date>` so the identity-seam /
 *       registry / coord-registration split is visible per spawn.
 *
 * SAFETY. The harness only ever talks to a **temp runner spawned through the
 * supervisor** (`POST :9875/runners/spawn-test`, ports 9877+). It never
 * touches, restarts or stops the primary runner on 9876, and it never kills a
 * process. `--runner-url` lets you point it at a temp runner you already have;
 * it refuses port 9876 outright.
 *
 * Run `node scripts/perf-harness.mjs --help` for usage.
 */

import { readFileSync, writeFileSync, mkdirSync, statSync, openSync, readSync, closeSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  stats,
  parseSpawnSpans,
  aggregateSpans,
  foldFrontendSample,
  foldChunkRates,
  renderSingleRun,
  renderComparison,
} from "./perf-harness-lib.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, "..");

/** The primary runner's port. The harness refuses to drive it, ever. */
const PRIMARY_RUNNER_PORT = 9876;

const DEFAULTS = {
  supervisor: "http://127.0.0.1:9875",
  sessions: [10, 20, 40],
  soakMs: 15000,
  settleMs: 3000,
  createTimeoutMs: 120000,
  spawnTimeoutSecs: 300,
  generator:
    'while ($true) { Get-Date -Format "o"; Start-Sleep -Milliseconds 50 }',
  outDir: join(REPO_ROOT, ".perf-harness"),
  minPctChange: 5,
  /**
   * The spans are `tracing::debug_span!`, so DEBUG must be on for the two
   * modules that own them. Scoped rather than blanket `qontinui_runner=debug`
   * because blanket debug floods the sink hard enough to distort the very
   * latency being measured.
   */
  rustLog:
    "qontinui_runner=info,qontinui_runner::terminal::session=debug,qontinui_runner::commands::terminal=debug,tauri=info",
};

const HELP = `
perf-harness — reproducible many-sessions load harness for qontinui-runner

USAGE
  node scripts/perf-harness.mjs [options]
  node scripts/perf-harness.mjs --diff <before.json> <after.json>
  node scripts/perf-harness.mjs --dry-run

MODES
  (default)              Spawn a temp runner via the supervisor, drive it at each
                         session level, write a run JSON and print the table.
  --diff A B             Render the before/after table from two saved run JSONs.
                         Touches no runner and needs nothing running.
  --dry-run              Exercise the entire pipeline against a synthetic
                         in-process runner. No supervisor, no HTTP, no spawns.
                         Use it to verify the harness itself.

RUNNER SELECTION
  --supervisor URL       Supervisor base URL (default ${DEFAULTS.supervisor}).
  --runner-url URL       Use an already-running temp runner instead of spawning
                         one. Port ${PRIMARY_RUNNER_PORT} is refused.
  --rebuild              spawn-test with rebuild:true (builds origin/main in the
                         supervisor's own pool — not your worktree).
  --git-ref REF          Build and run REF (implies --rebuild).
  --worktree PATH        Build and run a worktree checkout (implies --rebuild).
  --use-lkg              Pin the spawn to the supervisor's last-known-good exe.
  --keep-runner          Leave the temp runner running at the end (default is to
                         POST /runners/<id>/stop).

LOAD SHAPE
  --sessions 10,20,40    Cumulative session levels to measure (default
                         ${DEFAULTS.sessions.join(",")}). At each level the harness tops the
                         runner up to S terminals and times only the new ones,
                         so create latency is measured against a growing
                         population — the O(N) hypothesis this plan tests.
  --generator CMD        Noisy generator run in every terminal. Default is a
                         PowerShell 20 Hz timestamp loop.
  --soak-ms N            Generator soak before sampling (default ${DEFAULTS.soakMs}).
  --settle-ms N          Settle pause after creating terminals (default ${DEFAULTS.settleMs}).
  --skip-frontend        Do not install the UI Bridge probe. Use as the control
                         arm when a run wedges the webview, to establish whether
                         the probe or the load is responsible — and on any runner
                         whose webview is unavailable. Backend metrics (create
                         latency, chunk bytes/s, spawn spans) are unaffected.

OUTPUT
  --label NAME           Run label, used in filenames and tables.
  --out PATH             Run JSON path (default .perf-harness/<label>-<ts>.json).
  --markdown PATH        Also write the rendered markdown report here.
  --baseline PATH        After measuring, print the before/after table against
                         this saved run.
  --min-pct N            Percent change below which a diff reads "same"
                         (default ${DEFAULTS.minPctChange}).
  --json                 Print the run JSON to stdout instead of the table.
  --quiet                Suppress progress chatter.

EXAMPLES
  # First ever run — record the baseline.
  node scripts/perf-harness.mjs --label phase0-baseline --use-lkg \\
      --out .perf-harness/phase0-baseline.json

  # After a phase lands — measure and diff in one command.
  node scripts/perf-harness.mjs --label phase1 --rebuild \\
      --baseline .perf-harness/phase0-baseline.json

  # Re-render a table from saved runs, offline.
  node scripts/perf-harness.mjs --diff .perf-harness/phase0-baseline.json \\
      .perf-harness/phase1.json
`;

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

/** Flags taking no value. */
const BOOL_FLAGS = new Set([
  "help",
  "dry-run",
  "rebuild",
  "use-lkg",
  "keep-runner",
  "skip-frontend",
  "json",
  "quiet",
]);
/** Flags taking exactly two values. */
const PAIR_FLAGS = new Set(["diff"]);
/** Flags taking exactly one value. */
const VALUE_FLAGS = new Set([
  "supervisor",
  "runner-url",
  "git-ref",
  "worktree",
  "sessions",
  "generator",
  "soak-ms",
  "settle-ms",
  "spawn-timeout-secs",
  "rust-log",
  "label",
  "out",
  "markdown",
  "baseline",
  "min-pct",
]);

/**
 * Parse argv into an options object. Unknown flags are a hard error — a typo'd
 * `--sessions` that silently measured the default would be worse than a crash.
 *
 * @param {string[]} argv
 * @returns {Record<string, string|boolean|string[]>}
 */
export function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "-h") {
      out.help = true;
      continue;
    }
    if (!arg.startsWith("--")) throw new Error(`unexpected positional argument: ${arg}`);
    const name = arg.slice(2);
    if (BOOL_FLAGS.has(name)) {
      out[name] = true;
      continue;
    }
    if (PAIR_FLAGS.has(name)) {
      const a = argv[i + 1];
      const b = argv[i + 2];
      if (!a || !b || a.startsWith("--") || b.startsWith("--")) {
        throw new Error(`--${name} needs two values`);
      }
      out[name] = [a, b];
      i += 2;
      continue;
    }
    if (!VALUE_FLAGS.has(name)) {
      throw new Error(`unknown flag: --${name}`);
    }
    const value = argv[i + 1];
    if (value === undefined || value.startsWith("--")) {
      throw new Error(`--${name} needs a value`);
    }
    out[name] = value;
    i += 1;
  }
  return out;
}

/**
 * Parse `--sessions 10,20,40` into a sorted, deduped, positive integer list.
 *
 * @param {string|undefined} text
 * @returns {number[]}
 */
export function parseSessionLevels(text) {
  if (!text) return [...DEFAULTS.sessions];
  const levels = text
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean)
    .map((s) => {
      const n = Number.parseInt(s, 10);
      if (!Number.isInteger(n) || n <= 0) throw new Error(`bad session level: ${s}`);
      return n;
    });
  if (levels.length === 0) throw new Error("--sessions needs at least one level");
  return [...new Set(levels)].sort((a, b) => a - b);
}

// ---------------------------------------------------------------------------
// Small utilities
// ---------------------------------------------------------------------------

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/**
 * Read the tail of a file from `offset`, tolerating rotation (a file smaller
 * than the recorded offset is treated as a fresh file and read whole).
 *
 * Deliberately synchronous + chunked so a multi-hundred-MB sink never gets
 * slurped whole just to see the last few seconds.
 *
 * @param {string} path
 * @param {number} offset
 * @returns {string}
 */
export function readFileFrom(path, offset) {
  let size;
  try {
    size = statSync(path).size;
  } catch {
    return "";
  }
  const start = size < offset ? 0 : offset;
  const length = size - start;
  if (length <= 0) return "";
  const fd = openSync(path, "r");
  try {
    const buf = Buffer.allocUnsafe(length);
    let read = 0;
    while (read < length) {
      const n = readSync(fd, buf, read, length - read, start + read);
      if (n <= 0) break;
      read += n;
    }
    return buf.subarray(0, read).toString("utf8");
  } finally {
    closeSync(fd);
  }
}

/**
 * Byte size of a file, or 0 when it does not exist yet.
 *
 * @param {string} path
 * @returns {number}
 */
function fileSize(path) {
  try {
    return statSync(path).size;
  } catch {
    return 0;
  }
}

/**
 * JSON HTTP call with a timeout and a readable failure.
 *
 * @param {string} url
 * @param {{method?:string, body?:unknown, timeoutMs?:number}} [options]
 * @returns {Promise<unknown>}
 */
async function httpJson(url, options = {}) {
  const { method = "GET", body, timeoutMs = 30000 } = options;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const res = await fetch(url, {
      method,
      signal: controller.signal,
      headers: body === undefined ? undefined : { "Content-Type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const text = await res.text();
    let parsed;
    try {
      parsed = text ? JSON.parse(text) : null;
    } catch {
      throw new Error(`${method} ${url} -> ${res.status}, non-JSON body: ${text.slice(0, 400)}`);
    }
    if (!res.ok) {
      throw new Error(`${method} ${url} -> ${res.status} ${JSON.stringify(parsed).slice(0, 600)}`);
    }
    return parsed;
  } finally {
    clearTimeout(timer);
  }
}

/** Unwrap the runner's `{success, data}` envelope. */
function unwrap(payload) {
  if (payload && typeof payload === "object" && "data" in payload && "success" in payload) {
    if (payload.success === false) throw new Error(`runner API error: ${JSON.stringify(payload)}`);
    return payload.data;
  }
  return payload;
}

// ---------------------------------------------------------------------------
// In-page probe (UI Bridge only — never Playwright)
// ---------------------------------------------------------------------------

/**
 * JS installed in the runner webview to collect long tasks, paint gaps and
 * `terminal-output` delivery counts. Returned as one expression because that
 * is what `POST /ui-bridge/control/page/evaluate` accepts.
 *
 * The probe stores raw samples and does no maths — percentiles are computed in
 * Node by the unit-tested `perf-harness-lib` so the harness has exactly one
 * implementation of them.
 */
const PROBE_INSTALL = `(() => {
  const w = window;
  if (w.__qontinuiPerfProbe && typeof w.__qontinuiPerfProbe.stop === 'function') {
    try { w.__qontinuiPerfProbe.stop(); } catch (e) { /* previous probe already torn down */ }
  }
  const state = {
    startedAt: performance.now(),
    frameGapsMs: [],
    longTasks: [],
    webviewOutputEvents: 0,
    webviewHooked: false,
    errors: [],
    _raf: 0,
    _obs: null,
    _unlisten: null,
  };
  try {
    const obs = new PerformanceObserver((list) => {
      for (const e of list.getEntries()) {
        state.longTasks.push({ start: e.startTime, duration: e.duration });
        if (state.longTasks.length > 20000) state.longTasks.shift();
      }
    });
    obs.observe({ entryTypes: ['longtask'] });
    state._obs = obs;
  } catch (e) {
    state.errors.push('longtask-observer: ' + String(e));
  }
  let last = performance.now();
  const tick = () => {
    const now = performance.now();
    state.frameGapsMs.push(now - last);
    if (state.frameGapsMs.length > 20000) state.frameGapsMs.shift();
    last = now;
    state._raf = requestAnimationFrame(tick);
  };
  state._raf = requestAnimationFrame(tick);
  try {
    const ev = w.__TAURI__ && w.__TAURI__.event;
    if (ev && typeof ev.listen === 'function') {
      ev.listen('terminal-output', () => { state.webviewOutputEvents += 1; })
        .then((un) => { state._unlisten = un; state.webviewHooked = true; })
        .catch((e) => { state.errors.push('listen: ' + String(e)); });
    } else {
      state.errors.push('tauri-event-api-unavailable');
    }
  } catch (e) {
    state.errors.push('tauri-hook: ' + String(e));
  }
  state.stop = () => {
    try { cancelAnimationFrame(state._raf); } catch (e) { /* already cancelled */ }
    try { if (state._obs) state._obs.disconnect(); } catch (e) { /* already disconnected */ }
    try { if (state._unlisten) state._unlisten(); } catch (e) { /* already unlistened */ }
  };
  state.reset = () => {
    state.startedAt = performance.now();
    state.frameGapsMs.length = 0;
    state.longTasks.length = 0;
    state.webviewOutputEvents = 0;
    last = performance.now();
  };
  w.__qontinuiPerfProbe = state;
  return { installed: true, longTaskObserver: !!state._obs, errors: state.errors };
})()`;

/** Restart the probe's collection window without reinstalling the observers. */
const PROBE_RESET = `(() => {
  const s = window.__qontinuiPerfProbe;
  if (!s) return { error: 'probe-not-installed' };
  s.reset();
  return { reset: true };
})()`;

/** Read the probe's raw samples back. */
const PROBE_SAMPLE = `(() => {
  const s = window.__qontinuiPerfProbe;
  if (!s) return { error: 'probe-not-installed' };
  return {
    elapsedMs: performance.now() - s.startedAt,
    frameGapsMs: s.frameGapsMs.slice(),
    longTasks: s.longTasks.slice(),
    webviewOutputEvents: s.webviewOutputEvents,
    webviewHooked: s.webviewHooked,
    errors: s.errors.slice(),
  };
})()`;

/** Tear the probe down so a later harness run starts clean. */
const PROBE_STOP = `(() => {
  const s = window.__qontinuiPerfProbe;
  if (!s) return { stopped: false };
  s.stop();
  delete window.__qontinuiPerfProbe;
  return { stopped: true };
})()`;

// ---------------------------------------------------------------------------
// Live clients
// ---------------------------------------------------------------------------

/** Supervisor client — spawn-test and stop only. Never touches the primary. */
class SupervisorClient {
  constructor(baseUrl, log) {
    this.baseUrl = baseUrl.replace(/\/+$/, "");
    this.log = log;
  }

  async health() {
    return httpJson(`${this.baseUrl}/health`, { timeoutMs: 20000 });
  }

  /**
   * `POST /runners/spawn-test`. Temp runners land on 9877+; the primary is
   * untouched by construction — this endpoint cannot address it.
   *
   * @param {object} body
   * @param {number} timeoutMs
   */
  async spawnTest(body, timeoutMs) {
    this.log(`supervisor: POST /runners/spawn-test ${JSON.stringify(body)}`);
    return httpJson(`${this.baseUrl}/runners/spawn-test`, {
      method: "POST",
      body,
      timeoutMs,
    });
  }

  /**
   * @param {string} id temp-runner id returned by spawn-test
   */
  async stopRunner(id) {
    this.log(`supervisor: POST /runners/${id}/stop`);
    return httpJson(`${this.baseUrl}/runners/${encodeURIComponent(id)}/stop`, {
      method: "POST",
      body: {},
      timeoutMs: 60000,
    });
  }
}

/** Temp-runner client: terminals API, log-sink resolution and the UI Bridge. */
class RunnerClient {
  constructor(baseUrl, log) {
    this.baseUrl = baseUrl.replace(/\/+$/, "");
    const port = Number.parseInt(new URL(this.baseUrl).port || "0", 10);
    if (port === PRIMARY_RUNNER_PORT) {
      throw new Error(
        `refusing to drive port ${PRIMARY_RUNNER_PORT}: that is the primary runner. ` +
          `Spawn a temp runner via the supervisor (the harness does this by default).`,
      );
    }
    this.log = log;
  }

  async health() {
    return httpJson(`${this.baseUrl}/health`, { timeoutMs: 20000 });
  }

  /** Wait for `/health` to answer, polling until `timeoutMs` elapses. */
  async waitHealthy(timeoutMs) {
    const deadline = Date.now() + timeoutMs;
    let lastError = null;
    while (Date.now() < deadline) {
      try {
        const h = await this.health();
        if (h) return h;
      } catch (e) {
        lastError = e;
      }
      await sleep(1000);
    }
    throw new Error(`runner ${this.baseUrl} never became healthy: ${lastError?.message ?? "?"}`);
  }

  async logSink() {
    return unwrap(await httpJson(`${this.baseUrl}/log-sources/runner-log-sink`, { timeoutMs: 20000 }));
  }

  async listTerminals() {
    const data = unwrap(await httpJson(`${this.baseUrl}/terminals`, { timeoutMs: 30000 }));
    if (Array.isArray(data)) return data;
    if (Array.isArray(data?.terminals)) return data.terminals;
    if (Array.isArray(data?.sessions)) return data.sessions;
    return [];
  }

  /** @returns {Promise<{info:object, wallMs:number}>} */
  async createTerminal(body, timeoutMs) {
    const t0 = performance.now();
    const data = unwrap(
      await httpJson(`${this.baseUrl}/terminals`, { method: "POST", body, timeoutMs }),
    );
    return { info: data, wallMs: performance.now() - t0 };
  }

  async writeTerminal(id, text) {
    return httpJson(`${this.baseUrl}/terminals/${encodeURIComponent(id)}/write`, {
      method: "POST",
      body: { data: Buffer.from(text, "utf8").toString("base64") },
      timeoutMs: 20000,
    });
  }

  async closeTerminal(id) {
    return httpJson(`${this.baseUrl}/terminals/${encodeURIComponent(id)}`, {
      method: "DELETE",
      timeoutMs: 30000,
    });
  }

  /** UI Bridge page evaluation — the ONLY frontend measurement channel here. */
  async evaluate(expression, timeoutMs = 30000) {
    const payload = await httpJson(`${this.baseUrl}/ui-bridge/control/page/evaluate`, {
      method: "POST",
      body: { expression, unwrap: true, timeoutMs: Math.max(1000, timeoutMs - 2000) },
      timeoutMs,
    });
    const data = unwrap(payload);
    // `evaluate` answers `{type, value}` when it can classify the result.
    if (data && typeof data === "object" && "value" in data && "type" in data) return data.value;
    return data;
  }

  async activateTab(tabId) {
    return httpJson(`${this.baseUrl}/ui-bridge/control/tab/activate`, {
      method: "POST",
      body: { tabId },
      timeoutMs: 20000,
    });
  }
}

// ---------------------------------------------------------------------------
// Dry-run fake
// ---------------------------------------------------------------------------

/** Deterministic 32-bit PRNG so `--dry-run` output is reproducible. */
function makeRng(seed) {
  let s = seed >>> 0;
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 0x100000000;
  };
}

/**
 * A synthetic runner that reproduces the shape of the real one — including the
 * O(N) create latency this plan exists to remove, and a log file full of
 * genuine-format `terminal_spawn.*` span-close lines.
 *
 * `--dry-run` drives the identical measurement code path against it, so the
 * harness is verifiable end to end without a live runner.
 */
class FakeRunner {
  constructor(log) {
    this.log = log;
    this.baseUrl = "http://127.0.0.1:9999";
    this.terminals = new Map();
    this.rng = makeRng(20260728);
    this.logLines = [];
    this.seq = 0;
  }

  async health() {
    return { buildId: "dry-run-build", data: { gitSha: "0000000000", buildId: "dry-run-build" } };
  }

  async waitHealthy() {
    return this.health();
  }

  async logSink() {
    return { dir: "<dry-run>", prefix: "qontinui-runner.log", current_file: "<dry-run>", exists: true };
  }

  async listTerminals() {
    // Accrue output the way a real generator does, so the byte-delta path in
    // `foldChunkRates` is exercised rather than always dividing zero.
    const now = Date.now();
    for (const t of this.terminals.values()) {
      if (t.generating) {
        t.totalBytesProduced += Math.max(0, now - t.lastSampledAt) * 0.6;
        t.lastSampledAt = now;
      }
    }
    return [...this.terminals.values()];
  }

  async createTerminal() {
    const n = this.terminals.size;
    // Modelled on §0 B1/B2: a fixed process-exec cost plus per-spawn work that
    // grows linearly with the existing population (the whole-map registry
    // rewrite), plus jitter.
    const wallMs = 180 + n * 9 + this.rng() * 60;
    const id = `dry-${String(this.seq++).padStart(4, "0")}`;
    this.terminals.set(id, {
      id,
      totalBytesProduced: 0,
      isAlive: true,
      generating: false,
      lastSampledAt: Date.now(),
    });
    this.#emitSpans(id, wallMs);
    return { info: { id, isAlive: true, totalBytesProduced: 0 }, wallMs };
  }

  #emitSpans(id, wallMs) {
    const ts = new Date(Date.now()).toISOString().replace("T", " ").slice(0, 23);
    const segments = [
      ["terminal_spawn.resolve_config_dir", wallMs * 0.05],
      ["terminal_spawn.claude_hook_materialize", wallMs * 0.12],
      ["terminal_spawn.coord_mcp_provision", wallMs * 0.18],
      ["terminal_spawn.shim_materialize", wallMs * 0.15],
      ["terminal_spawn.record_open", wallMs * 0.22],
      ["terminal_spawn.identity_seam", wallMs * 0.5],
      ["terminal_spawn.manager_create", wallMs * 0.85],
      ["terminal_spawn.coord_register", wallMs * 0.06],
    ];
    for (const [span, ms] of segments) {
      const busy = (ms * 0.8).toFixed(3);
      const idle = (ms * 0.2).toFixed(3);
      this.logLines.push(
        `${ts} DEBUG ${span}{terminal_id=${id}}: qontinui_runner::terminal::session: ` +
          `close time.busy=${busy}ms time.idle=${idle}ms`,
      );
    }
  }

  async writeTerminal(id) {
    const t = this.terminals.get(id);
    if (t) {
      t.generating = true;
      t.lastSampledAt = Date.now();
    }
    return { success: true };
  }

  async closeTerminal(id) {
    this.terminals.delete(id);
    return { success: true };
  }

  async activateTab() {
    return { success: true };
  }

  async evaluate(expression) {
    if (expression === PROBE_INSTALL) return { installed: true, longTaskObserver: true, errors: [] };
    if (expression === PROBE_RESET) return { reset: true };
    if (expression === PROBE_STOP) return { stopped: true };
    if (expression === PROBE_SAMPLE) {
      const n = this.terminals.size;
      const elapsedMs = 15000;
      // Frame gaps degrade with session count — the A1/A3 render storm.
      const frames = [];
      const budget = 16.7 + n * 1.4;
      for (let i = 0; i < Math.max(1, Math.floor(elapsedMs / budget)); i += 1) {
        frames.push(budget * (0.7 + this.rng() * 0.7));
      }
      const longTasks = [];
      for (let i = 0; i < Math.floor(n * 2.5); i += 1) {
        longTasks.push({ start: i * 100, duration: 55 + this.rng() * 90 });
      }
      return {
        elapsedMs,
        frameGapsMs: frames,
        longTasks,
        webviewOutputEvents: Math.floor(n * n * 1.5),
        webviewHooked: true,
        errors: [],
      };
    }
    return null;
  }

  /** Log text the fake "wrote" since `offset` lines. */
  logTextFrom(offset) {
    return this.logLines.slice(offset).join("\n");
  }

  logMark() {
    return this.logLines.length;
  }
}

// ---------------------------------------------------------------------------
// Measurement
// ---------------------------------------------------------------------------

/**
 * Drive one session level: top the runner up to `target` terminals, timing
 * each new spawn, soak the generators, then sample frontend + chunk rates and
 * parse the spawn spans emitted during this level.
 *
 * @returns {Promise<object>} one `levels[]` entry
 */
async function measureLevel({ runner, target, existing, config, logSinkPath, isDry, log }) {
  const toCreate = Math.max(0, target - existing.length);
  log(`S=${target}: creating ${toCreate} terminal(s) (population before: ${existing.length})`);

  const logMark = isDry ? runner.logMark() : fileSize(logSinkPath);

  const created = [];
  const wallTimes = [];
  const failures = [];
  for (let i = 0; i < toCreate; i += 1) {
    try {
      const { info, wallMs } = await runner.createTerminal(
        {
          title: `perf-harness S${target} #${existing.length + i + 1}`,
          cols: 120,
          rows: 30,
          page_id: "perf-harness",
        },
        config.createTimeoutMs,
      );
      wallTimes.push(wallMs);
      created.push(info);
      existing.push(info);
    } catch (e) {
      failures.push(String(e?.message ?? e));
      log(`S=${target}: create #${i + 1} FAILED: ${e?.message ?? e}`);
    }
  }

  // Start the generator in the freshly created terminals only — the earlier
  // ones are already noisy from the previous level, which is exactly the
  // cumulative load this measures.
  for (const t of created) {
    try {
      await runner.writeTerminal(t.id, `${config.generator}\r`);
    } catch (e) {
      failures.push(`write ${t.id}: ${String(e?.message ?? e)}`);
    }
  }

  await sleep(config.settleMs);

  // Frontend probe window covers the soak only.
  let frontend = null;
  let frontendError = config.skipFrontend ? "skipped (--skip-frontend)" : null;
  if (!config.skipFrontend) {
    try {
      await runner.evaluate(PROBE_RESET);
    } catch (e) {
      frontendError = `probe reset: ${String(e?.message ?? e)}`;
    }
  }

  const bytesBefore = await totalBytes(runner);
  const soakStart = performance.now();
  await sleep(config.soakMs);
  const soakElapsed = performance.now() - soakStart;
  const bytesAfter = await totalBytes(runner);

  if (!config.skipFrontend) {
    try {
      const sample = await runner.evaluate(PROBE_SAMPLE, 60000);
      if (sample && !sample.error) frontend = foldFrontendSample(sample);
      else frontendError = frontendError ?? `probe sample: ${sample?.error ?? "empty"}`;
    } catch (e) {
      frontendError = frontendError ?? `probe sample: ${String(e?.message ?? e)}`;
    }
  }

  const chunks = foldChunkRates(
    { bytes: bytesAfter - bytesBefore, elapsedMs: soakElapsed },
    frontend,
  );

  const logText = isDry ? runner.logTextFrom(logMark) : readFileFrom(logSinkPath, logMark);
  const spanRecords = parseSpawnSpans(logText);
  const spawnSpans = aggregateSpans(spanRecords);

  return {
    sessions: target,
    populationBefore: target - toCreate,
    created: toCreate,
    createFailures: failures,
    terminalCreateMs: stats(wallTimes),
    terminalCreateSamplesMs: wallTimes.map((v) => Number(v.toFixed(2))),
    frontend,
    frontendError,
    // Tauri answers `failed to send message to the webview` when the webview's
    // event loop has stopped servicing IPC. Under this harness's load that is
    // not a transport hiccup — it is the freeze the plan exists to fix — so it
    // gets its own field rather than hiding inside an error string.
    webviewWedged: typeof frontendError === "string"
      ? frontendError.includes("failed to send message to the webview") ||
        frontendError.includes("page_evaluate timed out")
      : false,
    chunks,
    spawnSpans,
    spawnSpanSampleCount: spanRecords.length,
    logBytesScanned: logText.length,
  };
}

/** Sum `totalBytesProduced` across the runner's live terminals. */
async function totalBytes(runner) {
  try {
    const list = await runner.listTerminals();
    return list.reduce((sum, t) => sum + (Number(t?.totalBytesProduced) || 0), 0);
  } catch {
    return 0;
  }
}

// ---------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------

/**
 * Run the whole harness. Returns the run document.
 *
 * @param {object} config
 * @param {(msg:string)=>void} log
 */
export async function runHarness(config, log) {
  const isDry = Boolean(config.dryRun);
  const notes = [];
  const startedAt = new Date().toISOString();

  let supervisor = null;
  let runner = null;
  let spawned = null;

  if (isDry) {
    runner = new FakeRunner(log);
    notes.push(
      "DRY RUN — every number below is synthetic, produced by the in-process FakeRunner. " +
        "It exercises the harness, not the runner.",
    );
    log("dry-run: using the in-process FakeRunner (no supervisor, no HTTP)");
  } else if (config.runnerUrl) {
    runner = new RunnerClient(config.runnerUrl, log);
    notes.push(`Attached to an existing runner at ${config.runnerUrl} (not spawned by the harness).`);
    log(`attaching to existing runner ${config.runnerUrl}`);
  } else {
    supervisor = new SupervisorClient(config.supervisor, log);
    const health = await supervisor.health().catch((e) => {
      throw new Error(
        `supervisor at ${config.supervisor} is unreachable (${e.message}). ` +
          `Start it with dev-start.ps1 -Supervisor.`,
      );
    });
    log(
      `supervisor up: status=${health?.status} build.available_slots=${health?.build?.available_slots}`,
    );
    if (health?.build?.error_detected) {
      notes.push(
        "Supervisor reported a prior build error at harness start " +
          `(${String(health?.build?.last_error ?? "").split("\n")[0].slice(0, 200)}).`,
      );
    }

    const body = {
      rebuild: Boolean(config.rebuild || config.gitRef || config.worktree),
      wait: true,
      wait_timeout_secs: config.spawnTimeoutSecs,
      requester_id: `perf-harness/${config.label}`,
      health_probe_timeout_ms: 30000,
      extra_env: { RUST_LOG: config.rustLog },
    };
    if (config.gitRef) body.git_ref = config.gitRef;
    if (config.worktree) body.worktree_path = config.worktree;
    if (config.useLkg) body.use_lkg = true;

    spawned = await supervisor.spawnTest(body, (config.spawnTimeoutSecs + 900) * 1000);
    const apiUrl = spawned?.api_url ?? `http://localhost:${spawned?.port}`;
    log(`temp runner ${spawned?.id} spawned on ${apiUrl}`);
    for (const w of spawned?.warnings ?? []) notes.push(`supervisor warning: ${w}`);
    if (spawned?.lkg_stale_warning?.stale) {
      notes.push(`supervisor LKG staleness: ${spawned.lkg_stale_warning.message}`);
    }
    runner = new RunnerClient(apiUrl, log);
  }

  const run = {
    label: config.label,
    startedAt,
    harnessVersion: 1,
    dryRun: isDry,
    generator: config.generator,
    soakMs: config.soakMs,
    settleMs: config.settleMs,
    sessionLevels: config.sessions,
    skipFrontend: Boolean(config.skipFrontend),
    rustLog: config.rustLog,
    notes,
    runner: {},
    levels: [],
  };

  try {
    const health = await runner.waitHealthy(120000);
    run.runner = {
      id: spawned?.id ?? config.runnerUrl ?? "dry-run",
      apiUrl: runner.baseUrl,
      buildId: health?.buildId ?? health?.data?.buildId ?? null,
      gitSha: health?.data?.gitSha ?? null,
      mainSha: health?.data?.mainSha ?? null,
      usedLkg: spawned?.used_lkg ?? null,
      source: spawned?.source ?? null,
      buildSha: spawned?.build_sha ?? null,
    };
    log(`runner healthy: buildId=${run.runner.buildId} gitSha=${run.runner.gitSha}`);

    const sink = await runner.logSink().catch((e) => {
      notes.push(`log-sink resolution failed (${e.message}); spawn spans will be unavailable.`);
      return null;
    });
    run.logSink = sink;
    const logSinkPath = sink?.current_file ?? null;
    if (!isDry && !logSinkPath) {
      notes.push("No runner log sink resolved — the spawn-segment breakdown will be empty.");
    }

    if (config.skipFrontend) {
      notes.push(
        "--skip-frontend: the UI Bridge probe was NOT installed and the terminal tab was not " +
          "activated. Frontend metrics are absent by request; backend metrics are unaffected.",
      );
      log("skipping the UI Bridge probe (--skip-frontend)");
    } else {
      // Put the UI on the terminal tab so the panes actually render; a hidden
      // panel would measure an idle page and understate every frontend metric.
      try {
        await runner.activateTab("terminal");
        log("ui-bridge: activated the terminal tab");
      } catch (e) {
        notes.push(`Could not activate the terminal tab via the UI Bridge: ${e.message}`);
      }

      try {
        const installed = await runner.evaluate(PROBE_INSTALL, 45000);
        log(`ui-bridge: probe installed (longTaskObserver=${installed?.longTaskObserver})`);
        if (installed?.errors?.length) {
          notes.push(`probe install warnings: ${installed.errors.join("; ")}`);
        }
        if (installed && installed.longTaskObserver === false) {
          notes.push(
            "PerformanceObserver('longtask') is unsupported in this webview — long-task counts " +
              "will be 0. Frame time from rAF is still valid.",
          );
        }
      } catch (e) {
        notes.push(`UI Bridge probe install FAILED (${e.message}) — frontend metrics unavailable.`);
      }
    }

    const existing = await runner.listTerminals().catch(() => []);
    if (existing.length) {
      log(`runner already has ${existing.length} terminal(s); levels are cumulative on top`);
      notes.push(`Runner had ${existing.length} pre-existing terminal(s) at harness start.`);
    }

    for (const target of config.sessions) {
      const level = await measureLevel({
        runner,
        target,
        existing,
        config,
        logSinkPath,
        isDry,
        log,
      });
      run.levels.push(level);
      log(
        `S=${target}: create p50=${fmt(level.terminalCreateMs.p50)}ms ` +
          `p95=${fmt(level.terminalCreateMs.p95)}ms | ` +
          `frame p95=${fmt(level.frontend?.frameTimeMs?.p95)}ms | ` +
          `longtasks=${level.frontend?.longTasks?.count ?? "?"} | ` +
          `spans=${level.spawnSpanSampleCount}`,
      );
    }

    const wedgedAt = run.levels.filter((l) => l.webviewWedged).map((l) => `S=${l.sessions}`);
    if (wedgedAt.length) {
      notes.push(
        `WEBVIEW WEDGED at ${wedgedAt.join(", ")} — the runner's webview stopped servicing IPC ` +
          "(Tauri: `failed to send message to the webview`) while the backend stayed healthy and " +
          "every PTY stayed alive. Frontend metrics from that level onward are UNKNOWN, not zero. " +
          "Re-run with --skip-frontend to confirm the probe is not the cause.",
      );
    }
    if (run.levels.every((l) => l.spawnSpanSampleCount === 0)) {
      notes.push(
        "No `terminal_spawn.*` spans were found in the runner log. Either the binary predates " +
          "the spans, or DEBUG is not enabled for `qontinui_runner::terminal::session` / " +
          "`qontinui_runner::commands::terminal`. The spawn-segment breakdown is therefore " +
          "UNKNOWN for this run — not zero.",
      );
    }
  } finally {
    if (!config.skipFrontend) {
      try {
        await runner.evaluate(PROBE_STOP, 15000);
      } catch {
        /* probe teardown is best effort; a dead webview is not a run failure */
      }
    }
    if (spawned?.id && !config.keepRunner) {
      try {
        await supervisor.stopRunner(spawned.id);
        log(`temp runner ${spawned.id} stopped`);
      } catch (e) {
        log(`WARNING: failed to stop temp runner ${spawned.id}: ${e.message}`);
        run.notes.push(`Temp runner ${spawned.id} may still be running: ${e.message}`);
      }
    } else if (spawned?.id) {
      log(`temp runner ${spawned.id} left running (--keep-runner)`);
    }
  }

  run.finishedAt = new Date().toISOString();
  return run;
}

const fmt = (v) => (Number.isFinite(v) ? v.toFixed(1) : "?");

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/**
 * @param {string[]} argv
 * @returns {Promise<number>} process exit code
 */
export async function main(argv) {
  let args;
  try {
    args = parseArgs(argv);
  } catch (e) {
    process.stderr.write(`perf-harness: ${e.message}\n\nRun with --help.\n`);
    return 2;
  }

  if (args.help) {
    process.stdout.write(`${HELP}\n`);
    return 0;
  }

  const quiet = Boolean(args.quiet) || Boolean(args.json);
  const log = (msg) => {
    if (!quiet) process.stderr.write(`[perf-harness] ${msg}\n`);
  };

  // --diff: pure rendering, no runner involved.
  if (args.diff) {
    const [beforePath, afterPath] = args.diff;
    const before = JSON.parse(readFileSync(beforePath, "utf8"));
    const after = JSON.parse(readFileSync(afterPath, "utf8"));
    const md = `${renderSingleRun(before)}\n\n${renderSingleRun(after)}\n\n${renderComparison(
      before,
      after,
      { minPctChange: Number(args["min-pct"] ?? DEFAULTS.minPctChange) },
    )}\n`;
    process.stdout.write(md);
    if (args.markdown) {
      mkdirSync(dirname(resolve(String(args.markdown))), { recursive: true });
      writeFileSync(String(args.markdown), md, "utf8");
      log(`wrote ${args.markdown}`);
    }
    return 0;
  }

  const label = String(args.label ?? (args["dry-run"] ? "dry-run" : "run"));
  const config = {
    dryRun: Boolean(args["dry-run"]),
    supervisor: String(args.supervisor ?? DEFAULTS.supervisor),
    runnerUrl: args["runner-url"] ? String(args["runner-url"]) : null,
    rebuild: Boolean(args.rebuild),
    gitRef: args["git-ref"] ? String(args["git-ref"]) : null,
    worktree: args.worktree ? String(args.worktree) : null,
    useLkg: Boolean(args["use-lkg"]),
    keepRunner: Boolean(args["keep-runner"]),
    skipFrontend: Boolean(args["skip-frontend"]),
    sessions: parseSessionLevels(args.sessions ? String(args.sessions) : undefined),
    generator: String(args.generator ?? DEFAULTS.generator),
    soakMs: Number(args["soak-ms"] ?? DEFAULTS.soakMs),
    settleMs: Number(args["settle-ms"] ?? DEFAULTS.settleMs),
    createTimeoutMs: DEFAULTS.createTimeoutMs,
    spawnTimeoutSecs: Number(args["spawn-timeout-secs"] ?? DEFAULTS.spawnTimeoutSecs),
    rustLog: String(args["rust-log"] ?? DEFAULTS.rustLog),
    label,
  };

  const run = await runHarness(config, log);

  const outPath = resolve(
    String(args.out ?? join(DEFAULTS.outDir, `${label}-${run.startedAt.replace(/[:.]/g, "-")}.json`)),
  );
  mkdirSync(dirname(outPath), { recursive: true });
  writeFileSync(outPath, `${JSON.stringify(run, null, 2)}\n`, "utf8");
  log(`wrote ${outPath}`);

  if (args.json) {
    process.stdout.write(`${JSON.stringify(run, null, 2)}\n`);
    return 0;
  }

  let md = renderSingleRun(run);
  if (args.baseline) {
    const baseline = JSON.parse(readFileSync(String(args.baseline), "utf8"));
    md = `${renderSingleRun(baseline)}\n\n${md}\n\n${renderComparison(baseline, run, {
      minPctChange: Number(args["min-pct"] ?? DEFAULTS.minPctChange),
    })}`;
  }
  md += `\n\nRun JSON: \`${outPath}\`\n`;
  process.stdout.write(`${md}\n`);

  if (args.markdown) {
    mkdirSync(dirname(resolve(String(args.markdown))), { recursive: true });
    writeFileSync(String(args.markdown), `${md}\n`, "utf8");
    log(`wrote ${args.markdown}`);
  }
  return 0;
}

// Only run when invoked directly, so the unit tests can import the module.
const invokedDirectly =
  process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (invokedDirectly) {
  main(process.argv.slice(2))
    .then((code) => {
      process.exitCode = code;
    })
    .catch((e) => {
      process.stderr.write(`perf-harness: ${e?.stack ?? e}\n`);
      process.exitCode = 1;
    });
}
