/**
 * `nextOutputLines` — which source feeds a tab's compact-view output lines.
 *
 * The regression these pin is a SILENT one: an unmounted tab whose xterm buffer
 * read comes back empty used to fall through both branches and publish nothing,
 * while every other tracking surface for that tab (state chip, sparkline, last
 * output time) kept updating. Nothing failed; the lines just froze. It only
 * became reachable in production when `unwatched_flush_interval_ms` was wired
 * into the runner's emission path, which stands the `terminal-activity` digest
 * down and leaves this tap as such a tab's only feed.
 */

import { describe, it, expect } from "vitest";
import { nextOutputLines, stripAnsi, OUTPUT_LINES_WINDOW } from "./outputLineTracking";

/** Assert the fallback getter is not consulted on the buffer path. */
function throwingGetter(): readonly string[] {
  throw new Error("getExisting must not be called when the buffer answered");
}

describe("nextOutputLines", () => {
  it("prefers the xterm buffer when it has lines, and never reads the store", () => {
    expect(nextOutputLines(["a", "b"], "raw text\n", throwingGetter)).toEqual(["a", "b"]);
  });

  it("trims the buffer read to the window, keeping the NEWEST lines", () => {
    const many = Array.from({ length: OUTPUT_LINES_WINDOW + 5 }, (_, i) => `l${i}`);
    const out = nextOutputLines(many, "", throwingGetter);
    expect(out).toHaveLength(OUTPUT_LINES_WINDOW);
    expect(out?.[out.length - 1]).toBe(`l${OUTPUT_LINES_WINDOW + 4}`);
  });

  /**
   * The headline case. An unmounted tab's reader returns `[]` — the reader
   * EXISTS (the provider always supplies one), it simply has no live pane to
   * read. Before the fix this returned nothing at all.
   */
  it("falls back to the raw chunk when the buffer read is EMPTY", () => {
    expect(nextOutputLines([], "hello\nworld\n", () => [])).toEqual(["hello", "world"]);
  });

  it("accumulates onto what is already published", () => {
    expect(nextOutputLines([], "third\n", () => ["first", "second"])).toEqual([
      "first",
      "second",
      "third",
    ]);
  });

  it("collapses only CONSECUTIVE repeats, so a reprinted prompt survives", () => {
    // "$ " repeated back-to-back inside one chunk collapses; the same text
    // returning after another line is a real second prompt and is kept.
    expect(nextOutputLines([], "$ \n$ \nbuilt\n$ \n", () => [])).toEqual(["$ ", "built", "$ "]);
  });

  it("collapses a repeat across the seam with what is already published", () => {
    expect(nextOutputLines([], "tail\nnew\n", () => ["head", "tail"])).toEqual([
      "head",
      "tail",
      "new",
    ]);
  });

  it("trims the accumulated result to the window", () => {
    const existing = Array.from({ length: OUTPUT_LINES_WINDOW }, (_, i) => `old${i}`);
    const out = nextOutputLines([], "fresh\n", () => existing);
    expect(out).toHaveLength(OUTPUT_LINES_WINDOW);
    expect(out?.[out.length - 1]).toBe("fresh");
    expect(out?.[0]).toBe("old1");
  });

  it("returns null when nothing survives the strip, so no store write happens", () => {
    expect(nextOutputLines([], "", () => ["kept"])).toBeNull();
    expect(nextOutputLines([], "\n   \n", () => ["kept"])).toBeNull();
    // A pure cursor-motion frame is all escapes and no text.
    expect(nextOutputLines([], "\x1b[2J\x1b[H", () => ["kept"])).toBeNull();
  });

  it("treats a missing buffer reader the same as an empty read", () => {
    // The provider always supplies one today, but the signature allows `[]`
    // from either cause and both must reach the fallback.
    expect(nextOutputLines([], "line\n", () => [])).toEqual(["line"]);
  });
});

describe("stripAnsi", () => {
  it("removes the sequence families a PTY actually emits", () => {
    expect(stripAnsi("\x1b[31mred\x1b[0m")).toBe("red");
    expect(stripAnsi("\x1b]0;title\x07body")).toBe("body");
    expect(stripAnsi("\x1b(Bplain")).toBe("plain");
    expect(stripAnsi("\x1b7saved\x1b8")).toBe("saved");
    expect(stripAnsi("a\x07b")).toBe("ab");
  });

  it("drops carriage returns so a rewritten line does not split", () => {
    expect(stripAnsi("progress\rdone")).toBe("progressdone");
  });

  it("keeps newlines, which are the record separator downstream", () => {
    expect(stripAnsi("one\ntwo")).toBe("one\ntwo");
  });
});
