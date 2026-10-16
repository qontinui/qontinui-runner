#!/usr/bin/env node
/**
 * termhost-bench AI-session generator — the deterministic, calibrated load one
 * benchmark session runs inside its host's terminal pane (plan D3, D13).
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 1.
 *
 * Every setup's session is the platform's default interactive shell running this
 * generator with ONE identical command line (D13). Every session runs with --echo:
 * only the focused pane ever receives input, so the others behave as without it.
 *
 *   node scripts/termhost-bench/generator/ai-session.mjs --run-dir <dir> --profile <p>
 *        --seed <s> --echo --termhost-bench-generator <run-id>
 *
 * `--termhost-bench-generator <run-id>` is how the probe excludes generators from a
 * host's process tree, so never rewrite process.title. Because the command line is
 * identical, a generator learns its session index by claiming the first free
 * `<run-dir>/slot-<k>` with an exclusive create, and writes its PID, parent PID (the
 * session shell) and any host terminal id there.
 *
 * Output (stdout, fd 1): the profile's byte stream with a marker overlay re-drawn on
 * every write; markers advance at the profile's `marker_hz` (<= 10 Hz). The first write
 * hides the cursor (a block cursor on marker cell 0 would recolour it) and parks it
 * below the reserved rows; the last write shows it again. Every write goes through a
 * blocking `fs.writeSync(1, ...)`, timed on both sides, so time a host makes the
 * producer wait is measured directly (`producer_stall_pct`). A generator that falls
 * behind writes the SAME bytes late — it never skips — and logs how far behind it is;
 * while behind it still yields to the event loop every 50 ms, so stdin, EOF and
 * signals are serviced. A generator blocked INSIDE one write (its host stopped reading
 * altogether) runs no JavaScript until that write returns: no stop file, EOF or signal
 * handler can end it, only killing its PID (recorded in its slot file).
 *
 * Records (`<run-dir>/gen-<slot>.jsonl`, one JSON object per line; absolute
 * process.hrtime nanosecond timestamps are DECIMAL STRINGS because they exceed 2^53):
 *   start     configuration, t0_ns, terminal size, console handling
 *   marker    {seq, t_sched_ns, t_emit_ns, t_written_ns, bytes, phase} on each tick
 *   echo      {seq, byte, t_read_ns, t_emit_ns, t_written_ns, bytes} (--echo);
 *             t_read_ns is the string "unknown" when the read time is not known
 *   progress  cumulative counters every 500 ms (window any interval by differencing),
 *             among them blocked_ns (time inside the pane write), io_ns (time in
 *             the generator's own file I/O: every log record, resize ones included,
 *             and every stop-file check — as it stood BEFORE this record's own write)
 *             and io_after_prev_ns (io_ns right AFTER the previous start/progress
 *             record's own write: a record cannot carry its own, the next one does)
 *   resize    the pane size the host gave the session, as it changes
 *   exit      reason and final counters (blocked_ns, io_ns among them)
 *
 * --echo puts stdin in raw mode and answers each received byte with an echo marker in
 * row 1. Flags beyond D13's: --calibration (a static calibration marker, no load),
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

/**
 * A timer for the generator's own file I/O: `wrap(fn)` returns `fn` timed on
 * `clock`, adding each call's duration to the one total `ns()` reports.
 *
 * @param {{nowNs: () => bigint}} clock
 */
export function createIoTimer(clock) {
  let total = 0n;
  return {
    wrap: (fn) => (...a) => {
      const t = clock.nowNs();
      try {
        return fn(...a);
      } finally {
        total += clock.nowNs() - t;
      }
    },
    ns: () => total,
  };
}

/**
 * The environment variable that shifts the generator's clock into the host's
 * timebase. A generator running inside WSL (the `tmux-wsl` setup, plan Phase 5)
 * reads the Linux VM's CLOCK_MONOTONIC, which is not the Windows QPC clock the
 * probe and the harness read; the adapter measures the offset and hands it over
 * here, through the tmux server's environment, so the generator's command line
 * stays the one every host runs (D13). Absent everywhere else.
 */
export const CLOCK_OFFSET_ENV = "TERMHOST_BENCH_CLOCK_OFFSET_NS";

/** The offset from `env` as a BigInt (0n when absent; a malformed value throws). */
export function clockOffsetFromEnv(env = process.env) {
  const v = env[CLOCK_OFFSET_ENV];
  if (v === undefined || v === "") return 0n;
  if (!/^-?\d{1,20}$/.test(v)) throw new UsageError(`${CLOCK_OFFSET_ENV} must be an integer nanosecond offset, got ${v}`);
  return BigInt(v);
}

/**
 * process.hrtime clock, shifted by `offsetNs` (0 unless the host's timebase is
 * another machine's, see CLOCK_OFFSET_ENV). `sleepUntil` uses setTimeout and
 * `yield` uses setImmediate, so stdin data, EOF and signal handlers run whenever
 * the driver waits or yields.
 */
export function createRealClock({ offsetNs = 0n } = {}) {
  const now = () => process.hrtime.bigint() + offsetNs;
  return {
    offsetNs,
    nowNs: now,
    sleepUntil(tNs) {
      const ms = Number(tNs - now()) / 1e6;
      return new Promise((r) => setTimeout(r, Math.max(0, Math.ceil(ms))));
    },
    yield: () => new Promise((r) => setImmediate(r)),
  };
}

/**
 * A virtual clock: time moves only when the driver sleeps or a writer advances it.
 * Scheduled events (stdin bytes, EOF, a signal) are delivered when the driver sleeps
 * past their time — at that time — or yields after it — at the yield, as a real
 * event loop would deliver them to a busy generator.
 */
export function createVirtualClock({ startNs = 0n } = {}) {
  let now = startNs;
  const events = [];
  let handler = null;
  const deliverThrough = (tNs, advance) => {
    while (events.length > 0 && events[0].tNs <= tNs) {
      const ev = events.shift();
      if (advance && ev.tNs > now) now = ev.tNs;
      handler?.(ev.event, now);
    }
  };
  return {
    nowNs: () => now,
    advance(ns) {
      now += BigInt(ns);
    },
    async sleepUntil(tNs) {
      deliverThrough(tNs, true);
      if (tNs > now) now = tNs;
    },
    async yield() {
      deliverThrough(now, false);
    },
    /**
     * @param {number | bigint} tNs
     * @param {{bytes?: string | Buffer, eof?: boolean, signal?: string}} event
     */
    scheduleEvent(tNs, event) {
      const ev = { ...event };
      if (ev.bytes !== undefined) ev.bytes = Buffer.from(ev.bytes);
      events.push({ tNs: BigInt(tNs), event: ev });
      events.sort((a, b) => (a.tNs < b.tNs ? -1 : a.tNs > b.tNs ? 1 : 0));
    },
    setEventHandler(fn) {
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

/** While writes are queued, the driver yields to the event loop at least this often. */
export const YIELD_INTERVAL_MS = 50;

/**
 * The first write: enter the alternate screen if the profile asks, HIDE THE CURSOR
 * (a block cursor resting on marker cell 0 would turn it into the cursor colour), clear,
 * and park the cursor on the row below the echo region, out of both reserved regions.
 *
 * @param {object} profile
 * @returns {string}
 */
export function preambleFor(profile) {
  return `${profile.alt_screen ? "\x1b[?1049h" : ""}\x1b[?25l\x1b[H\x1b[2J\x1b[${ECHO_REGION.row + 2};1H`;
}

/**
 * The last write: reset SGR, show the cursor again, leave the alternate screen.
 *
 * @param {object} profile
 * @returns {string}
 */
export function postambleFor(profile) {
  return `\x1b[0m\x1b[?25h${profile.alt_screen ? "\x1b[?1049l" : ""}\r\n`;
}

/**
 * One generator run. Built by `runGenerator`; the methods share the run's counters.
 */
class GeneratorRun {
  constructor(opts) {
    this.opts = opts;
    this.profile = opts.profile;
    this.geometry = opts.profile.geometry;
    this.session = opts.session;
    this.clock = opts.clock;
    this.write = opts.write;
    // The generator's own file I/O — its log records and its stop-file checks —
    // is timed apart from the pane write (`io_ns` on progress and exit records).
    // On a host whose run directory is a network or 9P mount (tmux-wsl reaches the
    // Windows run directory over WSL's 9P) a slow log write can hold the generator
    // past a progress interval; the harness then subtracts this time from the gap
    // instead of calling it producer stall.
    // One timer for the run: the caller passes its own (`opts.io`) when it logs
    // outside this object too (the resize watcher), so all of it is counted once.
    this.io = opts.io ?? createIoTimer(this.clock);
    // The I/O total right after the last start/progress record's own write.
    this.ioAfterPoint = 0n;
    this.log = this.io.wrap(opts.log);
    this.echo = opts.echo ?? false;
    this.shouldStop = this.io.wrap(opts.shouldStop ?? (() => null));
    this.control = opts.control ?? createControl();
    this.t0 = this.clock.nowNs();
    this.endNs =
      opts.durationMs == null ? null : this.t0 + BigInt(Math.round(opts.durationMs * 1e6));
    this.st = {
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
    this.queue = [];
    this.nextProgressNs = this.t0 + BigInt(PROGRESS_INTERVAL_MS) * MS;
    this.nextStopCheckNs = this.t0;
    this.lastYieldNs = this.t0;
  }

  async run() {
    this.logStart();
    this.control.onInput = (buf, tReadNs) => this.onInput(buf, tReadNs);
    this.writePreamble();
    if (this.opts.calibration) await this.calibrationLoop();
    else await this.loadLoop();
    return this.finish();
  }

  logStart() {
    const { profile, opts } = this;
    this.log({
      type: "start",
      schema: GEN_SCHEMA,
      run_id: opts.runId,
      session: this.session,
      mode: opts.calibration ? "calibration" : "load",
      profile: profile.id,
      profile_digest: opts.profileDigest ?? profileDigest(profile),
      profile_calibrated: profile.calibrated,
      seed: opts.seed,
      geometry: { ...this.geometry },
      marker_hz: profile.marker_hz,
      echo: this.echo,
      clock: opts.clock?.offsetNs ? `process.hrtime+${opts.clock.offsetNs}ns` : "process.hrtime",
      t0_ns: String(this.t0),
      ...(opts.startExtra ?? {}),
    });
    this.ioAfterPoint = this.io.ns();
  }

  /** One blocking write, timed on both sides; the time inside it is producer stall. */
  timedWrite(buf) {
    const before = this.clock.nowNs();
    this.write(buf);
    const after = this.clock.nowNs();
    this.st.blockedNs += after - before;
    return [before, after];
  }

  failWrite(e) {
    this.control.stop(e && CLOSED_STDOUT_CODES.has(e.code) ? "stdout_closed" : "error", e);
  }

  /** --echo: answer each received byte with an echo marker; 0x03 stops the run. */
  onInput(buf, tReadNs) {
    if (!this.echo) return;
    for (const byte of buf) {
      if (this.control.stopped) return;
      if (byte === 0x03) {
        this.control.stop("interrupt");
        return;
      }
      const seq = this.st.echoes;
      this.st.echoes += 1;
      this.st.echoOverlay = markerOverlay(
        { kind: MARKER_KINDS.echo, session: this.session, seq: markerSeq(seq) },
        ECHO_REGION,
      );
      const bytes = Buffer.from(this.st.echoOverlay, "latin1");
      try {
        const [before, after] = this.timedWrite(bytes);
        this.st.echoBytes += bytes.length;
        this.log({
          type: "echo",
          seq,
          byte,
          // An unknown read time is recorded as unknown, never as the emit time (that
          // would read as zero latency).
          t_read_ns: tReadNs == null ? "unknown" : String(tReadNs),
          t_emit_ns: String(before),
          t_written_ns: String(after),
          bytes: bytes.length,
        });
      } catch (e) {
        this.failWrite(e);
        return;
      }
    }
  }

  writePreamble() {
    const preamble = preambleFor(this.profile);
    try {
      this.timedWrite(Buffer.from(preamble, "latin1"));
      this.st.bytesScheduled += preamble.length;
      this.st.bytesWritten += preamble.length;
      this.st.writes += 1;
    } catch (e) {
      this.failWrite(e);
    }
  }

  progress(now) {
    const { st } = this;
    this.log({
      type: "progress",
      t_ns: String(now),
      sched_pos_us: st.schedPosUs,
      bytes_scheduled: st.bytesScheduled,
      bytes_written: st.bytesWritten,
      writes: st.writes,
      blocked_ns: Number(st.blockedNs),
      io_ns: Number(this.io.ns()),
      io_after_prev_ns: Number(this.ioAfterPoint),
      echo_bytes: st.echoBytes,
      queued: this.queue.length,
    });
    // A record cannot carry the I/O total after its own write; the next one does.
    this.ioAfterPoint = this.io.ns();
  }

  pollStop(now) {
    if (this.endNs !== null && now >= this.endNs) this.control.stop("duration");
    if (!this.control.stopped && now >= this.nextStopCheckNs) {
      const reason = this.shouldStop();
      if (reason) this.control.stop(reason);
      this.nextStopCheckNs = now + BigInt(STOP_CHECK_INTERVAL_MS) * MS;
    }
  }

  /**
   * Yield to the event loop when YIELD_INTERVAL_MS has passed without one, so stdin,
   * EOF and signals are serviced while writes are queued. A generator blocked INSIDE
   * one write runs no JavaScript at all until that write returns.
   */
  async yieldIfDue() {
    if (this.clock.nowNs() - this.lastYieldNs < BigInt(YIELD_INTERVAL_MS) * MS) return;
    await this.clock.yield();
    this.lastYieldNs = this.clock.nowNs();
  }

  dueAt(w) {
    return this.t0 + BigInt(w.tUs) * 1000n;
  }

  /** Pull a due write into the queue, counting its scheduled bytes. */
  materialize(w) {
    const overlay = markerOverlay(
      { kind: MARKER_KINDS.output, session: this.session, seq: markerSeq(w.seq) },
      MARKER_REGION,
    );
    let content = null;
    let contentBytes = 0;
    if (w.content) {
      const known = knownContentSize(w.content);
      if (known !== null) {
        contentBytes = known;
      } else {
        content = buildContent(w.content, this.geometry);
        contentBytes = Buffer.byteLength(content, "utf8");
        if (this.queue.length >= RETAIN_QUEUED_CONTENT) content = null;
      }
    }
    const detBytes = contentBytes + overlay.length;
    this.st.bytesScheduled += detBytes;
    return { w, content, overlay, detBytes };
  }

  pullDue(now) {
    while (this.dueAt(this.pending) <= now) {
      this.queue.push(this.materialize(this.pending));
      this.pending = this.schedule.next();
    }
  }

  /** Write one queued item: its content, its marker and any echo marker. */
  perform(item) {
    const { w } = item;
    const { st } = this;
    const content = w.content ? (item.content ?? buildContent(w.content, this.geometry)) : "";
    const text = composeWrite(
      content,
      item.overlay + st.echoOverlay,
      w.content ? isSyncContent(w.content) : false,
    );
    const bytes = Buffer.from(text, "utf8");
    const [before, after] = this.timedWrite(bytes);
    st.writes += 1;
    st.bytesWritten += item.detBytes;
    st.echoBytes += bytes.length - item.detBytes;
    st.schedPosUs = w.tUs;
    if (w.newSeq !== null) {
      st.markers += 1;
      this.log({
        type: "marker",
        kind: "output",
        seq: w.newSeq,
        phase: w.phase.id,
        t_sched_ns: String(this.dueAt(w)),
        t_emit_ns: String(before),
        t_written_ns: String(after),
        bytes: bytes.length,
      });
    }
  }

  async loadLoop() {
    const { control, clock } = this;
    this.schedule = new Schedule({
      profile: this.profile,
      seed: this.opts.seed,
      session: this.session,
    });
    this.pending = this.schedule.next();
    while (!control.stopped) {
      const now = clock.nowNs();
      // A run of duration D owns exactly the writes due before D.
      if (this.endNs !== null && now >= this.endNs) {
        control.stop("duration");
        break;
      }
      this.pullDue(now);
      if (now >= this.nextProgressNs) {
        this.progress(now);
        while (this.nextProgressNs <= now) this.nextProgressNs += BigInt(PROGRESS_INTERVAL_MS) * MS;
      }
      this.pollStop(now);
      if (control.stopped) break;
      if (this.queue.length > 0) {
        try {
          this.perform(this.queue.shift());
        } catch (e) {
          this.failWrite(e);
        }
        await this.yieldIfDue();
        continue;
      }
      let wake = this.dueAt(this.pending);
      if (this.nextProgressNs < wake) wake = this.nextProgressNs;
      if (this.nextStopCheckNs < wake) wake = this.nextStopCheckNs;
      if (this.endNs !== null && this.endNs < wake) wake = this.endNs;
      await clock.sleepUntil(wake);
      this.lastYieldNs = clock.nowNs();
    }
    // Count everything the schedule called for up to the stop, written or not
    // (for a duration stop, everything due strictly before the end).
    this.pullDue(control.reason === "duration" ? this.endNs - 1n : clock.nowNs());
  }

  async calibrationLoop() {
    const { control, clock, st } = this;
    const overlay = markerOverlay(
      { kind: MARKER_KINDS.calibration, session: this.session, seq: 0 },
      MARKER_REGION,
    );
    const bytes = Buffer.from(overlay, "latin1");
    let nextDraw = this.t0;
    let first = true;
    while (!control.stopped) {
      const now = clock.nowNs();
      this.pollStop(now);
      if (control.stopped) break;
      if (now >= nextDraw) {
        try {
          const [before, after] = this.timedWrite(bytes);
          st.writes += 1;
          st.bytesScheduled += bytes.length;
          st.bytesWritten += bytes.length;
          if (first) {
            st.markers += 1;
            this.log({
              type: "marker",
              kind: "calibration",
              seq: 0,
              phase: "calibration",
              t_sched_ns: String(this.t0),
              t_emit_ns: String(before),
              t_written_ns: String(after),
              bytes: bytes.length,
            });
            first = false;
          }
        } catch (e) {
          this.failWrite(e);
          break;
        }
        nextDraw += BigInt(CALIBRATION_REDRAW_MS) * MS;
      }
      let wake = nextDraw;
      const check = clock.nowNs() + BigInt(STOP_CHECK_INTERVAL_MS) * MS;
      if (check < wake) wake = check;
      if (this.endNs !== null && this.endNs < wake) wake = this.endNs;
      await clock.sleepUntil(wake);
    }
  }

  finish() {
    const { st, control, clock } = this;
    const now = clock.nowNs();
    const elapsedUs = Number((now - this.t0) / 1000n);
    try {
      this.write(Buffer.from(postambleFor(this.profile), "latin1"));
    } catch {
      // the reader is gone; nothing left to restore
    }
    control.onInput = null;
    const exit = {
      type: "exit",
      reason: control.reason ?? "error",
      t_ns: String(now),
      wall_ns: Number(now - this.t0),
      bytes_scheduled: st.bytesScheduled,
      bytes_written: st.bytesWritten,
      writes: st.writes,
      blocked_ns: Number(st.blockedNs),
      io_ns: Number(this.io.ns()),
      io_after_prev_ns: Number(this.ioAfterPoint),
      markers: st.markers,
      echoes: st.echoes,
      echo_bytes: st.echoBytes,
      sched_pos_us: st.schedPosUs,
      queued: this.queue.length,
      lag_us: this.queue.length > 0 ? Math.max(0, elapsedUs - this.queue[0].w.tUs) : 0,
    };
    if (control.error) exit.error = String(control.error.message ?? control.error);
    this.log(exit);
    return exit;
  }
}

/**
 * Run one session's load until a stop condition.
 *
 * @param {{
 *   profile: object, profileDigest?: string, seed: number, session: number, runId: string,
 *   clock: {nowNs: () => bigint, sleepUntil: (tNs: bigint) => Promise<void>, yield: () => Promise<void>},
 *   write: (buf: Buffer) => void, log: (record: object) => void,
 *   echo?: boolean, calibration?: boolean, durationMs?: number|null,
 *   shouldStop?: () => string|null, control?: ReturnType<typeof createControl>,
 *   io?: ReturnType<typeof createIoTimer> (a timer the caller shares, e.g. with the resize watcher),
 *   startExtra?: object,
 * }} opts
 * @returns {Promise<object>} the exit record
 */
export async function runGenerator(opts) {
  return new GeneratorRun(opts).run();
}

/**
 * The event handling main() wires to stdin and signals, applied to a virtual event.
 *
 * @param {ReturnType<typeof createControl>} control
 * @param {{bytes?: Buffer, eof?: boolean, signal?: string}} event
 * @param {bigint} tNs
 */
function applyEvent(control, event, tNs) {
  if (event.bytes) control.onInput?.(event.bytes, tNs);
  else if (event.eof) control.stop("stdin_eof");
  else if (event.signal) control.stop(`signal:${event.signal}`);
}

/**
 * Run the generator against a virtual clock, collecting every write — the
 * deterministic harness for tests and offline analysis.
 *
 * @param {{profile: object, seed?: number, session?: number, durationMs: number,
 *   writeDelayNs?: number | bigint | ((buf: Buffer) => number | bigint),
 *   logDelayNs?: number | bigint, echo?: boolean, calibration?: boolean,
 *   inputs?: ({tNs: number|bigint, bytes: string|Buffer} | {tNs: number|bigint, eof: true}
 *     | {tNs: number|bigint, signal: string})[]}} opts
 * @returns {Promise<{writes: {tNs: bigint, bytes: Buffer}[], records: object[], exit: object,
 *   bytes: Buffer, postamble: Buffer}>}
 */
export async function runVirtual({
  profile,
  seed = 1,
  session = 0,
  durationMs,
  writeDelayNs = 0,
  logDelayNs = 0,
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
  for (const { tNs, ...event } of inputs) clock.scheduleEvent(tNs, event);
  clock.setEventHandler((event, t) => applyEvent(control, event, t));
  const exit = await runGenerator({
    profile,
    seed,
    session,
    runId: "virtual",
    clock,
    write,
    // `logDelayNs`: each log record takes this long to write (a slow run directory).
    log: (r) => {
      records.push(r);
      if (logDelayNs) clock.advance(logDelayNs);
    },
    echo,
    calibration,
    durationMs,
    control,
  });
  // The postamble is the last write; it is outside the measured stream.
  const stream = writes.slice(0, -1);
  return {
    writes: stream,
    records,
    exit,
    bytes: Buffer.concat(stream.map((w) => w.bytes)),
    postamble: writes[writes.length - 1].bytes,
  };
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
 * the console code page (the system's OEM code page unless something changed it) and
 * every multi-byte glyph would become several cells. The previous code page is restored at exit (main() restores it in a
 * `finally`). When the original code page cannot be read, nothing is changed: a page
 * that cannot be restored is never switched.
 *
 * @param {{platform?: string, stdoutIsTty?: boolean, run?: typeof spawnSync, env?: object}} [deps]
 * @returns {{applied: boolean, reason?: string, cp_before?: number|null, cp_after?: number|null, chcp?: string}}
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
  if (before === null) {
    return { applied: false, reason: "the console code page could not be read", cp_before: null };
  }
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

/** A `resize` record: the pane size the session now has. */
export function resizeRecord(size, tNs) {
  return { type: "resize", t_ns: String(tNs), cols: size.cols, rows: size.rows };
}

/**
 * Log a `resize` record whenever `stream` (the session's stdout) reports a new
 * size. Returns the unsubscribe.
 */
export function watchResize(stream, log, clock, sizeOf) {
  const onResize = () => {
    const size = sizeOf();
    if (size) log(resizeRecord(size, clock.nowNs()));
  };
  stream.on?.("resize", onResize);
  return () => stream.off?.("resize", onResize);
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
  let clock;
  try {
    clock = createRealClock({ offsetNs: clockOffsetFromEnv() });
  } catch (e) {
    process.stderr.write(`ai-session: ${e.message}\n`);
    return 2;
  }
  const { slot, path: slotPath } = claimSlot(runDir, {
    run_id: opts.runId,
    pid: process.pid,
    ppid: process.ppid,
    // Whose PIDs these are: a generator inside WSL (tmux-wsl) writes Linux PIDs
    // into a directory the Windows probe also reads, which must not take them
    // for Windows processes.
    platform: process.platform,
    host_terminal_id: hostTerminalIds(),
    claimed_at_ns: String(clock.nowNs()),
    argv: process.argv.slice(1),
  });
  const log = createJsonlLog(join(runDir, `gen-${slot}.jsonl`));
  // The pane size the host gives this session, as it changes (a host lays a
  // session out after starting it): `resize` records beside the start record's
  // `terminal_size`, so a harness can tell whether frames wrapped. Not part of
  // the deterministic output.
  // The run's I/O timer, shared with the resize watcher: a resize record is file
  // I/O like any other and lands in the same io_ns.
  const io = createIoTimer(clock);
  const unwatchResize = watchResize(process.stdout, io.wrap(log), clock, terminalSize);
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
  // The console is changed only once the slot and the log exist, and always restored.
  let consoleState = { applied: false };
  try {
    consoleState = prepareConsole();
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
      io,
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
    if (exit.reason === "error") {
      process.stderr.write(`ai-session: ${exit.error ?? "failed"}\n`);
      return 1;
    }
    return 0;
  } finally {
    if (raw) stdin.setRawMode(false);
    stdin.pause();
    restoreConsole(consoleState);
    unwatchResize();
    log.close();
  }
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
