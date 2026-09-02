/**
 * The shared scrollback reader, measured.
 *
 * `TerminalInstance` calls this from TWO places: the imperative
 * `TerminalInstanceHandle.getScrollback` and the `getScrollback` UI Bridge
 * custom action. The custom-action caller was verified on-page; the imperative
 * one was not, and the argument offered for it was "they share a function, so
 * the tests carry it".
 *
 * That is an argument from shape — the same one PR #1301's docstring made about
 * three siblings it had not checked, which is the defect this whole change
 * exists to close. These tests make the shared function a measurement, so both
 * callers rest on evidence rather than on inheritance.
 */

import { describe, it, expect } from "vitest";
import { readFileSync } from "fs";
import {
  DEFAULT_SCROLLBACK_LINES,
  readBufferScrollback,
  type ScrollbackSource,
} from "./terminalScrollback";

/** A buffer of `rows`, exactly as a backend would hand them back. */
function buffer(rows: Array<string | null>): ScrollbackSource {
  return {
    getBufferLength: () => rows.length,
    getBufferLine: (i: number) => rows[i] ?? null,
  };
}

/** 40 written lines plus the empty current row a live pane always carries. */
const LIVE_PANE = buffer([
  ...Array.from({ length: 40 }, (_, i) => `harness-line-${String(i + 1).padStart(3, "0")}`),
  "",
]);

describe("readBufferScrollback", () => {
  it("returns the whole buffer when the window is larger than it", () => {
    const out = readBufferScrollback(LIVE_PANE).split("\n");
    expect(out).toHaveLength(40);
    expect(out[0]).toBe("harness-line-001");
    expect(out[39]).toBe("harness-line-040");
    expect(DEFAULT_SCROLLBACK_LINES).toBe(500);
  });

  it("windows over BUFFER ROWS, and drops the empty current row inside the window", () => {
    // The exact figures measured on the page against a live pane: 12 rows of
    // window over 40 written lines + 1 empty current row yields ELEVEN lines.
    // This is the shipped contract, not an off-by-one — pinned so a later
    // "fix" to make the counts match has to argue with a test.
    const twelve = readBufferScrollback(LIVE_PANE, 12).split("\n");
    expect(twelve).toHaveLength(11);
    expect(twelve[0]).toBe("harness-line-030");
    expect(twelve[10]).toBe("harness-line-040");

    const three = readBufferScrollback(LIVE_PANE, 3).split("\n");
    expect(three).toHaveLength(2);
    expect(three).toEqual(["harness-line-039", "harness-line-040"]);
  });

  it("counts every row in the window, including ones it will not emit", () => {
    // Interleaved blanks: the window is 4 ROWS, two of which are empty, so two
    // lines come back. A reader that skipped blanks while advancing the window
    // would return four.
    const patchy = buffer(["a", "", "b", "", "c", "", "d"]);
    expect(readBufferScrollback(patchy, 4)).toBe("c\nd");
  });

  it("drops null rows as well as empty ones", () => {
    expect(readBufferScrollback(buffer(["a", null, "b"]))).toBe("a\nb");
  });

  it("answers '' for an absent backend rather than throwing", () => {
    // Both call sites pass `backendRef.current ?? null`, and a pane that has
    // unmounted or not yet attached has none. Throwing there would turn a
    // benign read into a failed automation step.
    expect(readBufferScrollback(null)).toBe("");
    expect(readBufferScrollback(undefined)).toBe("");
    expect(readBufferScrollback(null, 10)).toBe("");
  });

  it("handles an empty buffer and a zero-length window without a negative slice", () => {
    expect(readBufferScrollback(buffer([]))).toBe("");
    expect(readBufferScrollback(buffer([]), 10)).toBe("");
    // `Math.max(0, …)` is what keeps this from walking off the front.
    expect(readBufferScrollback(LIVE_PANE, 0)).toBe("");
    expect(readBufferScrollback(LIVE_PANE, -5)).toBe("");
  });

  it("is the SAME function both TerminalInstance callers reach", () => {
    // The duplication guard. Two byte-identical copies 1200 lines apart is how
    // this file already lost a fix once; a third copy must not appear.
    const src = readFileSync(`${__dirname}/TerminalInstance.tsx`, "utf8");
    expect(src).toContain('from "./terminalScrollback"');
    // Both call sites, and no local re-implementation.
    expect(src.match(/readBufferScrollback\(/g) ?? []).toHaveLength(2);
    expect(src).not.toContain("getBufferLength()");
  });
});
