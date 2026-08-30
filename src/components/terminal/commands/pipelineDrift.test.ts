/**
 * The one modelled thing in this harness, pinned to its original.
 *
 * `pipeline.testkit.ts::bind` reproduces the ~20 lines of ORDERING that live
 * inside `CommandBar.tsx` — the `matches` memo and the `execute` callback.
 * Everything else in the harness runs the real modules, but that glue cannot
 * be imported: it is a `useMemo` and a `useCallback` inside a React component,
 * and this repo has no jsdom or @testing-library to render one (adding either
 * to run a single callback would cost every build, for a component whose
 * only untested part is call ORDER).
 *
 * A model that drifts from its original is worse than no model: it keeps
 * passing while testing the wrong shape. That is the exact failure mode this
 * directory exists to eliminate, so the model is pinned HERE, against the
 * component's source text. If someone reorders `execute` — moves
 * `applyDeclaredFlags` after the handler, drops the `origin` argument, stops
 * guarding `unboundTokens` on the preset route — this spec goes red and names
 * what moved.
 *
 * It is a coarse pin, deliberately. It asserts the ORDER and the ARGUMENTS
 * that the harness depends on, not the component's prose.
 */

import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

const HERE = dirname(fileURLToPath(import.meta.url));
const BAR = readFileSync(resolve(HERE, "..", "CommandBar.tsx"), "utf8");

/** Assert `needles` appear in `text` in this order, naming the first miss. */
function inOrder(text: string, needles: readonly string[], what: string): void {
  let cursor = 0;
  for (const needle of needles) {
    const at = text.indexOf(needle, cursor);
    expect(
      at,
      `${what}: expected to find \`${needle}\` after offset ${cursor}. ` +
        `CommandBar.tsx has changed shape — pipeline.testkit.ts::bind models ` +
        `this sequence and must be updated with it.`,
    ).toBeGreaterThanOrEqual(0);
    cursor = at + needle.length;
  }
}

const section = (from: string, to: string): string => {
  const a = BAR.indexOf(from);
  expect(a, `CommandBar.tsx no longer contains \`${from}\``).toBeGreaterThanOrEqual(0);
  const b = BAR.indexOf(to, a);
  expect(b, `CommandBar.tsx no longer contains \`${to}\` after \`${from}\``).toBeGreaterThan(a);
  return BAR.slice(a, b);
};

describe("pipeline model — CommandBar has not drifted from pipeline.testkit.ts", () => {
  it("still imports exactly the pipeline functions the harness models", () => {
    for (const fn of [
      "resolve",
      "matchPattern",
      "chooseTier",
      "parseArgs",
      "applyDeclaredFlags",
      "didYouMean",
      "unboundTokens",
    ]) {
      expect(BAR, `CommandBar.tsx no longer uses ${fn}`).toContain(fn);
    }
  });

  it("still selects the tier the same way", () => {
    const matches = section("const matches = useMemo(", "// ── Tier-3 debounced fire");
    inOrder(
      matches,
      [
        "const tier1 = resolve(query, recents)",
        "chooseTier(tier1, matchPattern(query), tier3Match)",
        "presetArgs: head.presetArgs",
        "tier1.filter((m) => m.action.id !== headMatch.action.id)",
      ],
      "matches memo",
    );
    // The head is index 0 of the list Enter indexes into.
    expect(matches).toContain("[\n      headMatch,");
  });

  it("still binds arguments the same way", () => {
    const exec = section("const execute = useCallback(", "// History recall is armed");
    inOrder(
      exec,
      [
        "const preset = presetArgs !== undefined",
        "applyDeclaredFlags(",
        "preset ? presetArgs : parseArgs(rawInput, action)",
        'preset ? "preset" : "parsed"',
        "didYouMean(rawInput, action, matchPattern(rawInput))",
        "if (!presetArgs) {",
        "unboundTokens(rawInput, action)",
        "await action.handler(args",
      ],
      "execute callback",
    );
  });

  it("still runs the selected match on Enter, with its preset args", () => {
    expect(BAR).toContain(
      "void execute(selectedMatch.action, query, selectedMatch.presetArgs, selectedMatch.tier)",
    );
  });

  /**
   * The status line's KIND and TEXT are what iteration 10's D1, D2, D3 and D5
   * were all defects in, and none of them was reachable from this harness
   * while the composition lived inside the component. It now lives in
   * `verdict.ts::renderCommandStatus`, which `pipeline.testkit.ts::run` calls
   * too — so the golden's `status` column is the component's own answer, not a
   * second model of it. If someone re-inlines the ternary here, the harness
   * silently goes back to characterizing nothing, and this pin is what says so.
   */
  it("still delegates the status line to the shared renderer", () => {
    const exec = section("const execute = useCallback(", "// History recall is armed");
    expect(
      exec,
      "CommandBar.tsx composes its own status line again. The ok/noop/error " +
        "split must come from verdict.ts::renderCommandStatus, or the corpus " +
        "harness is characterizing a model of it instead of the real thing.",
    ).toContain("setStatus(renderCommandStatus(action.slash, result.value))");
    expect(exec).not.toContain('report.affected === 0 ? "noop" : "ok"');
  });

  it("still refuses trailing junk BEFORE the handler runs", () => {
    const exec = section("const execute = useCallback(", "// History recall is armed");
    const guard = exec.indexOf("unboundTokens(rawInput, action)");
    const call = exec.indexOf("await action.handler(args");
    expect(guard).toBeGreaterThanOrEqual(0);
    expect(
      guard,
      "the unbound-token guard must run before the handler — otherwise `/mute " +
        "please stop` runs the bare command and renders ✓",
    ).toBeLessThan(call);
  });
});
