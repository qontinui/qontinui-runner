/**
 * calibrate.mjs against a small synthetic `script --log-timing` recording
 * (fixtures/calibration-session.mjs): both timing formats parse, the analysis finds
 * the phases a reader can derive by hand, the fitted profile is valid and carries
 * NUMBERS ONLY, and re-calibrating the generator's own output reproduces the fit.
 *
 *   node --test scripts/termhost-bench/__tests__/calibrate.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  ANALYSIS_DEFAULTS,
  analyzeChunks,
  assertNumbersOnly,
  buildRuns,
  calibrateFiles,
  classifyFrames,
  fitProfile,
  parseCalibrateArgs,
  parseTiming,
  quantileTable,
  reconstructChunks,
  segmentFrames,
  stripScriptHeader,
} from "../calibrate.mjs";
import { validateProfile } from "../profile.mjs";
import { stripOverlays } from "../marker.mjs";
import { runVirtual } from "../generator/ai-session.mjs";
import {
  CANARY,
  EXPECTED,
  GEOMETRY,
  recordedChunks,
  renderAdvancedTiming,
  renderClassicTiming,
  renderTypescript,
  sessionEvents,
} from "./fixtures/calibration-session.mjs";

const CALIBRATE = join(dirname(fileURLToPath(import.meta.url)), "..", "calibrate.mjs");
const CALIBRATED_AT = "2026-10-11T12:00:00.000Z";

function withRecording(fn, { advanced = false, resize = false } = {}) {
  const dir = mkdtempSync(join(tmpdir(), "termhost-calibrate-"));
  try {
    const timing = join(dir, "s1.tm");
    const typescript = join(dir, "s1.ts");
    writeFileSync(
      timing,
      advanced ? renderAdvancedTiming(undefined, { resize }) : renderClassicTiming(),
    );
    writeFileSync(typescript, renderTypescript());
    return fn({ dir, timing, typescript });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

const close = (a, b, eps = 1e-6) => Math.abs(a - b) <= eps;

test("parseTiming reads the classic format", () => {
  const t = parseTiming("0.000000 15\n1.500000 120\n\n0.100000 8192\n");
  assert.equal(t.format, "classic");
  assert.deepEqual(t.entries, [
    { type: "O", delay: 0, bytes: 15 },
    { type: "O", delay: 1.5, bytes: 120 },
    { type: "O", delay: 0.1, bytes: 8192 },
  ]);
  assert.deepEqual(t.geometry, {});
  assert.throws(() => parseTiming("0.1 12 extra\n"), /timing line 1/);
  assert.throws(() => parseTiming(""), /empty/);
});

test("parseTiming reads the advanced format: geometry headers, input entries, resizes", () => {
  const t = parseTiming(renderAdvancedTiming());
  assert.equal(t.format, "advanced");
  assert.deepEqual(t.geometry, { cols: 80, rows: 24 });
  assert.equal(t.resized, false);
  assert.equal(t.entries.filter((e) => e.type === "I").length, 2);
  assert.equal(t.entries.filter((e) => e.type === "O").length, recordedChunks().length);
  assert.equal(parseTiming(renderAdvancedTiming(undefined, { resize: true })).resized, true);
  assert.throws(() => parseTiming("O 0.1 lots\n"), /byte count/);
  assert.throws(() => parseTiming("O 0.1 5\nX 0.1 5\n"), /timing line 2/);
});

test("the typescript header line is skipped; output bytes pair with timing entries", () => {
  const ts = renderTypescript();
  assert.ok(stripScriptHeader(ts).subarray(0, 8).equals(Buffer.from("\x1b[?1049h")));
  assert.ok(
    stripScriptHeader(Buffer.from("raw")).equals(Buffer.from("raw")),
    "no header, nothing skipped",
  );
  for (const text of [renderClassicTiming(), renderAdvancedTiming()]) {
    const chunks = reconstructChunks(parseTiming(text), ts);
    const want = recordedChunks();
    assert.equal(chunks.length, want.length);
    chunks.forEach((c, i) => {
      assert.ok(c.bytes.equals(want[i].bytes), `chunk ${i} bytes`);
      assert.ok(close(c.t, want[i].t), `chunk ${i} at ${c.t}, want ${want[i].t}`);
    });
  }
  assert.throws(() => reconstructChunks(parseTiming("0 999999\n"), ts), /typescript holds/);
});

test("segmentFrames: a synchronized block is one frame even when split across 8 KiB reads", () => {
  const frames = segmentFrames(recordedChunks());
  const events = sessionEvents();
  assert.equal(frames.length, events.length, "one frame per written event");
  frames.forEach((f, i) => {
    assert.equal(f.bytes, events[i].bytes.length, `frame ${i} size`);
    assert.equal(f.sync, events[i].sync, `frame ${i} sync`);
  });
  assert.ok(
    events.some((e) => e.bytes.length > 8192),
    "the fixture does split a frame",
  );
  const merged = segmentFrames([
    { t: 1, bytes: Buffer.from("ab") },
    { t: 1.002, bytes: Buffer.from("cd") },
    { t: 1.2, bytes: Buffer.from("e") },
  ]);
  assert.deepEqual(
    merged.map((f) => f.bytes),
    [4, 1],
    "unsynchronized reads closer than frameGapMs are one write",
  );
});

test("classifyFrames and buildRuns find the phases derivable from the timeline", () => {
  const frames = classifyFrames(segmentFrames(recordedChunks()));
  const events = sessionEvents();
  frames.forEach((f, i) => assert.equal(f.cls, events[i].cls, `frame ${i} class`));
  const runs = buildRuns(frames);
  assert.equal(runs.length, EXPECTED.runs.length);
  runs.forEach((r, i) => {
    const [cls, start, end, count] = EXPECTED.runs[i];
    assert.equal(r.cls, cls, `run ${i}`);
    assert.ok(
      close(r.start, start) && close(r.end, end),
      `run ${i} ${cls}: ${r.start}-${r.end}, want ${start}-${end}`,
    );
    assert.equal(r.frames, count, `run ${i} frames`);
  });
});

test("fitProfile yields a valid calibrated profile with the hand-derived numbers", () => {
  const analysis = analyzeChunks(recordedChunks());
  const profile = fitProfile([analysis], { geometry: GEOMETRY, calibratedAt: CALIBRATED_AT });
  assert.deepEqual(validateProfile(profile).errors, []);
  assert.equal(profile.calibrated, true);
  assert.equal(profile.provenance.kind, "calibration");
  assert.equal(profile.provenance.recordings, 1);
  assert.equal(profile.provenance.duration_s, EXPECTED.duration_s);
  assert.deepEqual(profile.geometry, GEOMETRY);
  assert.equal(profile.alt_screen, true, "the recording entered the alternate screen");
  assert.equal(profile.initial_phase, EXPECTED.initial_phase);
  assert.deepEqual(profile.transitions, EXPECTED.transitions);
  for (const [cls, fps] of Object.entries(EXPECTED.fps))
    assert.equal(profile.phases[cls].fps, fps, `${cls} fps`);
  assert.deepEqual(
    Object.fromEntries(Object.entries(profile.phases).map(([k, v]) => [k, v.kind])),
    {
      idle: "idle",
      thinking: "spinner",
      stream: "stream",
      repaint: "repaint",
      tool_burst: "burst",
    },
  );
  assert.deepEqual(profile.phases.stream.duration_ms, [
    [0, 50],
    [0.1, 50],
    [0.25, 50],
    [0.5, 250],
    [0.75, 1500],
    [0.9, 1500],
    [1, 1500],
  ]);
  assert.deepEqual(
    profile.phases.idle.duration_ms.map(([, v]) => v),
    [1450, 1450, 1450, 1450, 2000, 2000, 2000],
  );
  assert.ok(profile.phases.tool_burst.chunk_bytes.every(([, v]) => v === 3000));
  const events = sessionEvents();
  const total = events.reduce((a, e) => a + e.bytes.length, 0);
  const sync = events.filter((e) => e.sync).reduce((a, e) => a + e.bytes.length, 0);
  assert.equal(
    profile.expected.bytes_per_s,
    Math.round((total / EXPECTED.duration_s) * 1000) / 1000,
  );
  assert.equal(profile.expected.sync_share, Math.round((sync / total) * 1000) / 1000);
  assert.ok(
    close(
      Object.values(profile.expected.phase_share).reduce((a, b) => a + b, 0),
      1,
      0.003,
    ),
  );
  assert.equal(profile.expected.phase_share.idle, Math.round((3.45 / 9.75) * 1000) / 1000);
});

test("NUMBERS ONLY: no recorded content reaches the fitted profile", () => {
  const profile = withRecording(({ timing, typescript }) =>
    calibrateFiles([{ timing, typescript }], { geometry: GEOMETRY, calibratedAt: CALIBRATED_AT }),
  );
  const text = JSON.stringify(profile);
  for (const leak of [
    CANARY,
    "case_0",
    "xterm-256color",
    "/dev/pts/3",
    "/bin/bash",
    "Script started",
  ]) {
    assert.ok(!text.includes(leak), `the profile leaked "${leak}"`);
  }
  assert.doesNotThrow(() => assertNumbersOnly(profile, { allow: ["claude-tui"] }));
  assert.throws(
    () => assertNumbersOnly({ ...profile, title: `session ${CANARY}` }, { allow: ["claude-tui"] }),
    /free-text string at \/title/,
  );
  assert.throws(() => assertNumbersOnly({ phases: { [CANARY]: {} } }), /free-text string/);
});

test("calibrateFiles takes geometry from advanced headers, refuses resizes and unknown geometry", () => {
  const advanced = withRecording(
    ({ timing, typescript }) =>
      calibrateFiles([{ timing, typescript }], { calibratedAt: CALIBRATED_AT }),
    { advanced: true },
  );
  assert.deepEqual(advanced.geometry, GEOMETRY);
  const classic = withRecording(({ timing, typescript }) =>
    calibrateFiles([{ timing, typescript }], { geometry: GEOMETRY, calibratedAt: CALIBRATED_AT }),
  );
  assert.deepEqual(classic.phases, advanced.phases, "both formats describe the same session");
  assert.throws(
    () => withRecording(({ timing, typescript }) => calibrateFiles([{ timing, typescript }])),
    /geometry is unknown/,
  );
  assert.throws(
    () =>
      withRecording(({ timing, typescript }) => calibrateFiles([{ timing, typescript }]), {
        advanced: true,
        resize: true,
      }),
    /resized/,
  );
  assert.throws(() => calibrateFiles([]), /no recordings/);
});

test("several recordings pool into one fit", () => {
  const one = analyzeChunks(recordedChunks());
  const profile = fitProfile([one, one, one], { geometry: GEOMETRY, calibratedAt: CALIBRATED_AT });
  assert.equal(profile.provenance.recordings, 3);
  assert.equal(profile.provenance.duration_s, 29.1, "3 x 9.7 s, rounded to the millisecond");
  assert.deepEqual(profile.transitions.idle, { thinking: 6 });
  assert.equal(profile.phases.repaint.fps, 30);
});

test("quantileTable is nearest-rank at fixed probabilities", () => {
  assert.deepEqual(quantileTable([7]), [
    [0, 7],
    [0.1, 7],
    [0.25, 7],
    [0.5, 7],
    [0.75, 7],
    [0.9, 7],
    [1, 7],
  ]);
  assert.deepEqual(
    quantileTable([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]).map(([, v]) => v),
    [1, 1, 3, 5, 8, 9, 10],
  );
  assert.throws(() => quantileTable([]), /needs samples/);
});

test("the calibrate CLI writes a valid profile from recording files and exits 0", () => {
  withRecording(({ dir, timing, typescript }) => {
    const out = join(dir, "claude-tui.json");
    const r = spawnSync(
      process.execPath,
      [
        CALIBRATE,
        "--timing",
        timing,
        "--typescript",
        typescript,
        "--cols",
        "80",
        "--rows",
        "24",
        "--out",
        out,
      ],
      { encoding: "utf8" },
    );
    assert.equal(r.status, 0, r.stderr);
    const profile = JSON.parse(readFileSync(out, "utf8"));
    assert.deepEqual(validateProfile(profile).errors, []);
    assert.equal(profile.id, "claude-tui");
    assert.ok(!readFileSync(out, "utf8").includes(CANARY));
    const bad = spawnSync(process.execPath, [CALIBRATE, "--timing", timing], { encoding: "utf8" });
    assert.equal(bad.status, 2);
    assert.match(bad.stderr, /one --typescript per --timing/);
  });
  assert.deepEqual(
    parseCalibrateArgs(["--timing", "a", "--typescript", "b", "--id", "claude-tui"]).recordings,
    [{ timing: "a", typescript: "b" }],
  );
  assert.throws(() => parseCalibrateArgs(["--cols", "x"]), /positive integer/);
});

test("round trip: re-calibrating the generator's output reproduces the fitted profile", async () => {
  const fitted = fitProfile([analyzeChunks(recordedChunks())], {
    geometry: GEOMETRY,
    calibratedAt: CALIBRATED_AT,
  });
  const gen = await runVirtual({ profile: fitted, seed: 3, session: 0, durationMs: 300000 });
  const chunks = gen.writes
    .map((w) => ({ t: Number(w.tNs) / 1e9, bytes: Buffer.from(stripOverlays(w.bytes), "utf8") }))
    .filter((c) => c.bytes.length > 0);
  const refit = fitProfile([analyzeChunks(chunks, ANALYSIS_DEFAULTS)], {
    geometry: GEOMETRY,
    calibratedAt: CALIBRATED_AT,
  });
  assert.deepEqual(Object.keys(refit.phases).sort(), Object.keys(fitted.phases).sort());
  for (const [cls, phase] of Object.entries(fitted.phases)) {
    if (phase.fps)
      assert.ok(
        Math.abs(refit.phases[cls].fps - phase.fps) / phase.fps < 0.1,
        `${cls} fps ${refit.phases[cls].fps} vs ${phase.fps}`,
      );
    for (const key of ["frame_bytes", "chunk_bytes"]) {
      if (!phase[key]) continue;
      const want = phase[key][3][1];
      const got = refit.phases[cls][key][3][1];
      assert.ok(Math.abs(got - want) / want < 0.15, `${cls} ${key} median ${got} vs ${want}`);
    }
  }
  for (const [from, targets] of Object.entries(fitted.transitions)) {
    assert.deepEqual(
      Object.keys(refit.transitions[from]).sort(),
      Object.keys(targets).sort(),
      `${from} transitions`,
    );
  }
  assert.equal(refit.alt_screen, fitted.alt_screen);
  // The recording's own statistics. Sync share is set by the per-phase byte mix and
  // comes back tight; bytes/s and phase shares also carry this 10 s recording's path
  // (one pass through the chain) against the chain's long-run mix, hence wider bounds.
  const want = fitted.expected;
  const got = refit.expected;
  assert.ok(
    Math.abs(got.sync_share - want.sync_share) < 0.02,
    `sync_share ${got.sync_share} vs ${want.sync_share}`,
  );
  assert.ok(
    Math.abs(got.bytes_per_s - want.bytes_per_s) / want.bytes_per_s < 0.35,
    `bytes_per_s ${got.bytes_per_s} vs ${want.bytes_per_s}`,
  );
  for (const [cls, share] of Object.entries(want.phase_share)) {
    assert.ok(
      Math.abs(got.phase_share[cls] - share) < 0.08,
      `${cls} share ${got.phase_share[cls]} vs ${share}`,
    );
  }
});
