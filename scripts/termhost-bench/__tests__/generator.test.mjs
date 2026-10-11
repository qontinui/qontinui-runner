/**
 * The AI-session generator: determinism (one seed -> one SHA-256 over 10 s of
 * output, on a virtual clock), profile conformance (frame rates, sizes, durations,
 * transitions, the 10 Hz marker cadence, and the marker region surviving a repaint),
 * stall accounting through an injected slow writer, echo and calibration modes, and
 * the D13 command line run as real processes (slots, stop file, stdin EOF).
 *
 *   node --test scripts/termhost-bench/__tests__/generator.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { SYNC_BEGIN, SYNC_END } from "../../tui-repaint-generator.mjs";
import {
  ECHO_REGION,
  MARKER_REGION,
  decodeMarkerIndices,
  markerOverlay,
  parseOverlays,
  stripOverlays,
} from "../marker.mjs";
import { SHIPPED_PROFILES, loadProfile, validateProfile } from "../profile.mjs";
import { mulberry32 } from "../generator/content.mjs";
import { PhaseSequencer, Schedule, buildContent, pickWeighted } from "../generator/schedule.mjs";
import {
  HOST_TERMINAL_ENV,
  UsageError,
  claimSlot,
  composeWrite,
  createFdWriter,
  hostTerminalIds,
  parseArgs,
  parseCodePage,
  prepareConsole,
  restoreConsole,
  runVirtual,
} from "../generator/ai-session.mjs";
import { MiniVt } from "./helpers/mini-vt.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const GENERATOR = join(HERE, "..", "generator", "ai-session.mjs");
const FIXTURE_PROFILE = join(HERE, "fixtures", "determinism-profile.json");
const QUIET_PROFILE = join(HERE, "fixtures", "quiet-profile.json");
const readJson = (p) => JSON.parse(readFileSync(p, "utf8"));
const sha256 = (buf) => createHash("sha256").update(buf).digest("hex");
const shipped = (id) => loadProfile(id).profile;

/** SHA-256 of the determinism fixture's first 10 s at seed 1, session 0. */
const PINNED_SHA256 = "fa107f1eb890abfe8ee17485f138015f58a16bfa5bd6c2bf3306aa5d40358c1b";

// ---------------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------------

test("determinism: one seed -> one SHA-256 over 10 s of output", async () => {
  const profile = readJson(FIXTURE_PROFILE);
  const a = await runVirtual({ profile, seed: 1, session: 0, durationMs: 10000 });
  const b = await runVirtual({ profile, seed: 1, session: 0, durationMs: 10000 });
  assert.equal(sha256(a.bytes), sha256(b.bytes));
  assert.equal(
    sha256(a.bytes),
    PINNED_SHA256,
    "the stream changed: if intended, re-pin PINNED_SHA256",
  );
  const kinds = new Set(a.records.filter((r) => r.type === "marker").map((r) => r.phase));
  assert.deepEqual(
    [...kinds].sort(),
    ["idle", "repaint", "stream", "thinking", "tool_burst"],
    "10 s visits every phase kind",
  );
});

test("determinism: another seed or another session gives another stream", async () => {
  const profile = readJson(FIXTURE_PROFILE);
  // Compare CONTENT, overlays stripped: the marker alone already differs by session,
  // and sessions launched with one command line must be decorrelated in what they draw.
  const content = async (seed, session) =>
    sha256(stripOverlays((await runVirtual({ profile, seed, session, durationMs: 3000 })).bytes));
  const base = await content(1, 0);
  assert.notEqual(await content(2, 0), base, "another seed draws other content");
  assert.notEqual(await content(1, 1), base, "another session draws other content");
});

test("determinism: every shipped profile repeats byte-for-byte", async () => {
  for (const id of SHIPPED_PROFILES) {
    const profile = shipped(id);
    const ms = id === "bulk" ? 1000 : 4000;
    const a = await runVirtual({ profile, seed: 9, session: 3, durationMs: ms });
    const b = await runVirtual({ profile, seed: 9, session: 3, durationMs: ms });
    assert.ok(a.bytes.length > 0, id);
    assert.ok(a.bytes.equals(b.bytes), id);
  }
});

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

test("the shipped profiles are valid; claude-tui is uncalibrated at runner#1856's 120x40 @ 30 fps", () => {
  for (const id of SHIPPED_PROFILES) assert.deepEqual(validateProfile(shipped(id)).errors, [], id);
  const tui = shipped("claude-tui");
  assert.equal(tui.calibrated, false);
  assert.equal(tui.provenance.kind, "assumption");
  assert.deepEqual(tui.geometry, { cols: 120, rows: 40 });
  assert.equal(tui.phases.repaint.fps, 30);
  assert.equal(tui.phases.repaint.rows, 40);
  assert.equal(tui.phases.thinking.fps, 30);
  assert.deepEqual(Object.keys(tui.phases).sort(), [
    "idle",
    "repaint",
    "stream",
    "thinking",
    "tool_burst",
  ]);
  const allRepaint = shipped("all-repaint");
  assert.deepEqual(
    Object.values(allRepaint.phases).map((p) => [p.kind, p.fps, p.rows]),
    [["repaint", 30, 40]],
  );
  for (const id of SHIPPED_PROFILES) assert.ok(shipped(id).marker_hz <= 10);
});

test("invalid profiles are rejected with the rule they break", () => {
  const base = shipped("claude-tui");
  const cases = [
    [(p) => (p.marker_hz = 11), /marker_hz/],
    [(p) => (p.transitions.stream.nowhere = 1), /"nowhere" is not a phase/],
    [(p) => delete p.transitions.tool_burst, /"tool_burst" has no outgoing transitions/],
    [(p) => (p.initial_phase = "missing"), /initial_phase/],
    [
      (p) =>
        (p.phases.repaint.frame_bytes = [
          [0, 1],
          [1, 2],
        ]),
      /oneOf/,
    ],
    [(p) => (p.phases.idle.fps = 5), /not/],
    [
      (p) =>
        (p.phases.stream.chunk_bytes = [
          [0.1, 1],
          [1, 2],
        ]),
      /starts at p = 0/,
    ],
    [
      (p) =>
        (p.phases.stream.chunk_bytes = [
          [0, 9],
          [1, 2],
        ]),
      /must not decrease/,
    ],
    [(p) => (p.calibrated = true), /calibrated must be true exactly when/],
    [(p) => (p.phases.repaint.rows = 41), /exceeds the geometry/],
    [(p) => (p.geometry.cols = 19), /geometry\/cols/],
  ];
  for (const [mutate, pattern] of cases) {
    const p = structuredClone(base);
    mutate(p);
    const r = validateProfile(p);
    assert.equal(r.valid, false, String(pattern));
    assert.ok(
      r.errors.some((e) => pattern.test(`${e.path} ${e.message} ${e.keyword}`)),
      `${pattern}: ${JSON.stringify(r.errors)}`,
    );
  }
});

// ---------------------------------------------------------------------------
// Conformance
// ---------------------------------------------------------------------------

function scheduleWrites(profile, seconds, seed = 1) {
  const s = new Schedule({ profile, seed, session: 0 });
  const out = [];
  for (;;) {
    const w = s.next();
    if (w.tUs >= seconds * 1e6) return out;
    out.push(w);
  }
}

function empiricalQuantile(values, p) {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.max(0, Math.ceil(p * sorted.length) - 1))];
}

/**
 * The p-quantile of a profile table under its documented semantics (linear
 * interpolation between [p, value] points), written here independently of
 * profile.mjs: an expectation computed by the sampler under test would agree with
 * any bug in it.
 */
function tableQuantile(table, p) {
  for (let i = 1; i < table.length; i += 1) {
    const [p0, v0] = table[i - 1];
    const [p1, v1] = table[i];
    if (p <= p1) return p1 === p0 ? v1 : v0 + ((p - p0) * (v1 - v0)) / (p1 - p0);
  }
  return table[table.length - 1][1];
}

/**
 * Samples follow a quantile table: at each p, the share of samples at or below the
 * table's p-quantile is p within 4 standard errors (sqrt(p(1-p)/n)) plus `slack`
 * for rounding. Compared in probability space, because where a quantile function is
 * steep a value-space tolerance is narrower than one standard error.
 */
function assertFollowsQuantiles(
  samples,
  table,
  label,
  { ps = [0.1, 0.25, 0.5, 0.75, 0.9], slack = 0.005 } = {},
) {
  const n = samples.length;
  assert.ok(n >= 1000, `${label}: ${n} samples is too few to test a distribution`);
  for (const p of ps) {
    const q = tableQuantile(table, p);
    const share = samples.filter((x) => x <= q).length / n;
    const bound = 4 * Math.sqrt((p * (1 - p)) / n) + slack;
    assert.ok(
      Math.abs(share - p) <= bound,
      `${label}: ${(share * 100).toFixed(2)}% of samples <= the p${p * 100} value ${q}, want ${p * 100}% +/- ${(bound * 100).toFixed(2)}`,
    );
  }
  const lo = table[0][1];
  const hi = table[table.length - 1][1];
  for (const x of samples)
    assert.ok(x >= lo - 0.5 && x <= hi + 0.5, `${label}: ${x} outside [${lo}, ${hi}]`);
}

test("conformance: markers advance exactly at the profile's cadence, and every write carries one", () => {
  for (const id of SHIPPED_PROFILES) {
    const profile = shipped(id);
    const writes = scheduleWrites(profile, 60);
    const ticks = writes.filter((w) => w.newSeq !== null);
    assert.equal(ticks.length, 60 * profile.marker_hz, id);
    ticks.forEach((w, k) => {
      assert.equal(w.newSeq, k);
      assert.equal(w.tUs, Math.floor((k * 1e6) / profile.marker_hz), `${id} tick ${k}`);
    });
    let prev = -1;
    for (const w of writes) {
      assert.ok(w.seq >= 0 && w.seq >= prev, `${id}: seq never decreases`);
      prev = w.seq;
    }
  }
});

test("conformance: each phase emits frames at its profile frame rate (exactly ceil(D * fps) frames)", () => {
  for (const id of SHIPPED_PROFILES) {
    const profile = shipped(id);
    const seconds = id === "claude-tui" ? 900 : 120;
    const writes = scheduleWrites(profile, seconds);
    const byPhase = new Map();
    for (const w of writes) {
      if (!w.content) continue;
      const entry = byPhase.get(w.phase.ordinal) ?? { ...w.phase, frames: 0 };
      entry.frames += 1;
      byPhase.set(w.phase.ordinal, entry);
    }
    const seen = new Set();
    for (const p of byPhase.values()) {
      if (p.endUs > seconds * 1e6) continue;
      const def = profile.phases[p.id];
      // Frame j sits at start + floor(j * 1e6 / fps) while that is before the end.
      const expected = Math.ceil(((p.endUs - p.startUs) * def.fps) / 1e6 - 1e-9);
      assert.ok(
        Math.abs(p.frames - expected) <= 1,
        `${id}/${p.id} phase ${p.ordinal}: ${p.frames} frames in ${(p.endUs - p.startUs) / 1000} ms at ${def.fps} fps`,
      );
      seen.add(p.id);
    }
    for (const [phaseId, def] of Object.entries(profile.phases)) {
      if (def.fps) assert.ok(seen.has(phaseId), `${id}/${phaseId} never ran to completion`);
      if (!def.fps)
        assert.ok(
          writes.every((w) => w.phase.id !== phaseId || !w.content),
          `${id}/${phaseId} is idle: markers only`,
        );
    }
  }
});

test("conformance: frame and chunk sizes follow the profile", () => {
  for (const id of SHIPPED_PROFILES) {
    const profile = shipped(id);
    const writes = scheduleWrites(profile, id === "claude-tui" ? 900 : 30).filter((w) => w.content);
    for (const [phaseId, def] of Object.entries(profile.phases)) {
      const contents = writes.filter((w) => w.phase.id === phaseId).map((w) => w.content);
      if (def.kind === "repaint") {
        assert.ok(contents.length > 0);
        for (const c of contents) assert.equal(c.rows, def.rows, `${id}/${phaseId} rows`);
        const frame = buildContent(contents[0], profile.geometry);
        const painted = frame.match(/\x1b\[(\d+);1H/g).length;
        assert.equal(painted, def.rows, "a repaint frame paints every row it was asked to");
      }
      if (def.chunk_bytes) {
        const sizes = contents.map((c) => c.size);
        const flat = def.chunk_bytes[0][1] === def.chunk_bytes[def.chunk_bytes.length - 1][1];
        if (flat)
          assert.ok(
            sizes.every((s) => s === def.chunk_bytes[0][1]),
            `${id}/${phaseId}: fixed-size chunks`,
          );
        else
          assertFollowsQuantiles(sizes, def.chunk_bytes, `${id}/${phaseId} chunk_bytes`, {
            slack: 0.01,
          });
        for (const c of contents.slice(0, 40)) {
          assert.equal(
            Buffer.byteLength(buildContent(c, profile.geometry), "utf8"),
            c.size,
            "a chunk is exactly its size",
          );
        }
      }
    }
  }
});

test("conformance: frame_bytes repaints are fitted to the target sizes", () => {
  const profile = readJson(FIXTURE_PROFILE);
  const contents = scheduleWrites(profile, 600)
    .filter((w) => w.content?.type === "repaint")
    .map((w) => w.content);
  const sizes = contents
    .slice(0, 300)
    .map((c) => Buffer.byteLength(buildContent(c, profile.geometry), "utf8"));
  const table = profile.phases.repaint.frame_bytes;
  const want = tableQuantile(table, 0.5);
  const got = empiricalQuantile(sizes, 0.5);
  assert.ok(Math.abs(got - want) / want < 0.15, `median repaint ${got} B vs target ${want} B`);
  assert.ok(Math.min(...contents.map((c) => c.rows)) >= 1);
  assert.ok(Math.max(...contents.map((c) => c.rows)) <= profile.geometry.rows);
});

test("conformance: the phase chain follows the duration tables and transition weights", () => {
  for (const id of ["claude-tui"]) {
    const profile = shipped(id);
    const chain = new PhaseSequencer(profile, mulberry32(12345));
    const durations = {};
    const transitions = {};
    let prev = null;
    for (let i = 0; i < 100000; i += 1) {
      const p = chain.next();
      (durations[p.id] ??= []).push((p.endUs - p.startUs) / 1000);
      if (prev) {
        const row = (transitions[prev] ??= {});
        row[p.id] = (row[p.id] ?? 0) + 1;
      }
      prev = p.id;
    }
    for (const [phaseId, def] of Object.entries(profile.phases)) {
      assertFollowsQuantiles(durations[phaseId], def.duration_ms, `${id}/${phaseId} duration_ms`);
    }
    for (const [from, weights] of Object.entries(profile.transitions)) {
      const total = Object.values(weights).reduce((a, b) => a + b, 0);
      const seen = Object.values(transitions[from]).reduce((a, b) => a + b, 0);
      assert.deepEqual(
        Object.keys(transitions[from]).sort(),
        Object.keys(weights).sort(),
        `${from}: only declared transitions occur`,
      );
      for (const [to, w] of Object.entries(weights)) {
        const want = w / total;
        const got = transitions[from][to] / seen;
        const bound = 4 * Math.sqrt((want * (1 - want)) / seen);
        assert.ok(
          Math.abs(got - want) <= bound,
          `${from}->${to}: ${got.toFixed(4)} vs ${want.toFixed(4)} +/- ${bound.toFixed(4)}`,
        );
      }
    }
  }
});

test("pickWeighted honours weights and insertion order", () => {
  assert.equal(pickWeighted({ a: 1, b: 3 }, 0), "a");
  assert.equal(pickWeighted({ a: 1, b: 3 }, 0.2499), "a");
  assert.equal(pickWeighted({ a: 1, b: 3 }, 0.25), "b");
  assert.equal(pickWeighted({ a: 1, b: 3 }, 0.9999), "b");
});

test("conformance: the bulk profile schedules 4 MiB/s of content per session", async () => {
  const r = await runVirtual({ profile: shipped("bulk"), durationMs: 2000 });
  const content = Buffer.byteLength(stripOverlays(r.bytes), "utf8");
  const perSecond = content / 2;
  assert.ok(Math.abs(perSecond - 4 * 1024 * 1024) / (4 * 1024 * 1024) < 0.01, `${perSecond} B/s`);
});

// ---------------------------------------------------------------------------
// The marker region survives every frame
// ---------------------------------------------------------------------------

const regionDecode = (vt, region = MARKER_REGION) =>
  decodeMarkerIndices(vt.regionBackgrounds(region));

test("the marker region survives a repaint frame (and would not without the overlay)", () => {
  const vt = new MiniVt(120, 40);
  const marker = { kind: 0, session: 4, seq: 17 };
  const frame = buildContent({ type: "repaint", rows: 40, frame: 3 }, { cols: 120, rows: 40 });
  vt.write(composeWrite(frame, markerOverlay(marker), true));
  assert.deepEqual(pick(regionDecode(vt)), { ok: true, session: 4, seq: 17 });
  vt.write(frame);
  assert.equal(
    regionDecode(vt).ok,
    false,
    "control: a bare repaint overwrites row 0, which is why every frame re-draws the marker",
  );
  vt.write(
    composeWrite(
      buildContent({ type: "spinner", frame: 5 }, { cols: 120, rows: 40 }),
      markerOverlay({ ...marker, seq: 18 }),
      true,
    ),
  );
  assert.deepEqual(pick(regionDecode(vt)), { ok: true, session: 4, seq: 18 });
});

test("the marker region survives scrolling text, and the echo region sits beside it", () => {
  const vt = new MiniVt(80, 24);
  const chunk = buildContent({ type: "burst", size: 12000, seed: 7 }, { cols: 80, rows: 24 });
  vt.write(chunk);
  assert.ok(vt.scrolls > 0, "the chunk scrolled the screen");
  vt.write(
    markerOverlay({ kind: 0, session: 2, seq: 5 }) +
      markerOverlay({ kind: 1, session: 2, seq: 9 }, ECHO_REGION),
  );
  vt.write(
    composeWrite(
      chunk,
      markerOverlay({ kind: 0, session: 2, seq: 6 }) +
        markerOverlay({ kind: 1, session: 2, seq: 9 }, ECHO_REGION),
      false,
    ),
  );
  assert.deepEqual(pick(regionDecode(vt)), { ok: true, session: 2, seq: 6 });
  assert.deepEqual(pick(regionDecode(vt, ECHO_REGION)), { ok: true, session: 2, seq: 9 });
  assert.equal(regionDecode(vt, ECHO_REGION).kindName, "echo");
});

test("end to end: after every write a profile emits, the region shows that write's marker", async () => {
  const cases = [
    [readJson(FIXTURE_PROFILE), 10000],
    [shipped("all-repaint"), 2000],
    [shipped("claude-tui"), 10000],
  ];
  for (const [profile, ms] of cases) {
    const r = await runVirtual({ profile, seed: 4, session: 6, durationMs: ms });
    const vt = new MiniVt(profile.geometry.cols, profile.geometry.rows);
    vt.write(r.writes[0].bytes.toString("utf8"));
    for (const w of r.writes.slice(1)) {
      const text = w.bytes.toString("utf8");
      vt.write(text);
      const own = parseOverlays(text).filter((o) => o.row === MARKER_REGION.row);
      assert.equal(
        own.length,
        1,
        `${profile.id}: every scheduled write carries exactly one output marker`,
      );
      assert.deepEqual(
        pick(regionDecode(vt)),
        { ok: true, session: 6, seq: own[0].decoded.seq },
        profile.id,
      );
    }
  }
});

// ---------------------------------------------------------------------------
// Producer stall accounting
// ---------------------------------------------------------------------------

test("stall accounting: an injected slow writer below saturation", async () => {
  const profile = shipped("all-repaint");
  const delay = 10_000_000; // 10 ms per write; frames are 33.3 ms apart
  const r = await runVirtual({ profile, durationMs: 10000, writeDelayNs: delay });
  assert.equal(r.exit.writes, 301, "preamble + 300 frames");
  assert.equal(
    r.exit.blocked_ns,
    r.exit.writes * delay,
    "every write's blocked time is counted, and nothing else",
  );
  assert.equal(r.exit.wall_ns, 10_000_000_000);
  const stallPct = (r.exit.blocked_ns / r.exit.wall_ns) * 100;
  assert.ok(Math.abs(stallPct - 30.1) < 0.01, `stall ${stallPct}%`);
  assert.equal(r.exit.bytes_written, r.exit.bytes_scheduled, "nothing falls behind");
  const markers = r.records.filter((x) => x.type === "marker");
  assert.equal(markers.length, 100);
  for (const m of markers)
    assert.equal(BigInt(m.t_written_ns) - BigInt(m.t_emit_ns), BigInt(delay));
  const progress = r.records.filter((x) => x.type === "progress");
  assert.ok(progress.length >= 19);
  for (let i = 1; i < progress.length; i += 1)
    assert.ok(progress[i].blocked_ns >= progress[i - 1].blocked_ns);
});

test("stall accounting: a saturated writer falls behind without skipping a byte", async () => {
  const profile = shipped("all-repaint");
  const fast = await runVirtual({ profile, durationMs: 10000 });
  const slow = await runVirtual({ profile, durationMs: 10000, writeDelayNs: 50_000_000 });
  assert.ok(
    fast.bytes.subarray(0, slow.bytes.length).equals(slow.bytes),
    "the slow stream is a prefix of the fast one",
  );
  assert.ok(slow.exit.blocked_ns / slow.exit.wall_ns > 0.99, "blocked for the whole run");
  const delivered = slow.exit.bytes_written / slow.exit.bytes_scheduled;
  assert.ok(delivered > 0.6 && delivered < 0.72, `delivered ratio ${delivered}`);
  assert.equal(
    slow.exit.bytes_scheduled,
    fast.exit.bytes_scheduled,
    "the schedule does not depend on the writer",
  );
  assert.ok(
    slow.exit.queued > 0 && slow.exit.lag_us > 1_000_000,
    `queued ${slow.exit.queued}, lag ${slow.exit.lag_us} us`,
  );
  const last = slow.records.filter((x) => x.type === "marker").at(-1);
  assert.ok(
    BigInt(last.t_emit_ns) - BigInt(last.t_sched_ns) > 1_000_000_000n,
    "markers are written late, not skipped",
  );
  const progress = slow.records.filter((x) => x.type === "progress");
  for (const p of progress) assert.ok(p.bytes_scheduled >= p.bytes_written);
});

test("the fd writer delivers every byte through partial writes and EAGAIN", () => {
  const got = [];
  let calls = 0;
  let sleeps = 0;
  const writeSync = (fd, buf, off, len) => {
    calls += 1;
    assert.equal(fd, 9);
    if (calls === 2) throw Object.assign(new Error("again"), { code: "EAGAIN" });
    if (calls === 4) return 0;
    const n = Math.min(len, 3);
    got.push(buf.subarray(off, off + n).toString());
    return n;
  };
  createFdWriter({ fd: 9, writeSync, sleepMs: () => (sleeps += 1) })(Buffer.from("abcdefgh"));
  assert.equal(got.join(""), "abcdefgh");
  assert.equal(sleeps, 2, "EAGAIN and a zero-byte write each back off once");
  const closed = createFdWriter({
    writeSync: () => {
      throw Object.assign(new Error("gone"), { code: "EPIPE" });
    },
  });
  assert.throws(() => closed(Buffer.from("x")), /gone/);
});

test("composeWrite puts overlays inside a synchronized frame and after anything else", () => {
  assert.equal(
    composeWrite(`${SYNC_BEGIN}x${SYNC_END}`, "OV", true),
    `${SYNC_BEGIN}xOV${SYNC_END}`,
  );
  assert.equal(composeWrite("abc", "OV", false), "abcOV");
  assert.equal(composeWrite("", "OV", false), "OV");
  assert.throws(() => composeWrite("not synced", "OV", true), /SYNC_END/);
});

// ---------------------------------------------------------------------------
// Echo and calibration modes
// ---------------------------------------------------------------------------

test("--echo answers each received byte with an echo marker in row 1", async () => {
  const r = await runVirtual({
    profile: shipped("all-repaint"),
    durationMs: 3000,
    echo: true,
    inputs: [
      { tNs: 1_000_000_000n, bytes: "a" },
      { tNs: 1_500_000_000n, bytes: "b" },
    ],
  });
  const echoes = r.records.filter((x) => x.type === "echo");
  assert.deepEqual(
    echoes.map((e) => [e.seq, e.byte, e.t_read_ns]),
    [
      [0, 97, "1000000000"],
      [1, 98, "1500000000"],
    ],
  );
  for (const e of echoes) assert.ok(BigInt(e.t_emit_ns) >= BigInt(e.t_read_ns));
  const rowOne = parseOverlays(r.bytes).filter((o) => o.row === ECHO_REGION.row);
  assert.ok(rowOne.length > 30, "every later frame re-draws the echo marker");
  assert.ok(
    rowOne.every((o) => o.decoded.ok && o.decoded.kindName === "echo" && o.decoded.session === 0),
  );
  assert.equal(rowOne.at(-1).decoded.seq, 1);
  assert.equal(r.exit.echoes, 2);
  assert.ok(r.exit.echo_bytes > 0);
  assert.equal(
    r.exit.bytes_written,
    r.exit.bytes_scheduled,
    "echo bytes are not part of the scheduled stream",
  );
});

test("--echo: Ctrl+C (0x03) stops the generator, since raw mode disables SIGINT", async () => {
  const r = await runVirtual({
    profile: shipped("claude-tui"),
    durationMs: 5000,
    echo: true,
    inputs: [{ tNs: 500_000_000n, bytes: "\x03" }],
  });
  assert.equal(r.exit.reason, "interrupt");
  assert.ok(r.exit.wall_ns < 1_000_000_000);
  const plain = await runVirtual({
    profile: shipped("claude-tui"),
    durationMs: 1000,
    inputs: [{ tNs: 100_000_000n, bytes: "a\x03" }],
  });
  assert.equal(plain.exit.reason, "duration", "without --echo input is ignored");
  assert.equal(plain.records.filter((x) => x.type === "echo").length, 0);
});

test("--calibration paints only a static calibration marker", async () => {
  const r = await runVirtual({
    profile: readJson(QUIET_PROFILE),
    durationMs: 3000,
    calibration: true,
    session: 2,
  });
  const overlays = parseOverlays(r.bytes);
  assert.equal(overlays.length, 3, "re-drawn once a second");
  for (const o of overlays) assert.deepEqual(pick(o.decoded), { ok: true, session: 2, seq: 0 });
  assert.ok(overlays.every((o) => o.decoded.kindName === "calibration"));
  assert.deepEqual(
    r.records.filter((x) => x.type === "marker").map((m) => [m.kind, m.seq]),
    [["calibration", 0]],
  );
});

// ---------------------------------------------------------------------------
// Command line, slots, host ids, Windows console
// ---------------------------------------------------------------------------

test("parseArgs takes D13's identical command line and refuses anything else", () => {
  const d13 = [
    "--run-dir",
    "/tmp/r",
    "--profile",
    "claude-tui",
    "--seed",
    "42",
    "--termhost-bench-generator",
    "run-7",
  ];
  assert.deepEqual(parseArgs(d13), {
    runDir: "/tmp/r",
    profile: "claude-tui",
    seed: 42,
    runId: "run-7",
    echo: false,
    calibration: false,
    durationMs: null,
    help: false,
  });
  assert.equal(parseArgs([...d13, "--echo", "--duration-ms", "500"]).durationMs, 500);
  assert.throws(
    () => parseArgs(d13.slice(0, 6)),
    (e) => e instanceof UsageError && /--termhost-bench-generator is required/.test(e.message),
  );
  assert.throws(() => parseArgs([...d13.slice(0, 4), "--seed", "-1", ...d13.slice(6)]), /--seed/);
  assert.throws(
    () => parseArgs([...d13.slice(0, 4), "--seed", "4294967296", ...d13.slice(6)]),
    /--seed/,
  );
  assert.throws(
    () => parseArgs([...d13.slice(0, 6), "--termhost-bench-generator", "has space"]),
    /run id/,
  );
  assert.throws(() => parseArgs([...d13, "--bogus"]), /unknown argument/);
  assert.throws(() => parseArgs(["--run-dir"]), /needs a value/);
  assert.equal(parseArgs(["--help"]).help, true);
});

test("claimSlot hands out slots 0, 1, 2 with exclusive creates and records the claimant", () => {
  const dir = mkdtempSync(join(tmpdir(), "termhost-slot-"));
  try {
    const claims = [0, 1, 2].map((i) => claimSlot(dir, { run_id: "r", pid: 1000 + i }));
    assert.deepEqual(
      claims.map((c) => c.slot),
      [0, 1, 2],
    );
    const slot1 = readJson(join(dir, "slot-1"));
    assert.deepEqual(slot1, { schema: "termhost-bench/slot/v1", slot: 1, run_id: "r", pid: 1001 });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("host terminal ids come from an allowlist, never a wholesale env dump", () => {
  const env = {
    WT_SESSION: "abc",
    TMUX_PANE: "%3",
    QONTINUI_TERMINAL_ID: "t-1",
    SECRET_TOKEN: "nope",
    TERM_PROGRAM: "",
  };
  assert.deepEqual(hostTerminalIds(env), {
    WT_SESSION: "abc",
    TMUX_PANE: "%3",
    QONTINUI_TERMINAL_ID: "t-1",
  });
  assert.ok(HOST_TERMINAL_ENV.includes("TERMHOST_BENCH_TERMINAL_ID"));
});

test("on a Windows console the output code page becomes UTF-8 and is restored after", () => {
  assert.equal(parseCodePage("Active code page: 437"), 437);
  assert.equal(parseCodePage("Aktive Codepage: 850."), 850);
  assert.equal(parseCodePage(""), null);
  const calls = [];
  let cp = 437;
  const run = (exe, args) => {
    calls.push([exe, ...args]);
    if (args.length) cp = Number(args[0]);
    return { stdout: `Active code page: ${cp}\r\n` };
  };
  const state = prepareConsole({
    platform: "win32",
    stdoutIsTty: true,
    run,
    env: { SystemRoot: "C:\\Windows" },
  });
  assert.deepEqual([state.applied, state.cp_before, state.cp_after], [true, 437, 65001]);
  assert.ok(calls.some((c) => c[0].endsWith("chcp.com") && c[1] === "65001"));
  restoreConsole(state, { run });
  assert.equal(cp, 437);
  assert.deepEqual(prepareConsole({ platform: "linux", stdoutIsTty: true, run }), {
    applied: false,
  });
  assert.deepEqual(prepareConsole({ platform: "win32", stdoutIsTty: false, run }), {
    applied: false,
  });
  const already = prepareConsole({
    platform: "win32",
    stdoutIsTty: true,
    run: (e, a) => ({ stdout: "Active code page: 65001" }),
    env: {},
  });
  assert.equal(already.cp_after, 65001);
});

// ---------------------------------------------------------------------------
// The real CLI (D13), as processes
// ---------------------------------------------------------------------------

function startGenerator(runDir, extra = [], profile = QUIET_PROFILE) {
  const child = spawn(
    process.execPath,
    [
      GENERATOR,
      "--run-dir",
      runDir,
      "--profile",
      profile,
      "--seed",
      "7",
      "--termhost-bench-generator",
      "ci-run-1",
      ...extra,
    ],
    { stdio: ["pipe", "pipe", "pipe"], windowsHide: true },
  );
  const out = [];
  const err = [];
  child.stdout.on("data", (b) => out.push(b));
  child.stderr.on("data", (b) => err.push(b));
  const done = new Promise((res) => child.on("exit", (code, signal) => res({ code, signal })));
  return {
    child,
    done,
    stdout: () => Buffer.concat(out),
    stderr: () => Buffer.concat(err).toString(),
  };
}

async function waitFor(predicate, what, timeoutMs = 20000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (predicate()) return;
    await new Promise((r) => setTimeout(r, 25));
  }
  throw new Error(`timed out waiting for ${what}`);
}

const genRecords = (runDir, slot) => {
  const path = join(runDir, `gen-${slot}.jsonl`);
  if (!existsSync(path)) return [];
  return readFileSync(path, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((l) => JSON.parse(l));
};
const hasMarker = (runDir, slot) => genRecords(runDir, slot).some((r) => r.type === "marker");

async function withRunDir(fn) {
  const dir = mkdtempSync(join(tmpdir(), "termhost-gen-"));
  try {
    await fn(dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test(
  "CLI: the D13 command line claims slot 0, logs its records, and exits 0 at --duration-ms",
  { timeout: 60000 },
  async () => {
    await withRunDir(async (dir) => {
      const g = startGenerator(dir, ["--duration-ms", "1200"]);
      const { code } = await g.done;
      assert.equal(code, 0, g.stderr());
      const slot = readJson(join(dir, "slot-0"));
      assert.equal(slot.pid, g.child.pid);
      assert.equal(
        slot.ppid,
        process.pid,
        "the slot names the parent (the session shell in a real host)",
      );
      assert.equal(slot.run_id, "ci-run-1");
      const records = genRecords(dir, 0);
      assert.equal(records[0].type, "start");
      assert.equal(records[0].profile, "quiet-fixture");
      assert.equal(records[0].session, 0);
      assert.match(records[0].t0_ns, /^\d+$/, "absolute ns timestamps are decimal strings");
      const markers = records.filter((r) => r.type === "marker");
      assert.ok(markers.length >= 5, `${markers.length} markers`);
      assert.deepEqual(
        markers.map((m) => m.seq),
        markers.map((_, i) => i),
      );
      for (const m of markers) assert.ok(BigInt(m.t_written_ns) >= BigInt(m.t_emit_ns));
      assert.ok(records.some((r) => r.type === "progress"));
      assert.equal(records.at(-1).type, "exit");
      assert.equal(records.at(-1).reason, "duration");
      const overlays = parseOverlays(g.stdout());
      assert.ok(overlays.length >= markers.length);
      assert.ok(
        overlays.every(
          (o) => o.decoded.ok && o.decoded.session === 0 && o.decoded.kindName === "output",
        ),
      );
    });
  },
);

test("CLI: the stop file ends a generator with exit 0", { timeout: 60000 }, async () => {
  await withRunDir(async (dir) => {
    const g = startGenerator(dir);
    await waitFor(() => hasMarker(dir, 0), "the first marker");
    writeFileSync(join(dir, "stop"), "");
    const { code } = await g.done;
    assert.equal(code, 0, g.stderr());
    assert.equal(genRecords(dir, 0).at(-1).reason, "stop_file");
  });
});

test("CLI: stdin EOF ends a generator with exit 0", { timeout: 60000 }, async () => {
  await withRunDir(async (dir) => {
    const g = startGenerator(dir);
    await waitFor(() => hasMarker(dir, 0), "the first marker");
    g.child.stdin.end();
    const { code } = await g.done;
    assert.equal(code, 0, g.stderr());
    assert.equal(genRecords(dir, 0).at(-1).reason, "stdin_eof");
  });
});

test(
  "CLI: concurrent generators with one command line claim distinct slots and mark their own session",
  { timeout: 60000 },
  async () => {
    await withRunDir(async (dir) => {
      const gens = [startGenerator(dir), startGenerator(dir), startGenerator(dir)];
      await waitFor(
        () => [0, 1, 2].every((s) => hasMarker(dir, s)),
        "three generators' first markers",
      );
      writeFileSync(join(dir, "stop"), "");
      const exits = await Promise.all(gens.map((g) => g.done));
      assert.deepEqual(
        exits.map((e) => e.code),
        [0, 0, 0],
      );
      const slotOf = new Map([0, 1, 2].map((s) => [readJson(join(dir, `slot-${s}`)).pid, s]));
      assert.equal(slotOf.size, 3);
      for (const g of gens) {
        const slot = slotOf.get(g.child.pid);
        const sessions = new Set(parseOverlays(g.stdout()).map((o) => o.decoded.session));
        assert.deepEqual([...sessions], [slot], `pid ${g.child.pid} marks session ${slot}`);
      }
    });
  },
);

test("CLI: --echo answers bytes on stdin with echo markers", { timeout: 60000 }, async () => {
  await withRunDir(async (dir) => {
    const g = startGenerator(dir, ["--echo"]);
    await waitFor(() => hasMarker(dir, 0), "the first marker");
    g.child.stdin.write("ab");
    await waitFor(
      () => genRecords(dir, 0).filter((r) => r.type === "echo").length === 2,
      "two echo records",
    );
    writeFileSync(join(dir, "stop"), "");
    const { code } = await g.done;
    assert.equal(code, 0, g.stderr());
    assert.deepEqual(
      genRecords(dir, 0)
        .filter((r) => r.type === "echo")
        .map((r) => [r.seq, r.byte]),
      [
        [0, 97],
        [1, 98],
      ],
    );
    const echoes = parseOverlays(g.stdout()).filter((o) => o.row === ECHO_REGION.row);
    assert.deepEqual([...new Set(echoes.map((o) => o.decoded.seq))], [0, 1]);
    assert.ok(echoes.every((o) => o.decoded.kindName === "echo"));
  });
});

test(
  "CLI: a usage error exits 2 and a bad profile or run dir exits 1",
  { timeout: 60000 },
  async () => {
    await withRunDir(async (dir) => {
      const usage = spawn(process.execPath, [GENERATOR, "--run-dir", dir], {
        stdio: ["ignore", "pipe", "pipe"],
      });
      const usageExit = await new Promise((r) => usage.on("exit", r));
      assert.equal(usageExit, 2);
      const badProfile = startGenerator(dir, [], join(dir, "missing.json"));
      assert.equal((await badProfile.done).code, 1);
      const badDir = startGenerator(join(dir, "nope"));
      assert.equal((await badDir.done).code, 1);
      assert.equal(
        existsSync(join(dir, "slot-0")),
        false,
        "nothing claims a slot before its arguments are valid",
      );
    });
  },
);

function pick(decoded) {
  return decoded.ok
    ? { ok: true, session: decoded.session, seq: decoded.seq }
    : { ok: false, reason: decoded.reason };
}
