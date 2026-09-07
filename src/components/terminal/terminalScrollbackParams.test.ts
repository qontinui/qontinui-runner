/**
 * Tests for the `getScrollback` `maxLines` guard.
 *
 * THE DEFECT these cover (manual-test-loop iteration 25): `maxLines` was cast,
 * not checked, on BOTH terminal paths — and because the two paths slice their
 * buffers by different expressions, the same malformed value produced OPPOSITE
 * answers. Measured 3/3 reps with 40 lines in each pane, both HTTP 200
 * `success: true`: `{"a":1}` and `"abc"` returned the WHOLE buffer (42 lines)
 * on the proxy path and an EMPTY STRING on the mounted one. An automation
 * reading `""` concludes the pane is idle.
 *
 * The unit under test is the shared validator both handlers now call, so a
 * divergence cannot be reintroduced on one side alone; the source-scan guards
 * in `TerminalBridgeProxies.test.tsx` pin that both sides still call it.
 */

import { describe, expect, it } from "vitest";

import {
  DEFAULT_SCROLLBACK_MAX_LINES,
  SCROLLBACK_MAX_LINES_INVALID,
  requireMaxLines,
} from "./terminalScrollbackParams";

describe("requireMaxLines — what it accepts", () => {
  it("defaults when the parameter is absent — unchanged for every existing caller", () => {
    expect(requireMaxLines(undefined)).toBe(DEFAULT_SCROLLBACK_MAX_LINES);
    expect(DEFAULT_SCROLLBACK_MAX_LINES).toBe(500);
  });

  it("accepts a positive integer", () => {
    expect(requireMaxLines(1)).toBe(1);
    expect(requireMaxLines(40)).toBe(40);
    expect(requireMaxLines(100000)).toBe(100000);
  });
});

describe("requireMaxLines — the NaN-poisoning shapes that diverged", () => {
  it.each([
    ["an object", { a: 1 }],
    ["a string", "abc"],
    ["a numeric string", "3"],
    ["an array", [1]],
    ["a boolean", true],
    ["null", null],
    ["NaN", Number.NaN],
    ["Infinity", Number.POSITIVE_INFINITY],
    ["a fraction", 2.5],
  ])("rejects %s with the typed code", (_label, value) => {
    expect(() => requireMaxLines(value)).toThrow(SCROLLBACK_MAX_LINES_INVALID);
  });

  it('rejects `"3"` rather than number-coercing it to 3', () => {
    // Silent coercion is how `[1]` and `true` became 1 — a caller who sent the
    // wrong type got a plausible-looking answer and never learned.
    expect(() => requireMaxLines("3")).toThrow(SCROLLBACK_MAX_LINES_INVALID);
  });
});

describe("requireMaxLines — the non-positive domain", () => {
  it("rejects a negative bound", () => {
    expect(() => requireMaxLines(-5)).toThrow(/at least 1/);
  });

  // THE STATED CALL. `0` is arguably "no lines please", and serving it could
  // only ever answer "" — indistinguishable from an idle pane, a dead pane and
  // a failed read. Both paths agree because both call this one function.
  it("rejects 0", () => {
    expect(() => requireMaxLines(0)).toThrow(SCROLLBACK_MAX_LINES_INVALID);
    expect(() => requireMaxLines(0)).toThrow(/at least 1/);
  });

  it("rejects -0", () => {
    expect(() => requireMaxLines(-0)).toThrow(SCROLLBACK_MAX_LINES_INVALID);
  });
});

describe("requireMaxLines — the error it throws", () => {
  it("carries the machine-readable .code the SDK hoists onto the response", () => {
    try {
      requireMaxLines("abc");
      expect.unreachable("should have thrown");
    } catch (err) {
      expect((err as { code?: string }).code).toBe(SCROLLBACK_MAX_LINES_INVALID);
    }
  });

  it("describes the rejected value by TYPE, never by content", () => {
    try {
      requireMaxLines("s3cr3t-token");
      expect.unreachable("should have thrown");
    } catch (err) {
      expect((err as Error).message).not.toContain("s3cr3t-token");
      expect((err as Error).message).toContain("a string");
    }
  });
});
