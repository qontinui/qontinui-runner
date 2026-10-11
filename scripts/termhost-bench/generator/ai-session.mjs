#!/usr/bin/env node
/**
 * termhost-bench AI-session generator — the deterministic, calibrated load one
 * benchmark session runs inside its host's terminal pane (plan D3, D13).
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 1.
 *
 * Every setup's session is the platform's default interactive shell running this
 * generator with ONE identical command line (D13):
 *
 *   node scripts/termhost-bench/generator/ai-session.mjs --run-dir <dir> --profile <p>
 *        --seed <s> --termhost-bench-generator <run-id>
 *
 * `--termhost-bench-generator <run-id>` is how the probe excludes generators from a
 * host's process tree, so never rewrite process.title. Because the command line is
 * identical, a generator learns its session index by claiming the first free
 * `<run-dir>/slot-<k>` with an exclusive create, and writes its PID, parent PID (the
 * session shell) and any host terminal id there.
 *
 * Output (stdout, fd 1): the profile's byte stream with a marker overlay re-drawn on
 * every write; markers advance at the profile's `marker_hz` (<= 10 Hz). Every write
 * goes through a blocking `fs.writeSync(1, ...)`, timed on both sides, so time a host
 * makes the producer wait is measured directly (`producer_stall_pct`). A generator
 * that falls behind writes the SAME bytes late — it never skips — and logs how far
 * behind it is.
 *
 * Records (`<run-dir>/gen-<slot>.jsonl`, one JSON object per line; absolute
 * process.hrtime nanosecond timestamps are DECIMAL STRINGS because they exceed 2^53):
 *   start     configuration, t0_ns, terminal size, console handling
 *   marker    {seq, t_sched_ns, t_emit_ns, t_written_ns, bytes, phase} on each tick
 *   echo      {seq, byte, t_read_ns, t_emit_ns, t_written_ns, bytes} (--echo)
 *   progress  cumulative counters every 500 ms (window any interval by differencing)
 *   exit      reason and final counters
 *
 * Flags beyond D13's: --echo (raw-mode stdin; each received byte is answered by an
 * echo marker in row 1), --calibration (a static calibration marker, no load),
 * --duration-ms N (stop after N ms; tests and manual runs).
 *
 * Exit: 0 on the stop file (`<run-dir>/stop`), stdin EOF, a signal, the stdout
 * reader going away, Ctrl+C in echo mode, or --duration-ms; 1 on a runtime error;
 * 2 on a usage error.
 */

import { spawnSync } from "node:child_process";
import { closeSync, existsSync, openSync, statSync, writeSync } from "node:fs";
import { isatty } from "node:tty";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { SYNC_END } from "../../tui-repaint-generator.mjs";
import {
  ECHO_REGION,
  MARKER_KINDS,
  MARKER_REGION,
  SESSION_LIMIT,
  markerOverlay,
  markerSeq,
} from "../marker.mjs";
import { loadProfile, profileDigest } from "../profile.mjs";
import { Schedule, buildContent, isSyncContent, knownContentSize } from "./schedule.mjs";

export const GEN_SCHEMA = "termhost-bench/gen/v1";
export const SLOT_SCHEMA = "termhost-bench/slot/v1";
export const STOP_FILE = "stop";
export const PROGRESS_INTERVAL_MS = 500;
export const STOP_CHECK_INTERVAL_MS = 50;
export const CALIBRATION_REDRAW_MS = 1000;
/** Queued writes beyond this many keep a descriptor only and are rebuilt when written. */
const RETAIN_QUEUED_CONTENT = 8;
const MS = 1_000_000n;
/** Environment variables that name the host's terminal/pane; never a wholesale env dump. */
export const HOST_TERMINAL_ENV = Object.freeze([
  "TERMHOST_BENCH_TERMINAL_ID",
  "QONTINUI_TERMINAL_ID",
  "WT_SESSION",
  "TMUX_PANE",
  "TERM_PROGRAM",
  "TERM_PROGRAM_VERSION",
]);
const CLOSED_STDOUT_CODES = new Set(["EPIPE", "EIO", "ENXIO", "ECONNRESET", "EBADF", "EOF"]);

export class UsageError extends Error {}

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

/**
 * @param {string[]} argv
 * @returns {{runDir: string|null, profile: string|null, seed: number|null, runId: string|null,
 *   echo: boolean, calibration: boolean, durationMs: number|null, help: boolean}}
 */
export function parseArgs(argv) {
  const opts = {
    runDir: null,
    profile: null,
    seed: null,
    runId: null,
    echo: false,
    calibration: false,
    durationMs: null,
    help: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    const value = () => {
      const v = argv[i + 1];
      if (v === undefined || v.startsWith("--")) throw new UsageError(`${a} needs a value`);
      i += 1;
      return v;
    };
    switch (a) {
      case "--run-dir":
        opts.runDir = value();
        break;
      case "--profile":
        opts.profile = value();
        break;
      case "--seed": {
        const v = value();
        if (!/^\d+$/.test(v) || Number(v) > 0xffffffff)
          throw new UsageError("--seed needs an integer in 0..4294967295");
        opts.seed = Number(v);
        break;
      }
      case "--termhost-bench-generator": {
        const v = value();
        if (!/^[A-Za-z0-9._-]{1,64}$/.test(v))
          throw new UsageError("--termhost-bench-generator needs a run id ([A-Za-z0-9._-]{1,64})");
        opts.runId = v;
        break;
      }
      case "--echo":
        opts.echo = true;
        break;
      case "--calibration":
        opts.calibration = true;
        break;
      case "--duration-ms": {
        const n = Number(value());
        if (!Number.isFinite(n) || n <= 0)
          throw new UsageError("--duration-ms needs a positive number");
        opts.durationMs = n;
        break;
      }
      case "--help":
      case "-h":
        opts.help = true;
        break;
      default:
        throw new UsageError(`unknown argument: ${a}`);
    }
  }
  if (!opts.help) {
    for (const [flag, key] of [
      ["--run-dir", "runDir"],
      ["--profile", "profile"],
      ["--seed", "seed"],
      ["--termhost-bench-generator", "runId"],
    ]) {
      if (opts[key] === null) throw new UsageError(`${flag} is required`);
    }
  }
  return opts;
}

export const USAGE =
  "usage: node scripts/termhost-bench/generator/ai-session.mjs --run-dir <dir> --profile <id|path> " +
  "--seed <0..4294967295> --termhost-bench-generator <run-id> [--echo] [--calibration] [--duration-ms N]\n";

// ---------------------------------------------------------------------------
// Clocks, writers, logs
// ---------------------------------------------------------------------------

/** process.hrtime clock; sleeps with setTimeout (so stdin and signals are serviced). */
export function createRealClock() {
  return {
    nowNs: () => process.hrtime.bigint(),
    sleepUntil(tNs) {
      const ms = Number(tNs - process.hrtime.bigint()) / 1e6;
      return new Promise((r) => setTimeout(r, Math.max(0, Math.ceil(ms))));
    },
  };
}

/**
 * A virtual clock: time moves only when the driver sleeps or a writer advances it.
 * Inputs scheduled with `scheduleInput` are delivered at their virtual time.
 */
export function createVirtualClock({ startNs = 0n } = {}) {
  let now = startNs;
  const inputs = [];
  let handler = null;
  return {
    nowNs: () => now,
    advance(ns) {
      now += BigInt(ns);
    },
    async sleepUntil(tNs) {
      while (inputs.length > 0 && inputs[0].tNs <= tNs) {
        const ev = inputs.shift();
        if (ev.tNs > now) now = ev.tNs;
        handler?.(ev.bytes, now);
      }
      if (tNs > now) now = tNs;
    },
    scheduleInput(tNs, bytes) {
      inputs.push({ tNs: BigInt(tNs), bytes: Buffer.from(bytes) });
      inputs.sort((a, b) => (a.tNs < b.tNs ? -1 : a.tNs > b.tNs ? 1 : 0));
    },
    setInputHandler(fn) {
      handler = fn;
    },
  };
}

const SLEEP_CELL = new Int32Array(new SharedArrayBuffer(4));

/** A synchronous sleep (EAGAIN back-off) that does not spin. */
export function blockingSleepMs(ms) {
  Atomics.wait(SLEEP_CELL, 0, 0, ms);
}

/**
 * A blocking writer for `fd`: retries partial writes and EAGAIN (a non-blocking fd
 * whose buffer is full) until every byte is written, so time blocked anywhere in
 * delivering the buffer is inside the caller's timing.
 *
 * @param {{fd?: number, writeSync?: Function, sleepMs?: (ms: number) => void}} [opts]
 * @returns {(buf: Buffer) => void}
 */
export function createFdWriter({
  fd = 1,
  writeSync: ws = writeSync,
  sleepMs = blockingSleepMs,
} = {}) {
  return (buf) => {
    let off = 0;
    while (off < buf.length) {
      let n;
      try {
        n = ws(fd, buf, off, buf.length - off);
      } catch (e) {
        if (e && (e.code === "EAGAIN" || e.code === "EWOULDBLOCK")) {
          sleepMs(1);
          continue;
        }
        throw e;
      }
      if (n > 0) off += n;
      else sleepMs(1);
    }
  };
}

/**
 * Append-only JSON-lines log; refuses to overwrite (the slot claim makes the name unique).
 *
 * @param {string} path
 */
export function createJsonlLog(path) {
  const fd = openSync(path, "wx");
  const log = (record) => {
    writeSync(fd, `${JSON.stringify(record)}\n`);
  };
  log.close = () => closeSync(fd);
  log.path = path;
  return log;
}

/** The host terminal ids present in `env` (allowlisted names only). */
export function hostTerminalIds(env = process.env) {
  const out = {};
  for (const key of HOST_TERMINAL_ENV)
    if (typeof env[key] === "string" && env[key] !== "") out[key] = env[key];
  return out;
}

/**
 * Claim the first free `<runDir>/slot-<k>` with an exclusive create and record who
 * holds it. k is the session index the markers carry (0..342).
 *
 * @param {string} runDir
 * @param {object} info written into the slot file
 * @returns {{slot: number, path: string}}
 */
export function claimSlot(runDir, info) {
  for (let k = 0; k < SESSION_LIMIT; k += 1) {
    const path = join(runDir, `slot-${k}`);
    let fd;
    try {
      fd = openSync(path, "wx");
    } catch (e) {
      if (e.code === "EEXIST") continue;
      throw e;
    }
    try {
      writeSync(fd, `${JSON.stringify({ schema: SLOT_SCHEMA, slot: k, ...info })}\n`);
    } finally {
      closeSync(fd);
    }
    return { slot: k, path };
  }
  throw new Error(`no free slot below ${SESSION_LIMIT} in ${runDir}`);
}

/**
 * Insert overlays into a write: before SYNC_END for a synchronized frame (so they
 * land inside the same frame), after the content otherwise.
 *
 * @param {string} content
 * @param {string} overlays
 * @param {boolean} sync
 * @returns {string}
 */
export function composeWrite(content, overlays, sync) {
  if (!content) return overlays;
  if (sync) {
    if (!content.endsWith(SYNC_END)) throw new Error("a synchronized frame must end with SYNC_END");
    return content.slice(0, -SYNC_END.length) + overlays + SYNC_END;
  }
  return content + overlays;
}

/** Stop flag shared by the driver, signal handlers and stdin. */
export function createControl() {
  return {
    stopped: false,
    reason: null,
    error: null,
    onInput: null,
    stop(reason, error = null) {
      if (this.stopped) return;
      this.stopped = true;
      this.reason = reason;
      this.error = error;
    },
  };
}

// ---------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------

/**
 * Run one session's load until a stop condition.
 *
 * @param {{
 *   profile: object, profileDigest?: string, seed: number, session: number, runId: string,
 *   clock: {nowNs: () => bigint, sleepUntil: (tNs: bigint) => Promise<void>},
 *   write: (buf: Buffer) => void, log: (record: object) => void,
 *   echo?: boolean, calibration?: boolean, durationMs?: number|null,
 *   shouldStop?: () => string|null, control?: ReturnType<typeof createControl>,
 *   startExtra?: object,
 * }} opts
 * @returns {Promise<object>} the exit record
 */
export async function runGenerator(opts) {
  const {
    profile,
    seed,
    session,
    runId,
    clock,
    write,
    log,
    echo = false,
    calibration = false,
    durationMs = null,
    shouldStop = () => null,
    control = createControl(),
    startExtra = {},
  } = opts;
  const geometry = profile.geometry;
  const t0 = clock.nowNs();
  const endNs = durationMs === null ? null : t0 + BigInt(Math.round(durationMs * 1e6));
  const st = {
    bytesScheduled: 0,
    bytesWritten: 0,
    writes: 0,
    blockedNs: 0n,
    markers: 0,
    echoes: 0,
    echoBytes: 0,
    schedPosUs: 0,
    echoOverlay: "",
  };

  log({
    type: "start",
    schema: GEN_SCHEMA,
    run_id: runId,
    session,
    mode: calibration ? "calibration" : "load",
    profile: profile.id,
    profile_digest: opts.profileDigest ?? profileDigest(profile),
    profile_calibrated: profile.calibrated,
    seed,
    geometry: { ...geometry },
    marker_hz: profile.marker_hz,
    echo,
    clock: "process.hrtime",
    t0_ns: String(t0),
    ...startExtra,
  });

  const timedWrite = (buf) => {
    const before = clock.nowNs();
    write(buf);
    const after = clock.nowNs();
    st.blockedNs += after - before;
    return [before, after];
  };
  const failWrite = (e) => {
    control.stop(e && CLOSED_STDOUT_CODES.has(e.code) ? "stdout_closed" : "error", e);
  };

  control.onInput = (buf, tReadNs) => {
    if (!echo) return;
    for (const byte of buf) {
      if (control.stopped) return;
      if (byte === 0x03) {
        control.stop("interrupt");
        return;
      }
      const seq = st.echoes;
      st.echoes += 1;
      st.echoOverlay = markerOverlay(
        { kind: MARKER_KINDS.echo, session, seq: markerSeq(seq) },
        ECHO_REGION,
      );
      const bytes = Buffer.from(st.echoOverlay, "latin1");
      try {
        const [before, after] = timedWrite(bytes);
        st.echoBytes += bytes.length;
        log({
          type: "echo",
          seq,
          byte,
          t_read_ns: String(tReadNs ?? before),
          t_emit_ns: String(before),
          t_written_ns: String(after),
          bytes: bytes.length,
        });
      } catch (e) {
        failWrite(e);
        return;
      }
    }
  };

  const preamble = `${profile.alt_screen ? "\x1b[?1049h" : ""}\x1b[H\x1b[2J`;
  try {
    timedWrite(Buffer.from(preamble, "latin1"));
    st.bytesScheduled += preamble.length;
    st.bytesWritten += preamble.length;
    st.writes += 1;
  } catch (e) {
    failWrite(e);
  }

  const queue = [];
  let nextProgressNs = t0 + BigInt(PROGRESS_INTERVAL_MS) * MS;
  let nextStopCheckNs = t0;
  const progress = (now) => {
    log({
      type: "progress",
      t_ns: String(now),
      sched_pos_us: st.schedPosUs,
      bytes_scheduled: st.bytesScheduled,
      bytes_written: st.bytesWritten,
      writes: st.writes,
      blocked_ns: Number(st.blockedNs),
      echo_bytes: st.echoBytes,
      queued: queue.length,
    });
  };
  const pollStop = (now) => {
    if (endNs !== null && now >= endNs) control.stop("duration");
    if (!control.stopped && now >= nextStopCheckNs) {
      const reason = shouldStop();
      if (reason) control.stop(reason);
      nextStopCheckNs = now + BigInt(STOP_CHECK_INTERVAL_MS) * MS;
    }
  };

  if (calibration) {
    await calibrationLoop({
      ...opts,
      st,
      t0,
      timedWrite,
      failWrite,
      control,
      pollStop,
      endNs,
      clock,
      log,
    });
  } else {
    const schedule = new Schedule({ profile, seed, session });
    let pending = schedule.next();
    const dueAt = (w) => t0 + BigInt(w.tUs) * 1000n;
    const materialize = (w) => {
      const overlay = markerOverlay(
        { kind: MARKER_KINDS.output, session, seq: markerSeq(w.seq) },
        MARKER_REGION,
      );
      let content = null;
      let contentBytes = 0;
      if (w.content) {
        const known = knownContentSize(w.content);
        if (known !== null) {
          contentBytes = known;
        } else {
          content = buildContent(w.content, geometry);
          contentBytes = Buffer.byteLength(content, "utf8");
          if (queue.length >= RETAIN_QUEUED_CONTENT) content = null;
        }
      }
      const detBytes = contentBytes + overlay.length;
      st.bytesScheduled += detBytes;
      return { w, content, overlay, detBytes };
    };
    const pullDue = (now) => {
      while (dueAt(pending) <= now) {
        queue.push(materialize(pending));
        pending = schedule.next();
      }
    };
    const perform = (item) => {
      const { w } = item;
      const content = w.content ? (item.content ?? buildContent(w.content, geometry)) : "";
      const text = composeWrite(
        content,
        item.overlay + st.echoOverlay,
        w.content ? isSyncContent(w.content) : false,
      );
      const bytes = Buffer.from(text, "utf8");
      const [before, after] = timedWrite(bytes);
      st.writes += 1;
      st.bytesWritten += item.detBytes;
      st.echoBytes += bytes.length - item.detBytes;
      st.schedPosUs = w.tUs;
      if (w.newSeq !== null) {
        st.markers += 1;
        log({
          type: "marker",
          kind: "output",
          seq: w.newSeq,
          phase: w.phase.id,
          t_sched_ns: String(dueAt(w)),
          t_emit_ns: String(before),
          t_written_ns: String(after),
          bytes: bytes.length,
        });
      }
    };

    while (!control.stopped) {
      const now = clock.nowNs();
      // A run of duration D owns exactly the writes due before D.
      if (endNs !== null && now >= endNs) {
        control.stop("duration");
        break;
      }
      pullDue(now);
      if (now >= nextProgressNs) {
        progress(now);
        while (nextProgressNs <= now) nextProgressNs += BigInt(PROGRESS_INTERVAL_MS) * MS;
      }
      pollStop(now);
      if (control.stopped) break;
      if (queue.length > 0) {
        try {
          perform(queue.shift());
        } catch (e) {
          failWrite(e);
        }
        continue;
      }
      let wake = dueAt(pending);
      if (nextProgressNs < wake) wake = nextProgressNs;
      if (nextStopCheckNs < wake) wake = nextStopCheckNs;
      if (endNs !== null && endNs < wake) wake = endNs;
      await clock.sleepUntil(wake);
    }
    // Count everything the schedule called for up to the stop, written or not
    // (for a duration stop, everything due strictly before the end).
    pullDue(control.reason === "duration" ? endNs - 1n : clock.nowNs());
  }

  const now = clock.nowNs();
  const elapsedUs = Number((now - t0) / 1000n);
  const postamble = `\x1b[0m${profile.alt_screen ? "\x1b[?1049l" : ""}\r\n`;
  try {
    write(Buffer.from(postamble, "latin1"));
  } catch {
    // the reader is gone; nothing left to restore
  }
  control.onInput = null;
  const exit = {
    type: "exit",
    reason: control.reason ?? "error",
    t_ns: String(now),
    wall_ns: Number(now - t0),
    bytes_scheduled: st.bytesScheduled,
    bytes_written: st.bytesWritten,
    writes: st.writes,
    blocked_ns: Number(st.blockedNs),
    markers: st.markers,
    echoes: st.echoes,
    echo_bytes: st.echoBytes,
    sched_pos_us: st.schedPosUs,
    queued: queue.length,
    lag_us: queue.length > 0 ? Math.max(0, elapsedUs - queue[0].w.tUs) : 0,
  };
  if (control.error) exit.error = String(control.error.message ?? control.error);
  log(exit);
  return exit;
}

async function calibrationLoop({
  session,
  st,
  t0,
  timedWrite,
  failWrite,
  control,
  pollStop,
  endNs,
  clock,
  log,
}) {
  const overlay = markerOverlay({ kind: MARKER_KINDS.calibration, session, seq: 0 }, MARKER_REGION);
  const bytes = Buffer.from(overlay, "latin1");
  let nextDraw = t0;
  let first = true;
  while (!control.stopped) {
    const now = clock.nowNs();
    pollStop(now);
    if (control.stopped) break;
    if (now >= nextDraw) {
      try {
        const [before, after] = timedWrite(bytes);
        st.writes += 1;
        st.bytesScheduled += bytes.length;
        st.bytesWritten += bytes.length;
        if (first) {
          st.markers += 1;
          log({
            type: "marker",
            kind: "calibration",
            seq: 0,
            phase: "calibration",
            t_sched_ns: String(t0),
            t_emit_ns: String(before),
            t_written_ns: String(after),
            bytes: bytes.length,
          });
          first = false;
        }
      } catch (e) {
        failWrite(e);
        break;
      }
      nextDraw += BigInt(CALIBRATION_REDRAW_MS) * MS;
    }
    let wake = nextDraw;
    const check = clock.nowNs() + BigInt(STOP_CHECK_INTERVAL_MS) * MS;
    if (check < wake) wake = check;
    if (endNs !== null && endNs < wake) wake = endNs;
    await clock.sleepUntil(wake);
  }
}

/**
 * Run the generator against a virtual clock, collecting every write — the
 * deterministic harness for tests and offline analysis.
 *
 * @param {{profile: object, seed?: number, session?: number, durationMs: number,
 *   writeDelayNs?: number | bigint | ((buf: Buffer) => number | bigint),
 *   echo?: boolean, calibration?: boolean, inputs?: {tNs: number|bigint, bytes: string|Buffer}[]}} opts
 */
export async function runVirtual({
  profile,
  seed = 1,
  session = 0,
  durationMs,
  writeDelayNs = 0,
  echo = false,
  calibration = false,
  inputs = [],
}) {
  const clock = createVirtualClock();
  const writes = [];
  const records = [];
  const control = createControl();
  const write = (buf) => {
    writes.push({ tNs: clock.nowNs(), bytes: Buffer.from(buf) });
    const d = typeof writeDelayNs === "function" ? writeDelayNs(buf) : writeDelayNs;
    if (d) clock.advance(d);
  };
  for (const ev of inputs) clock.scheduleInput(ev.tNs, ev.bytes);
  clock.setInputHandler((bytes, t) => control.onInput?.(bytes, t));
  const exit = await runGenerator({
    profile,
    seed,
    session,
    runId: "virtual",
    clock,
    write,
    log: (r) => records.push(r),
    echo,
    calibration,
    durationMs,
    control,
  });
  // The postamble is the last write; it is outside the measured stream.
  const stream = writes.slice(0, -1);
  return { writes: stream, records, exit, bytes: Buffer.concat(stream.map((w) => w.bytes)) };
}

// ---------------------------------------------------------------------------
// Windows console
// ---------------------------------------------------------------------------

/** The code page in `chcp` output ("Active code page: 437", localized). */
export function parseCodePage(text) {
  const m = /(\d+)\D*$/.exec(String(text ?? "").trim());
  return m ? Number(m[1]) : null;
}

/**
 * On a Windows console, make the byte stream mean the same in every host: let libuv
 * enable VT processing (creating process.stdout does it) and switch the console's
 * output code page to UTF-8, because a raw fs.writeSync to a console is decoded in
 * the console code page (437 by default) and every multi-byte glyph would become
 * several cells. The previous code page is restored at exit.
 *
 * @param {{platform?: string, stdoutIsTty?: boolean, run?: typeof spawnSync, env?: object}} [deps]
 * @returns {{applied: boolean, cp_before?: number|null, cp_after?: number|null, chcp?: string}}
 */
export function prepareConsole({
  platform = process.platform,
  stdoutIsTty = isatty(1),
  run = spawnSync,
  env = process.env,
} = {}) {
  if (platform !== "win32" || !stdoutIsTty) return { applied: false };
  void process.stdout;
  const chcp = join(env.SystemRoot ?? "C:\\Windows", "System32", "chcp.com");
  const read = () =>
    parseCodePage(
      run(chcp, [], { encoding: "utf8", windowsHide: false, stdio: ["ignore", "pipe", "ignore"] })
        .stdout,
    );
  const before = read();
  let after = before;
  if (before !== 65001) {
    run(chcp, ["65001"], { windowsHide: false, stdio: "ignore" });
    after = read();
  }
  return { applied: true, cp_before: before, cp_after: after, chcp };
}

/** Undo prepareConsole's code-page change. */
export function restoreConsole(state, { run = spawnSync } = {}) {
  if (state?.applied && Number.isInteger(state.cp_before) && state.cp_before !== state.cp_after) {
    run(state.chcp, [String(state.cp_before)], { windowsHide: false, stdio: "ignore" });
  }
}

function terminalSize() {
  if (!isatty(1)) return null;
  const { columns, rows } = process.stdout;
  return Number.isInteger(columns) && Number.isInteger(rows) ? { cols: columns, rows } : null;
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

async function main(argv) {
  let opts;
  try {
    opts = parseArgs(argv);
  } catch (e) {
    process.stderr.write(`ai-session: ${e.message}\n${USAGE}`);
    return 2;
  }
  if (opts.help) {
    process.stderr.write(USAGE);
    return 0;
  }
  const runDir = resolve(opts.runDir);
  if (!existsSync(runDir) || !statSync(runDir).isDirectory()) {
    process.stderr.write(`ai-session: --run-dir ${runDir} is not a directory\n`);
    return 1;
  }
  let loaded;
  try {
    loaded = loadProfile(opts.profile);
  } catch (e) {
    process.stderr.write(`ai-session: ${e.message}\n`);
    return 1;
  }
  const clock = createRealClock();
  const consoleState = prepareConsole();
  const { slot, path: slotPath } = claimSlot(runDir, {
    run_id: opts.runId,
    pid: process.pid,
    ppid: process.ppid,
    host_terminal_id: hostTerminalIds(),
    claimed_at_ns: String(clock.nowNs()),
    argv: process.argv.slice(1),
  });
  const log = createJsonlLog(join(runDir, `gen-${slot}.jsonl`));
  const control = createControl();
  for (const sig of ["SIGINT", "SIGTERM", "SIGHUP", "SIGBREAK"]) {
    try {
      process.on(sig, () => control.stop(`signal:${sig}`));
    } catch {
      // a signal this platform does not have
    }
  }
  const stdin = process.stdin;
  let raw = false;
  if (opts.echo && stdin.isTTY && typeof stdin.setRawMode === "function") {
    stdin.setRawMode(true);
    raw = true;
  }
  stdin.on("data", (buf) => control.onInput?.(buf, clock.nowNs()));
  stdin.on("end", () => control.stop("stdin_eof"));
  stdin.on("error", () => control.stop("stdin_eof"));
  stdin.resume();

  const stopPath = join(runDir, STOP_FILE);
  const exit = await runGenerator({
    profile: loaded.profile,
    profileDigest: loaded.digest,
    seed: opts.seed,
    session: slot,
    runId: opts.runId,
    clock,
    write: createFdWriter({ fd: 1 }),
    log,
    echo: opts.echo,
    calibration: opts.calibration,
    durationMs: opts.durationMs,
    shouldStop: () => (existsSync(stopPath) ? "stop_file" : null),
    control,
    startExtra: {
      pid: process.pid,
      ppid: process.ppid,
      slot_path: slotPath,
      platform: process.platform,
      node: process.version,
      stdout_tty: isatty(1),
      terminal_size: terminalSize(),
      console: consoleState,
      host_terminal_id: hostTerminalIds(),
    },
  });
  if (raw) stdin.setRawMode(false);
  stdin.pause();
  restoreConsole(consoleState);
  log.close();
  if (exit.reason === "error") {
    process.stderr.write(`ai-session: ${exit.error ?? "failed"}\n`);
    return 1;
  }
  return 0;
}

const invokedDirectly =
  process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (invokedDirectly) {
  main(process.argv.slice(2)).then(
    (code) => process.exit(code),
    (e) => {
      process.stderr.write(`ai-session: ${e?.stack ?? e}\n`);
      process.exit(1);
    },
  );
}
