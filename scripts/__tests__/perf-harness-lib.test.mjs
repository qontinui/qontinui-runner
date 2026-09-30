/**
 * Unit tests for the pure half of the many-sessions perf harness.
 *
 *   node --test scripts/__tests__/perf-harness-lib.test.mjs
 *
 * These cover the parts that must be right when nobody is watching: the
 * percentile maths, the `tracing` span-close parser (fed real captured log
 * lines), and the before/after table diffing. All run without a runner, a
 * supervisor or a network.
 */

import { test } from "node:test";
import assert from "node:assert/strict";

import {
  percentile,
  stats,
  parseTracingDuration,
  splitTopLevel,
  parseSpanFields,
  parseSpanCloseLine,
  parseSpawnSpans,
  aggregateSpans,
  flattenRun,
  compareRuns,
  compareMetricKeys,
  metricMeta,
  fmtNumber,
  renderMarkdownTable,
  renderSingleRun,
  renderComparison,
  foldFrontendSample,
  foldChunkRates,
  GENERATOR_PRESETS,
  buildGeneratorCommand,
  unwrapTransportStats,
  framesAbove,
  normalizeFrontendTransport,
  foldIpcFallback,
  foldTransport,
  renderTransportSection,
} from "../perf-harness-lib.mjs";

// ---------------------------------------------------------------------------
// Fixtures — captured from a real runner log sink. The runner's file layer is
// `fmt::layer().with_ansi(false).with_span_events(FmtSpan::CLOSE)` with a
// `%Y-%m-%d %H:%M:%S%.3f` timer (src-tauri/src/logging.rs), and the spans are
// `tracing::debug_span!("terminal_spawn.<segment>", terminal_id = %id)`.
// ---------------------------------------------------------------------------

const LOG_FIXTURE = [
  "2026-08-05 00:31:53.554  INFO qontinui_runner::settings: Skipping tier migration persist for secondary runner",
  "2026-08-05 00:31:54.101 DEBUG terminal_spawn.resolve_config_dir{terminal_id=0fee26c7-338a-4314-95dc-2ea338a5715b}: qontinui_runner::terminal::session: close time.busy=8.42ms time.idle=112µs",
  "2026-08-05 00:31:54.180 DEBUG terminal_spawn.identity_seam{terminal_id=0fee26c7-338a-4314-95dc-2ea338a5715b}:terminal_spawn.claude_hook_materialize{terminal_id=0fee26c7-338a-4314-95dc-2ea338a5715b}: qontinui_runner::terminal::session: close time.busy=21.9ms time.idle=1.20ms",
  "2026-08-05 00:31:54.260 DEBUG terminal_spawn.identity_seam{terminal_id=0fee26c7-338a-4314-95dc-2ea338a5715b}:terminal_spawn.coord_mcp_provision{terminal_id=0fee26c7-338a-4314-95dc-2ea338a5715b}: qontinui_runner::terminal::session: close time.busy=46.3ms time.idle=980µs",
  "2026-08-05 00:31:54.331 DEBUG terminal_spawn.identity_seam{terminal_id=0fee26c7-338a-4314-95dc-2ea338a5715b}: qontinui_runner::terminal::session: close time.busy=141ms time.idle=3.10ms",
  "2026-08-05 00:31:54.400 DEBUG terminal_spawn.record_open{terminal_id=0fee26c7-338a-4314-95dc-2ea338a5715b}: qontinui_runner::terminal::session: close time.busy=63.7ms time.idle=402µs",
  "2026-08-05 00:31:54.470 DEBUG terminal_spawn.manager_create: qontinui_runner::commands::terminal: close time.busy=312ms time.idle=5.00ms",
  "2026-08-05 00:31:54.480 DEBUG terminal_spawn.coord_register: qontinui_runner::commands::terminal: close time.busy=1.20s time.idle=90ns",
  "2026-08-05 00:31:55.010  WARN qontinui_runner::terminal::session: something unrelated",
  "2026-08-05 00:31:55.900 DEBUG some_other_span{a=1}: qontinui_runner::other: close time.busy=1.00ms time.idle=0ns",
].join("\n");

// ---------------------------------------------------------------------------
// Percentiles
// ---------------------------------------------------------------------------

test("percentile uses nearest-rank and never invents an unobserved value", () => {
  const values = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100];
  assert.equal(percentile(values, 50), 50);
  assert.equal(percentile(values, 95), 100);
  assert.equal(percentile(values, 90), 90);
  assert.equal(percentile(values, 0), 10);
  assert.equal(percentile(values, 100), 100);
  // Every result is a member of the input — the point of nearest-rank.
  for (const p of [1, 25, 50, 75, 95, 99]) {
    assert.ok(values.includes(percentile(values, p)), `p${p} not an observed value`);
  }
});

test("percentile handles unsorted input, single values and empties", () => {
  assert.equal(percentile([5, 1, 4, 2, 3], 50), 3);
  assert.equal(percentile([42], 95), 42);
  assert.equal(percentile([], 50), null);
  assert.equal(percentile(null, 50), null);
});

test("percentile rejects an out-of-range p", () => {
  assert.throws(() => percentile([1, 2, 3], 101), RangeError);
  assert.throws(() => percentile([1, 2, 3], -1), RangeError);
});

test("stats reports the full distribution and ignores non-finite samples", () => {
  const s = stats([100, 200, 300, NaN, 400, Infinity]);
  assert.equal(s.count, 4);
  assert.equal(s.min, 100);
  assert.equal(s.max, 400);
  assert.equal(s.p50, 200);
  assert.equal(s.p95, 400);
  assert.equal(s.mean, 250);
  assert.equal(s.total, 1000);
});

test("stats on an empty sample is all-null rather than zero", () => {
  const s = stats([]);
  assert.equal(s.count, 0);
  assert.equal(s.p50, null);
  assert.equal(s.p95, null);
  assert.equal(s.mean, null);
});

// ---------------------------------------------------------------------------
// tracing duration parsing
// ---------------------------------------------------------------------------

test("parseTracingDuration covers every unit std::time::Duration emits", () => {
  assert.equal(parseTracingDuration("1.23ms"), 1.23);
  assert.equal(parseTracingDuration("500ms"), 500);
  assert.equal(parseTracingDuration("1.5s"), 1500);
  assert.equal(parseTracingDuration("2s"), 2000);
  assert.ok(Math.abs(parseTracingDuration("912µs") - 0.912) < 1e-9);
  assert.ok(Math.abs(parseTracingDuration("912us") - 0.912) < 1e-9);
  assert.ok(Math.abs(parseTracingDuration("912\u03BCs") - 0.912) < 1e-9);
  assert.ok(Math.abs(parseTracingDuration("84ns") - 0.000084) < 1e-12);
  assert.equal(parseTracingDuration("0ns"), 0);
});

test("parseTracingDuration rejects junk instead of guessing", () => {
  assert.equal(parseTracingDuration("12"), null);
  assert.equal(parseTracingDuration("12 ms"), null);
  assert.equal(parseTracingDuration("ms"), null);
  assert.equal(parseTracingDuration("12min"), null);
  assert.equal(parseTracingDuration(undefined), null);
  assert.equal(parseTracingDuration(42), null);
});

// ---------------------------------------------------------------------------
// Depth-aware splitting
// ---------------------------------------------------------------------------

test("splitTopLevel does not split inside braces, quotes, or a Rust `::` path", () => {
  assert.deepEqual(splitTopLevel("a{k=1}:b{k=2}: target::path: ", ":"), [
    "a{k=1}",
    "b{k=2}",
    " target::path",
    " ",
  ]);
  assert.deepEqual(splitTopLevel('a{path="C:\\\\x"}:b: t: ', ":"), [
    'a{path="C:\\\\x"}',
    "b",
    " t",
    " ",
  ]);
  // A three-segment module path must survive intact.
  assert.deepEqual(splitTopLevel("s: qontinui_runner::terminal::session: ", ":"), [
    "s",
    " qontinui_runner::terminal::session",
    " ",
  ]);
});

test("parseSpanFields unquotes values and tolerates spaces inside quotes", () => {
  assert.deepEqual(parseSpanFields("terminal_id=abc"), { terminal_id: "abc" });
  assert.deepEqual(parseSpanFields('a=1 b="two words" c=3'), {
    a: "1",
    b: "two words",
    c: "3",
  });
  assert.deepEqual(parseSpanFields(""), {});
});

// ---------------------------------------------------------------------------
// Span close parsing
// ---------------------------------------------------------------------------

test("parseSpanCloseLine reads a flat span close", () => {
  const line =
    "2026-08-05 00:31:54.101 DEBUG terminal_spawn.resolve_config_dir{terminal_id=abc}: qontinui_runner::terminal::session: close time.busy=8.42ms time.idle=112µs";
  const rec = parseSpanCloseLine(line);
  assert.equal(rec.span, "terminal_spawn.resolve_config_dir");
  assert.equal(rec.level, "DEBUG");
  assert.equal(rec.timestamp, "2026-08-05 00:31:54.101");
  assert.equal(rec.target, "qontinui_runner::terminal::session");
  assert.deepEqual(rec.fields, { terminal_id: "abc" });
  assert.equal(rec.busyMs, 8.42);
  assert.ok(Math.abs(rec.idleMs - 0.112) < 1e-9);
  assert.ok(Math.abs(rec.totalMs - 8.532) < 1e-9);
  assert.deepEqual(rec.parents, []);
});

test("parseSpanCloseLine attributes a nested close to the INNERMOST span", () => {
  const line =
    "2026-08-05 00:31:54.180 DEBUG terminal_spawn.identity_seam{terminal_id=abc}:terminal_spawn.claude_hook_materialize{terminal_id=abc}: qontinui_runner::terminal::session: close time.busy=21.9ms time.idle=1.20ms";
  const rec = parseSpanCloseLine(line);
  assert.equal(rec.span, "terminal_spawn.claude_hook_materialize");
  assert.deepEqual(rec.parents, ["terminal_spawn.identity_seam"]);
  assert.ok(Math.abs(rec.totalMs - 23.1) < 1e-9);
});

test("parseSpanCloseLine handles a fieldless span", () => {
  const rec = parseSpanCloseLine(
    "2026-08-05 00:31:54.470 DEBUG terminal_spawn.manager_create: qontinui_runner::commands::terminal: close time.busy=312ms time.idle=5.00ms",
  );
  assert.equal(rec.span, "terminal_spawn.manager_create");
  assert.deepEqual(rec.fields, {});
  assert.equal(rec.totalMs, 317);
});

test("parseSpanCloseLine returns null for non-span lines", () => {
  assert.equal(
    parseSpanCloseLine(
      "2026-08-05 00:31:53.554  INFO qontinui_runner::settings: Skipping tier migration persist",
    ),
    null,
  );
  assert.equal(parseSpanCloseLine(""), null);
  assert.equal(parseSpanCloseLine("garbage close time.busy=1ms time.idle=1ms"), null);
  assert.equal(parseSpanCloseLine(undefined), null);
});

test("parseSpawnSpans extracts only terminal_spawn.* closes from a real log slab", () => {
  const records = parseSpawnSpans(LOG_FIXTURE);
  assert.equal(records.length, 7);
  assert.deepEqual(
    [...new Set(records.map((r) => r.span))].sort(),
    [
      "terminal_spawn.claude_hook_materialize",
      "terminal_spawn.coord_mcp_provision",
      "terminal_spawn.coord_register",
      "terminal_spawn.identity_seam",
      "terminal_spawn.manager_create",
      "terminal_spawn.record_open",
      "terminal_spawn.resolve_config_dir",
    ],
  );
  // `some_other_span` and the INFO/WARN lines must not leak in.
  assert.ok(!records.some((r) => r.span === "some_other_span"));
});

test("parseSpawnSpans is empty (not throwing) on a log with no spans", () => {
  assert.deepEqual(parseSpawnSpans("2026-08-05 00:00:00.000  INFO a::b: hello"), []);
  assert.deepEqual(parseSpawnSpans(""), []);
  assert.deepEqual(parseSpawnSpans(null), []);
});

test("aggregateSpans produces per-segment stats sorted by name", () => {
  const agg = aggregateSpans(parseSpawnSpans(LOG_FIXTURE));
  assert.deepEqual(Object.keys(agg), [
    "terminal_spawn.claude_hook_materialize",
    "terminal_spawn.coord_mcp_provision",
    "terminal_spawn.coord_register",
    "terminal_spawn.identity_seam",
    "terminal_spawn.manager_create",
    "terminal_spawn.record_open",
    "terminal_spawn.resolve_config_dir",
  ]);
  // 1.20s + 90ns — the seconds unit must not be read as milliseconds.
  assert.ok(agg["terminal_spawn.coord_register"].p50 > 1199);
  assert.ok(agg["terminal_spawn.coord_register"].p50 < 1201);
  assert.equal(agg["terminal_spawn.manager_create"].count, 1);
});

test("aggregateSpans buckets repeated spawns of the same segment", () => {
  const twoSpawns = `${LOG_FIXTURE}\n${LOG_FIXTURE.replace(/0fee26c7/g, "11111111").replace(
    "time.busy=312ms",
    "time.busy=612ms",
  )}`;
  const agg = aggregateSpans(parseSpawnSpans(twoSpawns));
  assert.equal(agg["terminal_spawn.manager_create"].count, 2);
  assert.equal(agg["terminal_spawn.manager_create"].min, 317);
  assert.equal(agg["terminal_spawn.manager_create"].max, 617);
  assert.equal(agg["terminal_spawn.manager_create"].p95, 617);
});

// ---------------------------------------------------------------------------
// Run flattening + diffing
// ---------------------------------------------------------------------------

/** Minimal run document in the shape `perf-harness.mjs` writes. */
function makeRun(label, { createP50, createP95, frameP95, longTasks, fps }) {
  return {
    label,
    startedAt: "2026-08-05T00:00:00.000Z",
    runner: { buildId: `build-${label}`, gitSha: "abc123", apiUrl: "http://localhost:9877" },
    generator: "loop",
    soakMs: 15000,
    levels: [
      {
        sessions: 10,
        created: 10,
        terminalCreateMs: { p50: createP50, p95: createP95, max: createP95 * 1.2 },
        frontend: {
          frameTimeMs: { p50: frameP95 / 2, p95: frameP95 },
          longTasks: { count: longTasks, perSec: longTasks / 15, totalMs: longTasks * 70, blockingPct: 4 },
          fps,
        },
        chunks: { bytesPerSec: 120000, webviewEventsPerSec: 300, paintsPerWebviewEvent: 0.2 },
        spawnSpans: {
          "terminal_spawn.record_open": { count: 10, p50: createP50 * 0.2, p95: createP95 * 0.2 },
        },
      },
    ],
  };
}

test("flattenRun produces stable, level-scoped metric keys", () => {
  const flat = flattenRun(
    makeRun("a", { createP50: 200, createP95: 400, frameP95: 40, longTasks: 30, fps: 45 }),
  );
  assert.equal(flat["S10/terminal_create_ms.p50"], 200);
  assert.equal(flat["S10/terminal_create_ms.p95"], 400);
  assert.equal(flat["S10/frame_time_ms.p95"], 40);
  assert.equal(flat["S10/long_tasks.count"], 30);
  assert.equal(flat["S10/fps.mean"], 45);
  assert.equal(flat["S10/chunks.bytes_per_sec"], 120000);
  assert.equal(flat["S10/spawn_spans/terminal_spawn.record_open.p50"], 40);
});

test("flattenRun on garbage returns an empty map rather than throwing", () => {
  assert.deepEqual(flattenRun(null), {});
  assert.deepEqual(flattenRun({}), {});
  assert.deepEqual(flattenRun({ levels: [] }), {});
});

test("compareRuns marks latency drops better and rises WORSE", () => {
  const before = makeRun("before", {
    createP50: 400,
    createP95: 900,
    frameP95: 60,
    longTasks: 40,
    fps: 30,
  });
  const after = makeRun("after", {
    createP50: 200,
    createP95: 300,
    frameP95: 70,
    longTasks: 40,
    fps: 55,
  });
  const rows = compareRuns(before, after);
  const by = Object.fromEntries(rows.map((r) => [r.key, r]));

  assert.equal(by["S10/terminal_create_ms.p50"].verdict, "better");
  assert.equal(by["S10/terminal_create_ms.p50"].delta, -200);
  assert.equal(by["S10/terminal_create_ms.p50"].pct, -50);

  assert.equal(by["S10/terminal_create_ms.p95"].verdict, "better");
  // Frame time went UP — that is a regression and must be flagged.
  assert.equal(by["S10/frame_time_ms.p95"].verdict, "WORSE");
  // Long tasks unchanged.
  assert.equal(by["S10/long_tasks.count"].verdict, "same");
  // FPS is higher-is-better, so a rise must read "better", not "WORSE".
  assert.equal(by["S10/fps.mean"].verdict, "better");
});

test("compareRuns keeps metrics present on only one side", () => {
  const before = makeRun("before", {
    createP50: 400,
    createP95: 900,
    frameP95: 60,
    longTasks: 40,
    fps: 30,
  });
  const after = JSON.parse(JSON.stringify(before));
  after.label = "after";
  after.levels.push({
    sessions: 40,
    created: 30,
    terminalCreateMs: { p50: 900, p95: 1800, max: 2000 },
    frontend: null,
    chunks: {},
    spawnSpans: {},
  });
  delete after.levels[0].frontend;

  const rows = compareRuns(before, after);
  const by = Object.fromEntries(rows.map((r) => [r.key, r]));
  assert.equal(by["S40/terminal_create_ms.p50"].verdict, "new");
  assert.equal(by["S40/terminal_create_ms.p50"].before, null);
  assert.equal(by["S10/frame_time_ms.p95"].verdict, "dropped");
  assert.equal(by["S10/frame_time_ms.p95"].after, null);
});

test("compareRuns honours the min-percent reporting threshold", () => {
  const before = makeRun("b", { createP50: 100, createP95: 200, frameP95: 20, longTasks: 10, fps: 60 });
  const after = makeRun("a", { createP50: 103, createP95: 200, frameP95: 20, longTasks: 10, fps: 60 });
  const loose = compareRuns(before, after, { minPctChange: 5 });
  const tight = compareRuns(before, after, { minPctChange: 1 });
  assert.equal(loose.find((r) => r.key === "S10/terminal_create_ms.p50").verdict, "same");
  assert.equal(tight.find((r) => r.key === "S10/terminal_create_ms.p50").verdict, "WORSE");
});

test("compareRuns survives a zero baseline without producing Infinity", () => {
  const before = makeRun("b", { createP50: 0, createP95: 200, frameP95: 20, longTasks: 10, fps: 60 });
  const after = makeRun("a", { createP50: 5, createP95: 200, frameP95: 20, longTasks: 10, fps: 60 });
  const row = compareRuns(before, after).find((r) => r.key === "S10/terminal_create_ms.p50");
  assert.equal(row.pct, null);
  assert.equal(row.verdict, "changed");
  assert.equal(row.delta, 5);
});

test("compareMetricKeys orders session levels numerically, not lexically", () => {
  const keys = ["S40/a", "S10/a", "S100/a", "S20/a", "zzz"].sort(compareMetricKeys);
  assert.deepEqual(keys, ["S10/a", "S20/a", "S40/a", "S100/a", "zzz"]);
});

test("metricMeta flags the higher-is-better metrics", () => {
  assert.equal(metricMeta("S10/terminal_create_ms.p95").lowerIsBetter, true);
  assert.equal(metricMeta("S10/fps.mean").lowerIsBetter, false);
  assert.equal(metricMeta("S10/chunks.bytes_per_sec").lowerIsBetter, false);
  assert.equal(metricMeta("S10/spawn_spans/terminal_spawn.record_open.p50").unit, "ms");
  assert.equal(metricMeta("something/unknown.metric").lowerIsBetter, true);
});

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

test("fmtNumber renders an em dash for missing values", () => {
  assert.equal(fmtNumber(null), "—");
  assert.equal(fmtNumber(undefined), "—");
  assert.equal(fmtNumber(NaN), "—");
  assert.equal(fmtNumber(Infinity), "—");
  assert.equal(fmtNumber(1.234), "1.2");
  assert.equal(fmtNumber(12.34), "12.3");
  assert.equal(fmtNumber(1234), "1234");
});

test("renderMarkdownTable emits an aligned, well-formed table", () => {
  const md = renderMarkdownTable(["a", "bb"], [["1", "2"], ["333", "4"]]);
  const lines = md.split("\n");
  assert.equal(lines.length, 4);
  assert.ok(lines[0].startsWith("| a"));
  assert.ok(/^\|-+\|-+\|$/.test(lines[1]));
  assert.ok(lines[2].includes("| 1"));
});

test("renderSingleRun says the spawn breakdown is MISSING, never zero", () => {
  const run = makeRun("x", { createP50: 100, createP95: 200, frameP95: 20, longTasks: 5, fps: 60 });
  run.levels[0].spawnSpans = {};
  const md = renderSingleRun(run);
  assert.ok(md.includes("no `terminal_spawn.*` spans found"));
  assert.ok(md.includes("RUST_LOG"));
  assert.ok(!md.includes("Spawn-segment breakdown (`terminal_spawn.*` spans)"));
});

test("renderSingleRun includes the spawn table when spans exist", () => {
  const run = makeRun("x", { createP50: 100, createP95: 200, frameP95: 20, longTasks: 5, fps: 60 });
  const md = renderSingleRun(run);
  assert.ok(md.includes("Spawn-segment breakdown"));
  assert.ok(md.includes("terminal_spawn.record_open"));
});

test("renderComparison names every regression in its summary line", () => {
  const before = makeRun("before", {
    createP50: 400,
    createP95: 900,
    frameP95: 30,
    longTasks: 10,
    fps: 60,
  });
  const after = makeRun("after", {
    createP50: 800,
    createP95: 1800,
    frameP95: 30,
    longTasks: 10,
    fps: 60,
  });
  const md = renderComparison(before, after);
  assert.ok(md.includes("regressed"));
  assert.ok(md.includes("S10/terminal_create_ms.p95"));
  assert.ok(md.includes("WORSE"));
});

test("renderComparison reports a clean run explicitly", () => {
  const run = makeRun("x", { createP50: 100, createP95: 200, frameP95: 20, longTasks: 5, fps: 60 });
  const md = renderComparison(run, JSON.parse(JSON.stringify(run)));
  assert.ok(md.includes("No regressions"));
});

// ---------------------------------------------------------------------------
// Probe sample folding
// ---------------------------------------------------------------------------

test("foldFrontendSample derives fps, long-task rate and blocking percentage", () => {
  const folded = foldFrontendSample({
    elapsedMs: 10000,
    frameGapsMs: Array.from({ length: 300 }, () => 33.3),
    longTasks: [{ duration: 100 }, { duration: 200 }, { duration: 700 }],
    webviewOutputEvents: 1500,
    webviewHooked: true,
    errors: [],
  });
  assert.equal(folded.frameCount, 300);
  assert.equal(folded.fps, 30);
  assert.equal(folded.longTasks.count, 3);
  assert.equal(folded.longTasks.totalMs, 1000);
  assert.equal(folded.longTasks.maxMs, 700);
  assert.ok(Math.abs(folded.longTasks.perSec - 0.3) < 1e-9);
  assert.equal(folded.longTasks.blockingPct, 10);
  assert.equal(folded.webviewOutputEvents, 1500);
});

test("foldFrontendSample keeps an unhooked webview distinguishable from zero", () => {
  const folded = foldFrontendSample({
    elapsedMs: 5000,
    frameGapsMs: [16, 17],
    longTasks: [],
    webviewOutputEvents: null,
    webviewHooked: false,
    errors: ["tauri-event-api-unavailable"],
  });
  assert.equal(folded.webviewOutputEvents, null);
  assert.equal(folded.webviewHooked, false);
  assert.equal(folded.longTasks.count, 0);
  assert.deepEqual(folded.probeErrors, ["tauri-event-api-unavailable"]);
});

test("foldFrontendSample returns null for a missing sample", () => {
  assert.equal(foldFrontendSample(null), null);
  assert.equal(foldFrontendSample(undefined), null);
});

test("foldChunkRates converts a byte delta into a rate and a paint ratio", () => {
  const frontend = foldFrontendSample({
    elapsedMs: 10000,
    frameGapsMs: Array.from({ length: 600 }, () => 16.7),
    longTasks: [],
    webviewOutputEvents: 3000,
    webviewHooked: true,
    errors: [],
  });
  const chunks = foldChunkRates({ bytes: 1_000_000, elapsedMs: 10000 }, frontend);
  assert.equal(chunks.bytesPerSec, 100000);
  assert.equal(chunks.webviewEventsPerSec, 300);
  assert.equal(chunks.paintsPerWebviewEvent, 0.2);
});

test("foldChunkRates nulls the derived rates instead of dividing by zero", () => {
  const chunks = foldChunkRates({ bytes: 100, elapsedMs: 0 }, null);
  assert.equal(chunks.bytesPerSec, null);
  assert.equal(chunks.webviewEventsPerSec, null);
  assert.equal(chunks.paintsPerWebviewEvent, null);
});

// ---------------------------------------------------------------------------
// Transport counters (plan 2026-09-20-terminal-output-transport-is-unmeasured-
// encoded-broadcast, Phase 1)
// ---------------------------------------------------------------------------

/** A `GET /terminals/transport-stats` body in the route's exact shape. */
function transportDoc(overrides = {}) {
  return {
    since_reset_ms: 10000,
    reader: { chunks: 1000, bytes: 20_000_000 },
    encode: {
      count: 1000,
      frame_count: 500,
      ns: 40_000_000,
      bytes_in: 20_000_000,
      bytes_out: 26_700_000,
      waste: 250,
    },
    legs: {
      sse: { chunks: 0, bytes: 0 },
      ws: { chunks: 1000, bytes: 26_800_000 },
      webview: { emits: 800, emit_ns: 40_000_000, bytes: 21_000_000 },
    },
    pipe: { chunks: 1000, decode_ns: 10_000_000, redact_ns: 5_000_000, reencode_ns: 15_000_000 },
    ring_replay: { calls: 4, bytes: 4_194_304 },
    frame_size_hist: {
      bounds: [1024, 4096, 16384, 49152, 65536, 262144],
      counts: [100, 100, 300, 300, 100, 80, 20],
    },
    ...overrides,
  };
}

/** A `window.__qontinuiTransportStats` snapshot as the probe returns it. */
function frontendSnapshot() {
  return {
    enabled: true,
    sinceResetMs: 10000,
    decodeNs: { pane: 30_000_000, tap: 50_000_000 },
    decodeCalls: { pane: 300, tap: 800 },
    decodeBytes: { pane: 6_000_000, tap: 16_000_000 },
    writeToRenderNs: 3_000_000_000,
    writeToRenderCount: 300,
    writeToRenderBytes: 6_000_000,
    eventsDelivered: 800,
    eventsForeign: 200,
    ringReplay: { fetches: 3, bytesFetched: 3_145_728, bytesWritten: 12_288 },
    rawIpc: null,
  };
}

test("buildGeneratorCommand runs the node generator at the terminal's geometry", () => {
  assert.deepEqual(GENERATOR_PRESETS, ["tui-repaint", "spinner"]);
  assert.equal(
    buildGeneratorCommand({ preset: "tui-repaint", scriptPath: "/r/scripts/g.mjs", cols: 120, rows: 30 }),
    'node "/r/scripts/g.mjs" --fps 30 --cols 120 --rows 30',
  );
  assert.equal(
    buildGeneratorCommand({
      preset: "spinner",
      scriptPath: "C:\\Users\\a b\\g.mjs",
      cols: 80,
      rows: 24,
      fps: 10,
    }),
    'node "C:\\Users\\a b\\g.mjs" --fps 10 --cols 80 --rows 24 --spinner',
  );
});

test("buildGeneratorCommand refuses an unknown preset and an unquotable path", () => {
  assert.throws(
    () => buildGeneratorCommand({ preset: "date", scriptPath: "/g.mjs", cols: 1, rows: 1 }),
    /unknown --generator-preset/,
  );
  // Each is live inside double quotes in some shell: `"` everywhere, `$` in
  // PowerShell/POSIX, backtick in PowerShell/POSIX, `!` in bash, `%` in cmd.
  for (const bad of ['/a"b.mjs', "/a$HOME.mjs", "/a`b.mjs", "/a!b.mjs", "C:\\a%TEMP%.mjs"]) {
    assert.throws(
      () => buildGeneratorCommand({ preset: "spinner", scriptPath: bad, cols: 1, rows: 1 }),
      /quoted/,
      bad,
    );
  }
  // Spaces, parentheses and a single quote are literal inside double quotes.
  assert.equal(
    buildGeneratorCommand({
      preset: "tui-repaint",
      scriptPath: "/home/o'neil/x (1)/g.mjs",
      cols: 1,
      rows: 1,
    }),
    `node "/home/o'neil/x (1)/g.mjs" --fps 30 --cols 1 --rows 1`,
  );
});

test("unwrapTransportStats accepts a document without encode.frame_count, and folds its waste % to UNKNOWN", () => {
  const doc = transportDoc();
  delete doc.encode.frame_count;
  assert.equal(unwrapTransportStats(doc), doc);
  const d = foldTransport({ backend: doc }).derived;
  assert.equal(d.encodeFrameCount, null);
  assert.equal(d.encodeWastePct, null, "never silently falls back to encode.count");
  const bad = transportDoc();
  bad.encode.frame_count = "500";
  assert.equal(unwrapTransportStats(bad), null);
});

test("unwrapTransportStats accepts the bare document and the ApiResponse envelope", () => {
  const doc = transportDoc();
  assert.equal(unwrapTransportStats(doc), doc);
  assert.equal(unwrapTransportStats({ success: true, data: doc }), doc);
});

test("unwrapTransportStats treats a failed or partial answer as UNKNOWN (null)", () => {
  assert.equal(unwrapTransportStats({ success: false, data: transportDoc() }), null);
  assert.equal(unwrapTransportStats(null), null);
  assert.equal(unwrapTransportStats({ since_reset_ms: 1 }), null);
  const badHist = transportDoc({ frame_size_hist: { bounds: [], counts: [1, 2] } });
  assert.equal(unwrapTransportStats(badHist), null);
});

test("framesAbove sums the buckets strictly over a bound", () => {
  const hist = transportDoc().frame_size_hist;
  assert.equal(framesAbove(hist, 49152), 200);
  assert.equal(framesAbove(hist, 262144), 20);
  assert.equal(framesAbove(hist, 50000), null, "a non-bound threshold would straddle a bucket");
  assert.equal(framesAbove({ counts: [1] }, 49152), null);
});

test("normalizeFrontendTransport is null for a webview without the object", () => {
  assert.equal(normalizeFrontendTransport(null), null);
  assert.equal(normalizeFrontendTransport({}), null);
  const n = normalizeFrontendTransport(frontendSnapshot());
  assert.equal(n.decodeNs.tap, 50_000_000);
  assert.equal(n.rawIpc, null);
  assert.equal(n.timingEnabled, true);
});

test("foldIpcFallback distinguishes a flip, a clean window and UNKNOWN", () => {
  assert.equal(foldIpcFallback({ fallbackWarned: true }).postMessageFallback, true);
  assert.equal(
    foldIpcFallback({ postMessageHooked: true, postMessageCalls: 3 }).postMessageFallback,
    true,
  );
  assert.equal(
    foldIpcFallback({ postMessageHooked: true, postMessageCalls: 0 }).postMessageFallback,
    false,
  );
  assert.equal(foldIpcFallback({ postMessageHooked: false }).postMessageFallback, null);
  assert.equal(foldIpcFallback(null).postMessageFallback, null);
});

test("foldTransport derives the kill-criteria ratios", () => {
  const t = foldTransport({
    backend: transportDoc(),
    frontend: normalizeFrontendTransport(frontendSnapshot()),
    ipc: { postMessageHooked: true, postMessageCalls: 0 },
    altScreen: { sampled: 4, onAltScreen: 1 },
  });
  const d = t.derived;
  assert.equal(d.readerBytesPerSec, 2_000_000);
  assert.equal(d.encodeNsPerChunk, 40_000);
  // waste / frame_count (500), NOT waste / count (1000): flush encodes are never waste.
  assert.equal(d.encodeFrameCount, 500);
  assert.equal(d.encodeWastePct, 50);
  assert.equal(d.webviewEmitNsPerEmit, 50_000);
  assert.equal(d.pipeNsPerChunk, 30_000);
  assert.equal(d.framesOverRelayHazard, 200);
  assert.equal(d.framesOverRelayHazardPct, 20);
  assert.equal(d.paneDecodeNsPerCall, 100_000);
  assert.equal(d.writeToRenderMsMean, 10);
  assert.equal(d.eventsForeignPct, 25);
  assert.equal(d.ringReplayFetchToWriteRatio, 256);
  // (40 + 40 + 30 + 50) ms of transport against 3000 ms of write->render.
  assert.ok(Math.abs(d.k1TransportSharePct - (160 / 3160) * 100) < 1e-9);
  assert.equal(t.ipc.postMessageFallback, false);
  assert.deepEqual(t.altScreen, { sampled: 4, onAltScreen: 1 });
});

test("foldTransport keeps a missing half UNKNOWN with its reason", () => {
  const t = foldTransport({
    backend: null,
    backendError: "GET /terminals/transport-stats: 404",
    frontend: normalizeFrontendTransport(frontendSnapshot()),
  });
  assert.equal(t.backend, null);
  assert.equal(t.backendError, "GET /terminals/transport-stats: 404");
  assert.equal(t.derived.encodeNsPerChunk, undefined);
  assert.equal(t.derived.k1TransportSharePct, undefined);
  assert.equal(t.derived.eventsForeignPct, 25);
  assert.equal(t.ipc.postMessageFallback, null);
});

test("flattenRun and compareRuns carry the transport metrics", () => {
  const withTransport = (label, encodeNs) => {
    const run = makeRun(label, { createP50: 100, createP95: 200, frameP95: 20, longTasks: 5, fps: 60 });
    run.levels[0].transport = foldTransport({
      backend: transportDoc({
        encode: { count: 1000, frame_count: 800, ns: encodeNs, bytes_in: 1, bytes_out: 1, waste: 0 },
      }),
      frontend: normalizeFrontendTransport(frontendSnapshot()),
    });
    return run;
  };
  const before = withTransport("before", 40_000_000);
  const after = withTransport("after", 10_000_000);
  assert.equal(flattenRun(before)["S10/transport.encode_ns_per_chunk"], 40_000);
  assert.equal(flattenRun(before)["S10/transport.encode_frame_count"], 800);
  assert.equal(flattenRun(before)["S10/transport.encode_waste_pct"], 0);
  const row = compareRuns(before, after).find(
    (r) => r.key === "S10/transport.encode_ns_per_chunk",
  );
  assert.equal(row.unit, "ns");
  assert.equal(row.verdict, "better");
  assert.equal(metricMeta("S10/transport.reader_bytes_per_sec").lowerIsBetter, false);
});

test("renderTransportSection tabulates levels and names an UNAVAILABLE half", () => {
  const run = makeRun("x", { createP50: 100, createP95: 200, frameP95: 20, longTasks: 5, fps: 60 });
  assert.equal(renderTransportSection(run), "", "no transport block, no section");
  run.levels[0].transport = foldTransport({
    backend: null,
    backendError: "GET /terminals/transport-stats: 404",
    frontend: normalizeFrontendTransport(frontendSnapshot()),
    ipc: { fallbackWarned: true },
    altScreen: { sampled: 3, onAltScreen: 0 },
  });
  const md = renderTransportSection(run);
  assert.ok(md.includes("K1 share %"));
  assert.ok(md.includes("waste % (of frame)"));
  assert.ok(md.includes("runner counters UNAVAILABLE — GET /terminals/transport-stats: 404"));
  assert.ok(md.includes("| YES"));
  assert.ok(md.includes("0/3"));
  assert.ok(renderSingleRun(run).includes("#### Transport"));
});
