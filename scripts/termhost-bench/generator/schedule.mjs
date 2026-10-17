/**
 * The deterministic write schedule of one AI session: a profile's phase chain, the
 * content frames each phase emits, and the fixed-cadence marker ticks, merged into
 * one ordered list of writes on a virtual microsecond timeline.
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 1. The
 * schedule is a pure function of (profile, seed, session): it never reads a clock,
 * so the byte stream is identical however fast or slowly the host drains it — a
 * blocked generator writes the same bytes later; it never skips.
 *
 * Timeline rules:
 *   - phase k starts where phase k-1 ended; its duration is sampled from its
 *     `duration_ms` table (>= 1 ms) and the next phase from its transition weights;
 *   - a phase with a frame rate emits frame j at start + floor(j * 1e6 / fps) us
 *     while that is before the phase's end;
 *   - marker tick k is at floor(k * 1e6 / marker_hz) us and starts sequence k;
 *   - a frame and a tick at the same microsecond are ONE write (the frame carrying
 *     the new marker); a tick with no frame is a marker-only write; every other
 *     frame re-draws the current marker.
 *
 * Three independent PRNG streams (phases, sizes, text) are seeded from
 * mix(seed, session), so sessions launched with one command line (D13) are
 * decorrelated but reproducible.
 */

import { buildRepaintFrame, buildSpinnerFrame } from "../../tui-repaint-generator.mjs";
import { sampleQuantiles } from "../profile.mjs";
import {
  buildBurstChunk,
  buildStreamChunk,
  mixSeed,
  mulberry32,
  repaintRowsFor,
} from "./content.mjs";

/**
 * Pick a key from `{key: weight}` with u in [0, 1), walking the keys in SORTED order:
 * the stream then depends only on a profile's content (its canonical digest), never
 * on the key order of its JSON.
 *
 * @param {Record<string, number>} weights
 * @param {number} u
 * @returns {string}
 */
export function pickWeighted(weights, u) {
  const entries = Object.entries(weights).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  let total = 0;
  for (const [, w] of entries) total += w;
  const target = u * total;
  let acc = 0;
  for (const [key, w] of entries) {
    acc += w;
    if (target < acc) return key;
  }
  return entries[entries.length - 1][0];
}

/** The per-session base seed. */
export function sessionSeed(seed, session) {
  if (!Number.isInteger(seed) || seed < 0 || seed > 0xffffffff)
    throw new RangeError(`seed must be a 32-bit unsigned integer: ${seed}`);
  if (!Number.isInteger(session) || session < 0)
    throw new RangeError(`bad session index ${session}`);
  return mixSeed(mixSeed(seed, 0x7465726d), session);
}

/**
 * The phase chain alone: cheap to run for thousands of phases (profile conformance).
 */
export class PhaseSequencer {
  /**
   * @param {object} profile a validated profile
   * @param {{next: () => number}} rng
   */
  constructor(profile, rng) {
    this.profile = profile;
    this.rng = rng;
    this.nextId = profile.initial_phase;
    this.endUs = 0;
    this.ordinal = 0;
  }

  /** @returns {{id: string, kind: string, def: object, startUs: number, endUs: number, ordinal: number}} */
  next() {
    const id = this.nextId;
    const def = this.profile.phases[id];
    const durationUs = Math.max(
      1000,
      Math.round(sampleQuantiles(def.duration_ms, this.rng.next()) * 1000),
    );
    const phase = {
      id,
      kind: def.kind,
      def,
      startUs: this.endUs,
      endUs: this.endUs + durationUs,
      ordinal: this.ordinal,
    };
    this.ordinal += 1;
    this.endUs = phase.endUs;
    this.nextId = pickWeighted(this.profile.transitions[id], this.rng.next());
    return phase;
  }
}

/**
 * @typedef {{type: "repaint", rows: number, frame: number}
 *   | {type: "spinner", frame: number}
 *   | {type: "stream" | "burst", size: number, seed: number}} Content
 * @typedef {{tUs: number, phase: {id: string, kind: string, ordinal: number, startUs: number, endUs: number},
 *   content: Content | null, newSeq: number | null, seq: number}} ScheduledWrite
 */

export class Schedule {
  /**
   * @param {{profile: object, seed: number, session: number}} opts
   */
  constructor({ profile, seed, session }) {
    this.profile = profile;
    this.geometry = profile.geometry;
    this.markerHz = profile.marker_hz;
    const base = sessionSeed(seed, session);
    this.sequencer = new PhaseSequencer(profile, mulberry32(mixSeed(base, 1)));
    this.rngSize = mulberry32(mixSeed(base, 2));
    this.rngText = mulberry32(mixSeed(base, 3));
    this.repaintFrame = mixSeed(base, 4) & 0xffff;
    this.spinnerFrame = 0;
    this.tickK = 0;
    this.seq = -1;
    /** Phases still needed, oldest first. */
    this.phases = [this.sequencer.next()];
    /** Frame cursor: index into `phases` and frame number within that phase. */
    this.cursor = { index: 0, j: 0 };
  }

  tickUs(k) {
    return Math.floor((k * 1e6) / this.markerHz);
  }

  /** @returns {ScheduledWrite} */
  next() {
    const tickUs = this.tickUs(this.tickK);
    const frame = this.peekFrame(tickUs);
    let write;
    if (frame && frame.tUs < tickUs) write = this.takeFrame(frame, false);
    else if (frame) write = this.takeFrame(frame, true);
    else write = this.takeTick(tickUs);
    this.trim();
    return write;
  }

  /**
   * The next frame at or before `limitUs`, or null. Commits the cursor past phases
   * whose frames are exhausted (they can never emit again).
   */
  peekFrame(limitUs) {
    for (;;) {
      while (this.cursor.index >= this.phases.length) this.phases.push(this.sequencer.next());
      const phase = this.phases[this.cursor.index];
      if (phase.startUs > limitUs) return null;
      if (phase.def.fps) {
        const tUs = phase.startUs + Math.floor((this.cursor.j * 1e6) / phase.def.fps);
        if (tUs < phase.endUs) return tUs <= limitUs ? { index: this.cursor.index, tUs } : null;
      }
      this.cursor = { index: this.cursor.index + 1, j: 0 };
    }
  }

  takeFrame(frame, withTick) {
    const phase = this.phases[frame.index];
    this.cursor = { index: frame.index, j: this.cursor.j + 1 };
    const content = this.contentFor(phase);
    let newSeq = null;
    if (withTick) {
      this.seq += 1;
      this.tickK += 1;
      newSeq = this.seq;
    }
    return { tUs: frame.tUs, phase: describe(phase), content, newSeq, seq: this.seq };
  }

  takeTick(tickUs) {
    this.seq += 1;
    this.tickK += 1;
    return {
      tUs: tickUs,
      phase: describe(this.phaseAt(tickUs)),
      content: null,
      newSeq: this.seq,
      seq: this.seq,
    };
  }

  phaseAt(tUs) {
    for (;;) {
      for (const phase of this.phases) if (phase.startUs <= tUs && tUs < phase.endUs) return phase;
      this.phases.push(this.sequencer.next());
    }
  }

  /** Drop phases no future frame or tick can land in. */
  trim() {
    const nextTick = this.tickUs(this.tickK);
    let drop = 0;
    while (drop < this.cursor.index && this.phases[drop].endUs <= nextTick) drop += 1;
    if (drop > 0) {
      this.phases.splice(0, drop);
      this.cursor = { index: this.cursor.index - drop, j: this.cursor.j };
    }
  }

  /** @returns {Content} */
  contentFor(phase) {
    const def = phase.def;
    switch (def.kind) {
      case "repaint": {
        const rows =
          def.rows ??
          repaintRowsFor(sampleQuantiles(def.frame_bytes, this.rngSize.next()), this.geometry);
        const content = { type: "repaint", rows, frame: this.repaintFrame };
        this.repaintFrame += 1;
        return content;
      }
      case "spinner": {
        const content = { type: "spinner", frame: this.spinnerFrame };
        this.spinnerFrame += 1;
        return content;
      }
      case "stream":
      case "burst": {
        const size = Math.max(1, Math.round(sampleQuantiles(def.chunk_bytes, this.rngSize.next())));
        return { type: def.kind, size, seed: this.rngText.nextU32() };
      }
      default:
        throw new Error(`phase kind ${def.kind} emits no frames`);
    }
  }
}

function describe(phase) {
  return {
    id: phase.id,
    kind: phase.kind,
    ordinal: phase.ordinal,
    startUs: phase.startUs,
    endUs: phase.endUs,
  };
}

/**
 * Build a content descriptor's bytes (deterministic; safe to call twice).
 *
 * @param {Content} content
 * @param {{cols: number, rows: number}} geometry
 * @returns {string}
 */
export function buildContent(content, geometry) {
  switch (content.type) {
    case "repaint":
      return buildRepaintFrame({ cols: geometry.cols, rows: content.rows, frame: content.frame });
    case "spinner":
      return buildSpinnerFrame({ cols: geometry.cols, rows: geometry.rows, frame: content.frame });
    case "stream":
      return buildStreamChunk(content.size, content.seed, geometry.cols);
    case "burst":
      return buildBurstChunk(content.size, content.seed);
    default:
      throw new Error(`unknown content type ${content.type}`);
  }
}

/** True for content that is one DEC 2026 synchronized block (ends with SYNC_END). */
export function isSyncContent(content) {
  return content.type === "repaint" || content.type === "spinner";
}

/** A content's byte size when known without building it (stream/burst), else null. */
export function knownContentSize(content) {
  return content.type === "stream" || content.type === "burst" ? content.size : null;
}
