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
 *     live there, and `inject` announces each key it is about to send in
 *     `<gen-dir>/inject.jsonl`, which `watch` reads to tell the probe's own keys
 *     from an operator's. So the input phase's `watch` and `inject` share the
 *     directory, and on Windows a `--tag` too.
 *   - Every measuring `watch` is given the host's root PIDs (`--host-pid`,
 *     repeatable) and the generators' run id (`--run-id`): without live roots the
 *     probe samples no host group at all, so the host KPIs are unknown, never 0.
 *   - A `watch` ends when its stdin closes (`--stdin-stop`), never through the stop
 *     file — `<gen-dir>/stop` also stops the generators.
 *   - The probe writes one run per file: `watch` refuses a `probe.jsonl` that
 *     already holds records, and `inject` an `inject.jsonl` (`records.rs`
 *     `FileMode::New`). The generators keep running across a unit's phases, so
 *     the phases share the gen dir, and each phase's file is set aside — renamed
 *     to `probe-<label>.jsonl` / `inject-<label>.jsonl` — once the process that
 *     wrote it has exited, before the next one starts.
 *   - A short "look" (`watch --duration-ms`, frames on change, no process sampler)
 *     in its own scratch directory answers "which sessions are visible, where" for
 *     zone resolution and arrangement checks; an activation (`inject --count 0
 *     --activate`) gets its own directory too.
 *
 * Every process here is started with an argv array, never a shell string.
 */

import { spawn, spawnSync } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync, renameSync, statSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { PROBE_SOURCES } from "./kpi-catalog.mjs";
import { parseJsonl } from "./kpis.mjs";

export { PROBE_SOURCES };

const HERE = dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = resolve(HERE, "..", "..");

/**
 * The primary checkout of a linked worktree (the directory holding the shared
 * `.git`), from `git rev-parse --git-common-dir`; null in the primary itself, in
 * a bare repository, or when git cannot answer.
 *
 * @param {string} repoRoot
 * @param {{gitCommonDir?: (dir: string) => string | null}} [o]
 */
export function primaryCheckoutRoot(repoRoot, { gitCommonDir = readGitCommonDir } = {}) {
  const common = gitCommonDir(repoRoot);
  if (!common) return null;
  const abs = resolve(repoRoot, common);
  if (basename(abs) !== ".git") return null;
  const root = dirname(abs);
  return root === resolve(repoRoot) ? null : root;
}

/** `git -C <dir> rev-parse --git-common-dir`, the same form run.mjs `harnessCommit` uses. */
export function readGitCommonDir(dir, run = spawnSync) {
  const r = run("git", ["-C", dir, "rev-parse", "--git-common-dir"], { encoding: "utf8", timeout: 10000 });
  const out = String(r.stdout ?? "").trim();
  return r.status === 0 && out !== "" ? out : null;
}


/**
 * Every path the probe binary is looked for at, in order, each with its source.
 * `--probe` or `$TERMHOST_PROBE`, when given, is the ONLY candidate: a named
 * binary that is missing is an error, never a silent fallback to another build.
 * Otherwise: this checkout's `target/{debug,release}` and
 * `target-agent/{debug,release}`, then — from a linked worktree — the primary
 * checkout's `target-agent/{debug,release}`, where cargo-guard.sh builds a
 * worktree's crates (its shared TARGET_DIR, built from whichever worktree built
 * last: `primary_shared`, provenance unproven).
 *
 * @param {{explicit?: string | null, env?: object, platform?: string, repoRoot?: string,
 *   primaryRoot?: string | null}} [o]
 * @returns {{path: string, source: string}[]}
 */
export function probeCandidateList({ explicit = null, env = process.env, platform = process.platform, repoRoot = REPO_ROOT, primaryRoot = null } = {}) {
  if (explicit) return [{ path: explicit, source: "explicit" }];
  if (env.TERMHOST_PROBE) return [{ path: env.TERMHOST_PROBE, source: "env" }];
  const exe = platform === "win32" ? "termhost-probe.exe" : "termhost-probe";
  const own = [
    ["target", "debug"],
    ["target", "release"],
    ["target-agent", "debug"],
    ["target-agent", "release"],
  ].map((parts) => ({ path: join(repoRoot, ...parts, exe), source: "checkout" }));
  const shared = primaryRoot
    ? [
        ["target-agent", "debug"],
        ["target-agent", "release"],
      ].map((parts) => ({ path: join(primaryRoot, ...parts, exe), source: "primary_shared" }))
    : [];
  return [...own, ...shared];
}

/** The candidate paths alone (see `probeCandidateList`). */
export function probeCandidates(o = {}) {
  return probeCandidateList(o).map((c) => c.path);
}

/**
 * The probe binary: the first candidate that exists, with its source and every
 * path tried (for the doctor); `path` null when none does.
 */
export function locateProbeBinary({
  explicit = null,
  env = process.env,
  platform = process.platform,
  repoRoot = REPO_ROOT,
  primaryRoot,
  gitCommonDir,
  exists = existsSync,
} = {}) {
  const named = Boolean(explicit || env.TERMHOST_PROBE);
  const primary = named ? null : primaryRoot !== undefined ? primaryRoot : primaryCheckoutRoot(repoRoot, gitCommonDir ? { gitCommonDir } : {});
  const list = probeCandidateList({ explicit, env, platform, repoRoot, primaryRoot: primary });
  const tried = list.map((c) => c.path);
  for (const c of list) if (exists(c.path)) return { path: resolve(c.path), source: c.source, tried };
  return { path: null, source: named ? list[0].source : null, tried };
}

/** The probe binary path, or null (see `locateProbeBinary`). */
export function resolveProbeBinary(o = {}) {
  return locateProbeBinary(o).path;
}

/**
 * Pin the run's probe: copy the found binary into the run directory (keeping its
 * name, so a Windows `.exe` stays one, and its mode, so the exec bit survives) and
 * hash the COPY. The doctor and every later probe call run the copy, so a shared
 * build rewritten mid-run (another worktree building into the primary's
 * target-agent) changes neither what this run executes nor what it records.
 *
 * @param {string} src the found probe binary
 * @param {string} runDir
 * @returns {{path: string, size: number, sha256: string, source_mtime: string}}
 */
export function pinProbe(src, runDir) {
  const st = statSync(src);
  const dir = join(runDir, "probe-bin");
  mkdirSync(dir, { recursive: true });
  const dst = join(dir, basename(src));
  copyFileSync(src, dst);
  chmodSync(dst, st.mode & 0o7777);
  const bytes = readFileSync(dst);
  return { path: dst, size: bytes.length, sha256: `sha256:${createHash("sha256").update(bytes).digest("hex")}`, source_mtime: st.mtime.toISOString() };
}

/**
 * Which probe binary a run used, for the results' `run.probe`: the ORIGINAL path
 * and source, the pinned copy it executed (`executed_path`), the copy's size and
 * sha256, the original's mtime when it was copied, and the codec version its
 * doctor reported. The probe reports no build commit (only its crate version, in
 * start records), so `build` is an honest unknown rather than the harness's commit.
 *
 * @param {{path: string | null, source: string | null}} found `locateProbeBinary` output
 * @param {{pinned?: ReturnType<typeof pinProbe> | null, codecVersion?: number | null}} [o]
 */
export function probeIdentity(found, { pinned = null, codecVersion = null } = {}) {
  const absent = (detail) => ({ state: "unknown", reason: "harness_error", detail });
  const notPinned = absent(found?.path ? "the probe was not pinned" : "no probe binary found");
  return {
    path: found?.path ?? absent("no probe binary found"),
    source: PROBE_SOURCES.includes(found?.source) ? found.source : absent("unknown source"),
    executed_path: pinned?.path ?? notPinned,
    size_bytes: pinned?.size ?? notPinned,
    mtime: pinned?.source_mtime ?? notPinned,
    sha256: pinned?.sha256 ?? notPinned,
    codec_version: Number.isInteger(codecVersion) ? codecVersion : absent("the probe's doctor reported no codec"),
    build: { state: "unknown", reason: "not_instrumented", detail: "termhost-probe reports no build commit (only its crate version, in start records)" },
  };
}

/** The golden file's codec, in the shape the probe's doctor record reports it. */
export function goldenCodec(golden = JSON.parse(readFileSync(join(HERE, "marker-golden.json"), "utf8"))) {
  const c = golden.codec;
  return {
    version: c.version,
    pixel: {
      channel_low_max: c.pixel.channel_low_max,
      channel_high_min: c.pixel.channel_high_min,
      width_tolerance: c.pixel.width_tolerance,
      boundary_gap_max: c.pixel.boundary_gap_max,
    },
    row_reasons: c.pixel.row_reasons,
    decode_order: c.decode_order,
  };
}

/**
 * Where the probe's codec (its doctor record's `codec`, compiled in from
 * marker-golden.json at its build) differs from this harness's golden file:
 * one line per differing field, empty when they agree. A probe built from
 * another commit would otherwise decode frames under the wrong rules, silently.
 */
export function codecMismatch(probeCodec, golden = goldenCodec()) {
  if (!probeCodec || typeof probeCodec !== "object") return ["the probe's doctor record carries no codec"];
  const out = [];
  const cmp = (label, a, b) => {
    if (JSON.stringify(a) !== JSON.stringify(b)) out.push(`${label}: probe ${JSON.stringify(a)}, golden ${JSON.stringify(b)}`);
  };
  cmp("version", probeCodec.version, golden.version);
  for (const k of Object.keys(golden.pixel)) cmp(`pixel.${k}`, probeCodec.pixel?.[k], golden.pixel[k]);
  cmp("row_reasons", probeCodec.row_reasons, golden.row_reasons);
  cmp("decode_order", probeCodec.decode_order, golden.decode_order);
  return out;
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
  if (o.crosscheckProfile) args.push("--crosscheck-profile", o.crosscheckProfile);
  return args;
}

/**
 * argv for `inject` (D9). Keys are letters only (`--count`: a-z, repeating), so no
 * injected key is Ctrl+C or anything a shell would act on. `inject` first waits
 * `--quiet-ms` (default 2000) for a desktop nobody is using.
 *
 * @param {{runDir: string, target: object, count: number, intervalMs?: number,
 *   startDelayMs?: number, settleMs?: number, activate?: boolean, tag?: string,
 *   quietMs?: number}} o
 */
export function injectArgs(o) {
  if (!Number.isInteger(o.count) || o.count < 0) throw new RangeError(`inject count must be >= 0, got ${o.count}`);
  const args = ["inject", "--run-dir", o.runDir, ...targetArgs(o.target), "--count", String(o.count)];
  if (o.intervalMs !== undefined) args.push("--interval-ms", String(o.intervalMs));
  if (o.startDelayMs !== undefined) args.push("--start-delay-ms", String(o.startDelayMs));
  if (o.settleMs !== undefined) args.push("--settle-ms", String(o.settleMs));
  if (o.activate) args.push("--activate");
  if (o.tag) args.push("--tag", o.tag);
  if (o.quietMs !== undefined) args.push("--quiet-ms", String(o.quietMs));
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

const LABEL = /^[A-Za-z0-9._-]{1,64}$/;

/**
 * `promise`, or "timeout" after `ms` — with the timer CLEARED either way, so a
 * finished race never keeps the process alive (or a test waiting) for the rest
 * of the timeout.
 *
 * @template T
 * @param {Promise<T>} promise
 * @param {number} ms
 * @returns {Promise<T | "timeout">}
 */
export function raceTimeout(promise, ms, { setTimer = setTimeout, clearTimer = clearTimeout } = {}) {
  let timer;
  const timeout = new Promise((r) => {
    timer = setTimer(() => r("timeout"), ms);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimer(timer));
}

/**
 * End our own child by its handle: SIGTERM, then SIGKILL if it is still there
 * after `graceMs` (a child that ignores SIGTERM must not be waited on forever).
 *
 * @returns {Promise<"exited" | "killed" | "survived">}
 */
export async function endChild(child, exited, { graceMs = 5000 } = {}) {
  try {
    child.kill("SIGTERM");
  } catch {
    // already gone
  }
  if ((await raceTimeout(exited, graceMs)) !== "timeout") return "exited";
  try {
    child.kill("SIGKILL");
  } catch {
    // already gone
  }
  return (await raceTimeout(exited, graceMs)) !== "timeout" ? "killed" : "survived";
}

/**
 * Set a finished run's file aside: `<runDir>/<name>.jsonl` becomes
 * `<runDir>/<name>-<label>.jsonl` (with a counter if that exists), so the next
 * `watch` or `inject` in the directory starts on no file. Call it only after the
 * process that wrote the file has exited.
 *
 * @returns {string | null} the new path, or null when there was no file
 */
export function setAside(runDir, name, label) {
  if (!LABEL.test(label)) throw new RangeError(`bad run-file label ${JSON.stringify(label)}`);
  const src = join(runDir, `${name}.jsonl`);
  if (!existsSync(src)) return null;
  let dst = join(runDir, `${name}-${label}.jsonl`);
  for (let i = 1; existsSync(dst); i += 1) dst = join(runDir, `${name}-${label}-${i}.jsonl`);
  renameSync(src, dst);
  return dst;
}

/**
 * Read a finished run's file and set it aside.
 *
 * @returns {{records: object[], path: string | null, error?: string}}
 */
export function takeRunFile(runDir, name, label) {
  const { records } = readRecordsFrom(join(runDir, `${name}.jsonl`));
  try {
    return { records, path: setAside(runDir, name, label) };
  } catch (e) {
    return { records, path: null, error: `could not set ${name}.jsonl aside: ${e.message}` };
  }
}

/** A fresh injection tag (0x-hex; never 0, which an untagged injection carries). */
export function freshTag(random = () => randomBytes(4).readUInt32BE(0)) {
  let t = 0;
  while (t === 0) t = random() >>> 0;
  return `0x${t.toString(16).padStart(8, "0")}`;
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
 * (`--stdin-stop`), the probe writes its summary and exits, and the phase's
 * records are read back from `<runDir>/probe.jsonl`, which is then set aside as
 * `probe-<label>.jsonl` so the next phase's watch starts on no file.
 */
export class WatchProcess {
  /**
   * @param {{probe: string, args: string[], runDir: string, label: string, env?: object,
   *   spawnFn?: typeof spawn, log?: (s: string) => void}} o
   */
  constructor({ probe, args, runDir, label, env = process.env, spawnFn = spawn, log = () => {}, killGraceMs = 5000 }) {
    if (!args.includes("--stdin-stop")) throw new Error("a WatchProcess must run with --stdin-stop");
    if (!LABEL.test(label ?? "")) throw new RangeError(`bad watch label ${JSON.stringify(label)}`);
    this.runDir = runDir;
    this.label = label;
    this.killGraceMs = killGraceMs;
    this.probeJsonl = join(runDir, "probe.jsonl");
    if (fileSize(this.probeJsonl) > 0) {
      throw new Error(`${this.probeJsonl} already holds a run's records: the previous watch's file was not set aside`);
    }
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
      if (readRecordsFrom(this.probeJsonl).records.some((r) => r?.type === "start")) return true;
      await sleepFn(100);
    }
    return false;
  }

  /**
   * End the watch, collect its records and set its file aside.
   *
   * @returns {Promise<{records: object[], exit: object, stderr: string, timedOut: boolean,
   *   path: string | null, error?: string}>}
   */
  async stop({ timeoutMs = 20000 } = {}) {
    try {
      this.child.stdin?.end();
    } catch {
      // already closed
    }
    let timedOut = false;
    if ((await raceTimeout(this.exited, timeoutMs)) === "timeout") {
      timedOut = true;
      this.log(`watch pid ${this.pid} did not exit ${timeoutMs} ms after stdin closed; ending our child by its handle`);
      // Our own child, by its handle: Node signals exactly this PID.
      const how = await endChild(this.child, this.exited, { graceMs: this.killGraceMs });
      if (how === "survived") this.log(`watch pid ${this.pid} survived SIGTERM and SIGKILL`);
    }
    const taken = takeRunFile(this.runDir, "probe", this.label);
    if (taken.error) this.log(`watch pid ${this.pid}: ${taken.error}`);
    return {
      records: taken.records,
      exit: this.exit,
      stderr: this.stderr,
      timedOut,
      path: taken.path,
      ...(taken.error ? { error: taken.error } : {}),
    };
  }
}

/**
 * Run a probe subcommand to completion (inject, a timed look).
 *
 * @returns {Promise<{code: number | null, stdout: string, stderr: string}>}
 */
export function runProbe({ probe, args, env = process.env, timeoutMs = 120000, spawnFn = spawn, killGraceMs = 5000 }) {
  return new Promise((resolveRun) => {
    const child = spawnFn(probe, args, { env, stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
    let stdout = "";
    let stderr = "";
    let settled = false;
    const finish = (r) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolveRun(r);
    };
    const exited = new Promise((r) => child.on("exit", (code) => r(code)));
    child.stdout?.on("data", (d) => {
      stdout = (stdout + d.toString("utf8")).slice(-64 * 1024);
    });
    child.stderr?.on("data", (d) => {
      stderr = (stderr + d.toString("utf8")).slice(-16 * 1024);
    });
    const timer = setTimeout(async () => {
      stderr += `\n(termhost-bench: ${args[0]} exceeded ${timeoutMs} ms; ending our child by its handle)`;
      // SIGTERM, then SIGKILL: a child that ignores the first never holds the run.
      const how = await endChild(child, exited, { graceMs: killGraceMs });
      if (how === "survived") finish({ code: null, stdout, stderr: `${stderr}\n(termhost-bench: our child survived SIGTERM and SIGKILL)` });
    }, timeoutMs);
    exited.then((code) => finish({ code, stdout, stderr }));
    child.on("error", (e) => finish({ code: null, stdout, stderr: `${stderr}\n${e.message}` }));
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
  // Desktop Duplication is the cross-check, never the primary source (Phase 2 S6).
  const primary = opened.find((s) => s !== "dda") ?? null;
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
 * --activate`, in a directory of its own (`inject` writes one run per file). D9
 * still applies: after the quiet period the probe refuses when real input is
 * heard. A failed activation is not an exit status — `inject` records it as an
 * incident and completes — so the records are read back.
 *
 * @returns {Promise<{ok: boolean, detail: string}>}
 */
export async function activate({ probe, runDir, target, env = process.env, run = runProbe }) {
  if (target?.pid === undefined && target?.hwnd === undefined) {
    return { ok: false, detail: "activation needs a pid or hwnd target" };
  }
  mkdirSync(runDir, { recursive: true });
  const args = injectArgs({ runDir, target, count: 0, settleMs: 0, activate: true });
  const res = await run({ probe, args, env, timeoutMs: 30000 });
  const { records } = readRecordsFrom(join(runDir, "inject.jsonl"));
  const failed = records.find((r) => r?.type === "incident" && /--activate/.test(String(r.detail ?? "")));
  const abort = records.find((r) => r?.type === "inject_abort");
  if (res.code === 0 && !failed) return { ok: true, detail: "target activated" };
  return {
    ok: false,
    detail: failed
      ? String(failed.detail)
      : abort
        ? `inject --activate refused (${abort.reason}): ${abort.detail}`
        : `inject --activate exited ${res.code}: ${`${res.stdout}${res.stderr}`.slice(-400)}`,
  };
}
