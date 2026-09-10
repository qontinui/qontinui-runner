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

// ============================================================================
// What `maxLines` COUNTS — the both-paths agreement (iteration 26)
// ============================================================================

/**
 * Iteration 25 made `maxLines` REJECT identically on both paths. This block
 * covers the half it did not: what an ACCEPTED `maxLines` MEANS.
 *
 * Measured live before the fix, two panes holding identical content, identical
 * request, HTTP 200 on both:
 *
 *   | `maxLines`    | MOUNTED        | PROXY             |
 *   |---------------|----------------|-------------------|
 *   | 1             | `""` (0 chars) | 21 chars, 1 line  |
 *   | 2             | `""`           | 38 chars, 2 lines |
 *   | 3             | `""`           | 98 chars, 3 lines |
 *   | default (500) | 81 chars, 3 ln | 98 chars, 3 lines |
 *
 * The mounted path counted `getBufferLength()` RENDERED ROWS — blank viewport
 * padding included — and then dropped the blanks with `if (line)`, so a 3-line
 * pane in a 34-row viewport spent 31 of every 34 lines of budget on nothing and
 * answered `""` for every bound below 34. The proxy counted real ring lines.
 * Whether a pane is mounted is a property of the VIEWPORT, so the same script
 * got content or `""` depending on where the flow grid had scrolled.
 */
import {
  scrollbackTail,
  scrollbackTailOfLines,
  takeLastContentLines,
} from "./terminalScrollbackParams";

/** The mounted path's backing store: content rows, then blank viewport padding. */
function mountedPane(content: readonly string[], rows: number): (i: number) => string {
  const buffer = [...content, ...Array.from({ length: rows - content.length }, () => "")];
  return (i) => buffer[i] ?? "";
}

/** How the MOUNTED handler reads its buffer. */
function mountedRead(content: readonly string[], rows: number, limit: number): string {
  return scrollbackTail(rows, mountedPane(content, rows), limit);
}

/** How the PROXY handler reads the decoded PTY ring. */
function proxyRead(content: readonly string[], limit: number): string {
  return scrollbackTailOfLines([...content], limit);
}

describe("maxLines means N CONTENT LINES on both paths (iter 26)", () => {
  // The sparse pane from the live reproduction: three lines of real output
  // sitting in a 34-row viewport. 34 is the boundary that was measured — the
  // first `maxLines` at which the mounted path produced anything at all.
  const CONTENT = ["> npm run dev", "ready in 412 ms", "PS C:\\qontinui-root> "];
  const ROWS = 34;

  it.each([[1], [2], [3], [500]])(
    "mounted and proxy return the SAME thing at maxLines %i",
    (limit) => {
      expect(mountedRead(CONTENT, ROWS, limit)).toBe(proxyRead(CONTENT, limit));
    },
  );

  it("maxLines: 1 is the last CONTENT line on both — not an empty string", () => {
    // The worst case in the finding, and the most natural input there is.
    // Before the fix the mounted path answered `""` here, which an automation
    // reads as "the pane is idle".
    const last = "PS C:\\qontinui-root> ";
    expect(mountedRead(CONTENT, ROWS, 1)).toBe(last);
    expect(proxyRead(CONTENT, 1)).toBe(last);
  });

  it("blank viewport padding never consumes the bound", () => {
    // 1 line of content in a 200-row viewport: the old arithmetic needed
    // maxLines >= 200 to see it.
    expect(mountedRead(["only-line"], 200, 1)).toBe("only-line");
    expect(mountedRead(["only-line"], 200, 3)).toBe("only-line");
  });

  it("asking for more lines than exist yields everything, not padding", () => {
    expect(mountedRead(CONTENT, ROWS, 999)).toBe(CONTENT.join("\n"));
    expect(proxyRead(CONTENT, 999)).toBe(CONTENT.join("\n"));
  });

  it("blank lines INSIDE the content are skipped identically on both paths", () => {
    const sparse = ["first", "", "   ", "second", "", "third"];
    for (const limit of [1, 2, 3, 500]) {
      expect(mountedRead(sparse, 40, limit)).toBe(proxyRead(sparse, limit));
    }
    expect(proxyRead(sparse, 2)).toBe("second\nthird");
  });

  it("an empty buffer is an empty string on both paths", () => {
    expect(mountedRead([], 34, 1)).toBe("");
    expect(proxyRead([], 1)).toBe("");
  });
});

describe("takeLastContentLines — the one implementation both handlers call", () => {
  it("reads from the END and stops at the bound", () => {
    // Cost is `limit` accessor calls, not `totalLines`: a 10 000-row buffer
    // must not be walked to answer `maxLines: 2`.
    const seen: number[] = [];
    const out = takeLastContentLines(
      10000,
      (i) => {
        seen.push(i);
        return `row-${i}`;
      },
      2,
    );
    expect(out).toEqual(["row-9998", "row-9999"]);
    expect(seen).toEqual([9999, 9998]);
  });

  it("treats null/undefined rows as blank rather than crashing", () => {
    // `getBufferLine(i)` can answer undefined for a row xterm has not
    // materialized.
    const rows: (string | undefined)[] = ["a", undefined, "b"];
    expect(takeLastContentLines(3, (i) => rows[i], 3)).toEqual(["a", "b"]);
  });

  it("preserves each returned line VERBATIM, trailing spaces included", () => {
    // `trim()` decides whether a line COUNTS; it never edits what is returned.
    // A shell prompt's trailing space is content the caller asked for.
    expect(takeLastContentLines(1, () => "PS C:\\> ", 1)).toEqual(["PS C:\\> "]);
  });
});
