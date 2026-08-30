/**
 * The differential harness, exercised — and the committed golden snapshot.
 *
 * Four things happen here:
 *
 *   1. **Identity.** The same state against itself must produce ZERO deltas.
 *      Without this a harness that reports nothing looks healthy.
 *   2. **Self-check.** Two states that differ by KNOWN mutations must produce
 *      deltas in the expected classes. This is the guard the throwaway
 *      harnesses learned the hard way: a harness bug reads as "no changes",
 *      and "no changes" is the answer everyone wants to see.
 *   3. **Golden.** The `golden` corpus is characterized into
 *      `__golden__/pipeline-golden.txt` and compared on every build.
 *   4. **Sweep.** Every input in the `fast` corpus (and `full` under
 *      `TERMINAL_CORPUS=full`) must run the whole pipeline without throwing.
 *
 * ## Regenerating the golden
 *
 * ```sh
 * node scripts/terminal-command-corpus.mjs --update
 * ```
 *
 * The resulting `git diff` IS the review artifact. A row that changes action
 * or args is a semantic change; a row that changes only its verdict may be
 * the stubbed context in `realRegistry.testkit.ts` moving instead.
 *
 * ## Comparing two commits
 *
 * ```sh
 * git checkout <base>
 * node scripts/terminal-command-corpus.mjs --out /tmp/base.txt --tier fast
 * git checkout <head>
 * node scripts/terminal-command-corpus.mjs --baseline /tmp/base.txt --tier fast
 * ```
 *
 * Both sides run their own real modules and real handlers.
 */

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve as resolvePath } from "node:path";
import { fileURLToPath } from "node:url";

import { afterAll, beforeAll, describe, expect, it } from "vitest";

import {
  buildAiProbes,
  buildCorpus,
  buildDirectProbes,
  type CorpusTier,
} from "./corpus.testkit";
import {
  captureSnapshot,
  captureTier,
  diffSnapshots,
  formatDelta,
  formatSnapshot,
  parseSnapshot,
  summarize,
  type PipelineState,
} from "./differential.testkit";
import { loadRealRegistry } from "./realRegistry.testkit";
import type { CommandAction } from "./types";

const HERE = dirname(fileURLToPath(import.meta.url));
const GOLDEN_PATH = resolvePath(HERE, "__golden__", "pipeline-golden.txt");

const UPDATE = process.env.TERMINAL_GOLDEN_UPDATE === "1";
const SWEEP_TIER: CorpusTier = process.env.TERMINAL_CORPUS === "full" ? "full" : "fast";
const SNAPSHOT_OUT = process.env.TERMINAL_SNAPSHOT_OUT ?? "";
const SNAPSHOT_TIER = (process.env.TERMINAL_SNAPSHOT_TIER as CorpusTier) || SWEEP_TIER;
const DIFF_BASELINE = process.env.TERMINAL_DIFF_BASELINE ?? "";

let base: PipelineState;

beforeAll(async () => {
  const h = await loadRealRegistry();
  base = { name: "registry@HEAD", actions: h.actions };
});

afterAll(async () => {
  // captureSnapshot installs states into the module registry; put the real
  // one back so file ordering cannot leak a mutated registry into a sibling.
  const registry = await import("./registry");
  registry.__resetForTest();
  for (const a of base.actions) registry.register(a);
});

// ── 1. Identity ──────────────────────────────────────────────────────

describe("differential harness — identity", () => {
  it("reports no delta between a state and itself", async () => {
    const corpus = buildCorpus(base.actions, "golden");
    const a = await captureSnapshot(base, corpus);
    const b = await captureSnapshot(base, corpus);
    const deltas = diffSnapshots(a, b);
    expect(formatDelta(deltas, "self", "self")).toContain("STRICT  0 regressions");
    expect(deltas).toEqual([]);
  });
});

// ── 2. Self-check: the harness can SEE all four classes ──────────────

/**
 * Four surgical mutations, each aimed at one delta class. If any of these
 * comes back with a zero count the harness is blind in that direction, and a
 * blind harness reporting "no changes" is exactly how 14 changes went through
 * undetected in one round and 605 in another.
 */
function mutate(actions: readonly CommandAction[]): CommandAction[] {
  return actions.map((a) => {
    // (a) action-changed: without `/focus-mode`'s pattern, `/focus mode` stops
    //     rerouting and falls back to `/focus`.
    if (a.id === "terminal.toggle-focus-mode") return { ...a, patterns: [] };
    // (b) args-changed + (c) now-errors: without `/spawn`'s pattern,
    //     `/spawn 3 plain` binds `count: "3 plain"` through the catch-all
    //     instead of `count: 3`, and the count guard then refuses it.
    if (a.id === "terminal.spawn") return { ...a, patterns: [] };
    // (d) now-runs: giving `/mute` a positional field stops `unboundTokens`
    //     refusing `/mute please stop`, so it runs.
    if (a.id === "terminal.mute") return { ...a, paramSchema: { note: "string" } };
    return a;
  });
}

describe("differential harness — self-check", () => {
  it("sees a change in every one of the four classes", async () => {
    const corpus = buildCorpus(base.actions, "golden");
    const before = await captureSnapshot(base, corpus);
    const after = await captureSnapshot({ name: "mutated", actions: mutate(base.actions) }, corpus);
    const deltas = diffSnapshots(before, after);
    const s = summarize(deltas);
    const report = formatDelta(deltas, "registry@HEAD", "mutated");

    expect(s.strict["action-changed"], report).toBeGreaterThan(0);
    expect(s.strict["args-changed"], report).toBeGreaterThan(0);
    expect(s.strict["now-errors"], report).toBeGreaterThan(0);
    expect(s.strict["now-runs"], report).toBeGreaterThan(0);
  });

  /**
   * The class that matters most, isolated. An action-only diff would report
   * `/spawn 3 plain` as UNCHANGED — same action either way — while its bound
   * argument went from `3` to the string `"3 plain"`.
   */
  it("catches an args change that leaves the action alone", async () => {
    const corpus = ["/spawn 3 plain"];
    const before = await captureSnapshot(base, corpus);
    const after = await captureSnapshot({ name: "mutated", actions: mutate(base.actions) }, corpus);
    const deltas = diffSnapshots(before, after);
    expect(deltas).toHaveLength(1);
    expect(deltas[0].before?.actionId).toBe(deltas[0].after?.actionId);
    expect(deltas[0].classes).toContain("args-changed");
    expect(deltas[0].before?.args).toContain("3");
    expect(deltas[0].after?.args).toContain("3 plain");
  });

  /**
   * Both arms are reported, never one. `now-errors` / `now-runs` can move
   * because the stubbed context moved rather than because the pipeline did;
   * the structural classes cannot.
   */
  it("reports a strict and a lenient arm, and they differ", async () => {
    const corpus = buildCorpus(base.actions, "golden");
    const before = await captureSnapshot(base, corpus);
    const after = await captureSnapshot({ name: "mutated", actions: mutate(base.actions) }, corpus);
    const s = summarize(diffSnapshots(before, after));
    expect(s.strictTotal).toBeGreaterThan(s.lenientTotal);
    expect(s.lenientTotal).toBeGreaterThan(0);
  });

  /** Corpus growth is reported separately so it cannot inflate regressions. */
  it("files a corpus-derived new input as drift, not as a regression", async () => {
    const before = await captureSnapshot(base, ["/mute"]);
    const after = await captureSnapshot(base, ["/mute", "/sort"]);
    const deltas = diffSnapshots(before, after);
    expect(deltas.map((d) => d.classes)).toEqual([["input-added"]]);
    expect(summarize(deltas).strictTotal).toBe(0);
  });
});

// ── 3. The golden characterization ───────────────────────────────────

describe("differential harness — golden snapshot", () => {
  it("matches the committed characterization", async () => {
    const current = await captureTier(base, "golden");
    const text = formatSnapshot(current, "golden");

    if (UPDATE) {
      mkdirSync(dirname(GOLDEN_PATH), { recursive: true });
      writeFileSync(GOLDEN_PATH, text, "utf8");
    }
    expect(
      existsSync(GOLDEN_PATH),
      `golden snapshot missing — run: node scripts/terminal-command-corpus.mjs --update`,
    ).toBe(true);

    const committed = parseSnapshot(readFileSync(GOLDEN_PATH, "utf8"));
    const deltas = diffSnapshots(committed, current);
    expect(
      deltas.length,
      `The pipeline no longer behaves the way the golden snapshot records.\n` +
        `If the change is intended, regenerate it — the DIFF is the review artifact:\n` +
        `  node scripts/terminal-command-corpus.mjs --update\n\n` +
        formatDelta(deltas, "committed golden", "current"),
    ).toBe(0);
  });

  it("characterizes a corpus large enough to span the whole registry", async () => {
    const snap = await captureTier(base, "golden");
    const actions = new Set(
      Array.from(snap.values())
        .map((r) => r.actionId)
        .filter((id) => id !== "-"),
    );
    // Every registered action must be reachable from the corpus, or the
    // characterization has a blind spot the day someone edits that action.
    const missing = base.actions.map((a) => a.id).filter((id) => !actions.has(id));
    expect(missing, `actions no corpus input ever reaches: ${missing.join(", ")}`).toEqual([]);
  });
});

// ── 4. Sweep + the CLI hooks ─────────────────────────────────────────

describe("differential harness — corpus sweep", () => {
  it(`runs the whole ${SWEEP_TIER} corpus without throwing`, async () => {
    const corpus = buildCorpus(base.actions, SWEEP_TIER);
    const probeCount =
      buildAiProbes(base.actions, SWEEP_TIER).length +
      buildDirectProbes(base.actions, SWEEP_TIER).length;
    const started = Date.now();
    const snap = await captureTier(base, SWEEP_TIER);
    const elapsed = Date.now() - started;
    expect(snap.size).toBe(corpus.length + probeCount);
    // `threw` is the executor's last-resort arm. A handler reaching it from a
    // TYPED input is a crash the CommandBar would render as a raw message.
    //
    // Probe rows are excluded, and not as a convenience: a `«direct»` row
    // throws BY CONTRACT (`callRegistry` reports failure that way), and an
    // `«ai»` row throwing is an OBSERVATION about a hand-authored arg bag —
    // the very thing those rows exist to record. Asserting over them would
    // convert a finding into a red suite and, worse, stop the snapshot from
    // being written at all.
    const threw = Array.from(snap.entries()).filter(
      ([input, r]) => r.verdict === "threw" && !input.startsWith("«"),
    );
    expect(threw.map(([i]) => i)).toEqual([]);
    const probeThrew = Array.from(snap.entries()).filter(
      ([input, r]) => r.verdict === "threw" && input.startsWith("«ai»"),
    );
    // eslint-disable-next-line no-console
    console.log(
      `[corpus] tier=${SWEEP_TIER} inputs=${corpus.length} probes=${probeCount} ` +
        `ms=${elapsed} ai-probes-that-threw=${probeThrew.length}`,
    );

    if (SNAPSHOT_OUT) {
      const out =
        SNAPSHOT_TIER === SWEEP_TIER
          ? snap
          : await captureTier(base, SNAPSHOT_TIER);
      mkdirSync(dirname(resolvePath(SNAPSHOT_OUT)), { recursive: true });
      writeFileSync(resolvePath(SNAPSHOT_OUT), formatSnapshot(out, SNAPSHOT_TIER), "utf8");
      // eslint-disable-next-line no-console
      console.log(`[corpus] wrote ${SNAPSHOT_OUT} (tier=${SNAPSHOT_TIER})`);
    }
  }, 120_000);

  it("compares against TERMINAL_DIFF_BASELINE when one is given", async () => {
    if (!DIFF_BASELINE) return;
    const current = await captureTier(base, SNAPSHOT_TIER);
    const baseline = parseSnapshot(readFileSync(resolvePath(DIFF_BASELINE), "utf8"));
    const deltas = diffSnapshots(baseline, current);
    // eslint-disable-next-line no-console
    console.log(formatDelta(deltas, DIFF_BASELINE, "current", 200));
    if (process.env.TERMINAL_DIFF_STRICT === "1") {
      expect(summarize(deltas).strictTotal).toBe(0);
    }
  }, 120_000);
});
