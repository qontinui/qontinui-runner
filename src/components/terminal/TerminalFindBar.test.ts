/**
 * Pure-helper tests for `TerminalFindBar`.
 *
 * The runner's vitest config uses `environment: "node"` — JSX rendering
 * isn't available (same precedent as `MidSessionToast.test.tsx`), so we
 * exercise the exported pure helpers: the find-input key interpreter and
 * the VS Code-style match counter.
 */

import { describe, expect, it } from "vitest";
import { interpretFindKey, formatMatchCount } from "./TerminalFindBar";

describe("interpretFindKey", () => {
  it("maps Enter / F3 to next", () => {
    expect(interpretFindKey({ key: "Enter", shiftKey: false })).toBe("next");
    expect(interpretFindKey({ key: "F3", shiftKey: false })).toBe("next");
  });

  it("maps Shift+Enter / Shift+F3 to prev", () => {
    expect(interpretFindKey({ key: "Enter", shiftKey: true })).toBe("prev");
    expect(interpretFindKey({ key: "F3", shiftKey: true })).toBe("prev");
  });

  it("maps Escape to close", () => {
    expect(interpretFindKey({ key: "Escape", shiftKey: false })).toBe("close");
    expect(interpretFindKey({ key: "Escape", shiftKey: true })).toBe("close");
  });

  it("ignores ordinary typing", () => {
    expect(interpretFindKey({ key: "a", shiftKey: false })).toBeNull();
    expect(interpretFindKey({ key: "Backspace", shiftKey: false })).toBeNull();
    expect(interpretFindKey({ key: "ArrowDown", shiftKey: false })).toBeNull();
  });
});

describe("formatMatchCount", () => {
  it("is empty with no query", () => {
    expect(formatMatchCount(-1, 0, "")).toBe("");
    expect(formatMatchCount(3, 10, "")).toBe("");
  });

  it("shows 'No results' for a query with zero matches", () => {
    expect(formatMatchCount(-1, 0, "nope")).toBe("No results");
  });

  it("shows 'k of n' with a 1-based active index", () => {
    expect(formatMatchCount(0, 17, "x")).toBe("1 of 17");
    expect(formatMatchCount(16, 17, "x")).toBe("17 of 17");
  });

  it("falls back to the bare total when the index is unavailable (-1 over highlight limit)", () => {
    expect(formatMatchCount(-1, 1234, "x")).toBe("1234");
  });
});
