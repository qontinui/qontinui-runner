/**
 * Pure-logic tests for the input accumulator extracted from
 * `TerminalInstance`'s xterm.js `onData` loop.
 *
 * The runner's vitest config uses `environment: "node"` (no jsdom) — and
 * `TerminalInstance` itself is impractical to import from a test
 * because it transitively pulls `@xterm/addon-canvas`, which touches
 * `self` at module init and crashes under Node. The load-bearing logic
 * was extracted to `./consumeInputChunk.ts` (a leaf module with zero
 * imports) so it can be unit-tested in isolation. `TerminalInstance`'s
 * `onData` handler is now a thin shell over this helper.
 *
 * The cases below cover:
 *   - Multiple newlines in a single chunk emit multiple lines.
 *   - A line split across two chunks accumulates correctly.
 *   - Non-printable chars (code < 32) are dropped from the accumulator.
 *   - `firstInputLineIfAny` is gated by the caller's "already reported"
 *     flag so `onFirstInput` fires only once per session.
 *   - Bare empty newlines (whitespace-only) emit no line — preserving
 *     the historical "empty line trimmed" behavior.
 */

import { describe, it, expect } from "vitest";
import { consumeInputChunk } from "./consumeInputChunk";

describe("consumeInputChunk", () => {
  it("emits two lines for two newlines in one chunk", () => {
    const result = consumeInputChunk("hello\nworld\n", "", false);
    expect(result.lines).toEqual(["hello", "world"]);
    expect(result.firstInputLineIfAny).toBe("hello");
    expect(result.accum).toBe("");
  });

  it("accumulates a line split across two chunks", () => {
    const first = consumeInputChunk("hel", "", false);
    expect(first.lines).toEqual([]);
    expect(first.firstInputLineIfAny).toBeUndefined();
    expect(first.accum).toBe("hel");

    const second = consumeInputChunk("lo\n", first.accum, false);
    expect(second.lines).toEqual(["hello"]);
    expect(second.firstInputLineIfAny).toBe("hello");
    expect(second.accum).toBe("");
  });

  it("drops non-printable chars (code < 32) from the accumulator", () => {
    // ESC (0x1b) sits between two printable chars — it must be dropped
    // so the line becomes "ab", matching the historical loop behavior at
    // the `ch.charCodeAt(0) >= 32` gate.
    const result = consumeInputChunk("a\x1bb\n", "", false);
    expect(result.lines).toEqual(["ab"]);
    expect(result.firstInputLineIfAny).toBe("ab");
    expect(result.accum).toBe("");
  });

  it("does not surface firstInputLineIfAny when already reported", () => {
    const result = consumeInputChunk("second turn\n", "", true);
    // The line still flows through `lines` (for `onUserInputLine`), but
    // the first-input slot stays empty so the caller's `onFirstInput`
    // doesn't fire again.
    expect(result.lines).toEqual(["second turn"]);
    expect(result.firstInputLineIfAny).toBeUndefined();
    expect(result.accum).toBe("");
  });

  it("emits no line for a bare empty newline", () => {
    const result = consumeInputChunk("\n", "", false);
    expect(result.lines).toEqual([]);
    expect(result.firstInputLineIfAny).toBeUndefined();
    expect(result.accum).toBe("");
  });

  it("emits no line when the accumulator is whitespace-only", () => {
    // Historical "trim then check empty" — a line of just spaces is
    // dropped from both `lines` and `firstInputLineIfAny`.
    const result = consumeInputChunk("   \n", "", false);
    expect(result.lines).toEqual([]);
    expect(result.firstInputLineIfAny).toBeUndefined();
    expect(result.accum).toBe("");
  });

  it("treats \\r the same as \\n as a line terminator", () => {
    // xterm.js fires `\r` on Enter, not `\n`, so the historical loop
    // accepts either as a line break. Verify both are honored.
    const result = consumeInputChunk("alpha\rbeta\n", "", false);
    expect(result.lines).toEqual(["alpha", "beta"]);
    expect(result.firstInputLineIfAny).toBe("alpha");
  });
});
