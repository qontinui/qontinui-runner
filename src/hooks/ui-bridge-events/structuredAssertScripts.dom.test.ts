// @vitest-environment jsdom
/**
 * Execution oracle for `POST /ui-bridge/control/assert` (plan
 * 2026-11-12-structured-assert-arms-never-pass-and-diagnose-assertions-never-reach-the-route, D2).
 *
 * The golden fixture holds, for each case, the exact script the Rust builder
 * must return (pinned by `structured_assert.rs`
 * `every_case_builds_exactly_its_golden_script`). Here each script is run
 * through the page's REAL evaluate path — `checkEvaluateBlocklist` then
 * `compileEvaluateExpression` — against a page where it must pass and a page
 * where it must fail. A script that returns no verdict (the discarded inner
 * IIFE of coord finding edfbb26a) fails both.
 *
 * jsdom has no layout, so every element reports a zero rect; visibility is
 * stubbed per element: anything inside `data-test-visible="false"` has a
 * zero rect (as `set-tab-readback.test.ts` does).
 */
import { describe, expect, it } from "vitest";

import golden from "../../lib/__golden__/structured-assert-scripts.json";
import { checkEvaluateBlocklist, compileEvaluateExpression } from "./utils";

interface GoldenCase {
  name: string;
  request: Record<string, unknown>;
  script: string;
  passHtml: string;
  failHtml: string;
}

/** The one case the page gate refuses, by design: its `expected` text names a
 * gate word. The route sends it through the direct-eval door instead (D3). */
const GATE_REFUSED = "hasText-gate-word";

function render(html: string): void {
  document.body.innerHTML = html;
  for (const el of Array.from(document.querySelectorAll<HTMLElement>("*"))) {
    const visible = el.closest('[data-test-visible="false"]') === null;
    el.getBoundingClientRect = () =>
      ({
        width: visible ? 100 : 0,
        height: visible ? 100 : 0,
      }) as DOMRect;
  }
}

/** Run one golden script the way the route's door for it runs it, and read
 * its verdict. Throws when the script yields no JSON string with a boolean
 * `passed` — that is no verdict, never a fail. */
function verdict(c: GoldenCase): boolean {
  let fn: () => unknown;
  if (c.name === GATE_REFUSED) {
    // Direct door: the runner's `eval` of the script, no gate.
    fn = new Function(`return ${c.script}`) as () => unknown;
  } else {
    fn = compileEvaluateExpression(c.script);
  }
  const value = fn();
  if (typeof value !== "string") {
    throw new Error(`${c.name}: no verdict — the script returned ${typeof value}`);
  }
  const parsed = JSON.parse(value) as { passed?: unknown };
  if (typeof parsed.passed !== "boolean") {
    throw new Error(`${c.name}: no verdict — no boolean \`passed\` in ${value}`);
  }
  return parsed.passed;
}

const cases = (golden as { cases: GoldenCase[] }).cases;

describe("structured assert golden scripts", () => {
  it("has the eleven pinned cases", () => {
    expect(cases.map((c) => c.name)).toEqual([
      "exists-css",
      "notExists-css",
      "count-css",
      "isVisible-css",
      "hasText-css",
      "hasClass-css",
      "hasAttribute-present-css",
      "hasAttribute-equals-css",
      "exists-text",
      "exists-textContains",
      "hasText-gate-word",
    ]);
  });

  for (const c of cases) {
    describe(c.name, () => {
      it("the page gate's decision matches the door the route picks", () => {
        const matched = checkEvaluateBlocklist(c.script, false);
        if (c.name === GATE_REFUSED) {
          expect(matched).not.toBeNull();
        } else {
          expect(matched).toBeNull();
        }
      });

      it("passes on the passing page", () => {
        render(c.passHtml);
        expect(verdict(c)).toBe(true);
      });

      it("fails on the failing page", () => {
        render(c.failHtml);
        expect(verdict(c)).toBe(false);
      });
    });
  }
});
