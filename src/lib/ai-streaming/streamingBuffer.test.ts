/**
 * Tests for the streaming accumulator + its display window (plan
 * `2026-07-28-runner-many-sessions-performance.md` §A5: "cap the visible
 * streaming buffer — tail window + show earlier").
 */

import { describe, it, expect } from "vitest";

import {
  computeStreamWindow,
  STREAM_EARLIER_STEP_CHARS,
  STREAM_TAIL_CHARS,
  StreamingTextBuffer,
} from "./streamingBuffer";

describe("StreamingTextBuffer", () => {
  it("preserves content and order across many appends", () => {
    const buffer = new StreamingTextBuffer();
    const lines = Array.from({ length: 1000 }, (_, i) => `line ${i}\n`);
    for (const line of lines) buffer.append(line);

    expect(buffer.text).toBe(lines.join(""));
    expect(buffer.length).toBe(lines.join("").length);
    expect(buffer.droppedChars).toBe(0);
  });

  it("reports length without forcing a materialisation mismatch", () => {
    const buffer = new StreamingTextBuffer();
    buffer.append("abc");
    expect(buffer.length).toBe(3);
    buffer.append("de");
    expect(buffer.length).toBe(5);
    expect(buffer.text).toBe("abcde");
    expect(buffer.length).toBe(5);
  });

  it("ignores empty appends", () => {
    const buffer = new StreamingTextBuffer();
    buffer.append("");
    expect(buffer.isEmpty).toBe(true);
    buffer.append("x");
    expect(buffer.isEmpty).toBe(false);
  });

  it("caps retention by dropping the OLDEST text and counting it", () => {
    const buffer = new StreamingTextBuffer({ maxChars: 100, trimToChars: 60 });
    for (let i = 0; i < 20; i++) {
      buffer.append("x".repeat(10)); // 200 chars total
    }

    const text = buffer.text;
    expect(text.length).toBeLessThanOrEqual(100);
    expect(buffer.droppedChars).toBeGreaterThan(0);
    expect(buffer.droppedChars + text.length).toBe(200);
  });

  it("keeps the TAIL when trimming, so the newest output survives", () => {
    const buffer = new StreamingTextBuffer({ maxChars: 50, trimToChars: 30 });
    buffer.append("A".repeat(60));
    buffer.append("TAIL");

    const text = buffer.text;
    expect(text.endsWith("TAIL")).toBe(true);
    expect(text.startsWith("A")).toBe(true);
  });

  it("reset() clears the text AND the dropped counter", () => {
    const buffer = new StreamingTextBuffer({ maxChars: 10, trimToChars: 5 });
    buffer.append("y".repeat(100));
    expect(buffer.droppedChars).toBeGreaterThan(0);

    buffer.reset();
    expect(buffer.text).toBe("");
    expect(buffer.droppedChars).toBe(0);
    expect(buffer.isEmpty).toBe(true);
  });

  it("never sets a trim target at or above the ceiling", () => {
    // A trim target >= the ceiling would re-trim on every single append.
    const buffer = new StreamingTextBuffer({ maxChars: 10, trimToChars: 999 });
    buffer.append("z".repeat(50));
    expect(buffer.text.length).toBeLessThan(10);
  });
});

describe("computeStreamWindow — the 'show earlier' ladder", () => {
  const text = Array.from({ length: 2000 }, (_, i) => `line ${i}\n`).join("");

  it("renders only the tail by default", () => {
    const win = computeStreamWindow(text, STREAM_TAIL_CHARS);
    expect(win.visible.length).toBe(STREAM_TAIL_CHARS);
    expect(win.visible).toBe(text.slice(text.length - STREAM_TAIL_CHARS));
    expect(win.hiddenChars).toBe(text.length - STREAM_TAIL_CHARS);
    expect(win.atStart).toBe(false);
  });

  it("widening the window reveals strictly more of the SAME text, tail-anchored", () => {
    let windowChars = STREAM_TAIL_CHARS;
    let previous = computeStreamWindow(text, windowChars);

    for (let step = 0; step < 5; step++) {
      windowChars += STREAM_EARLIER_STEP_CHARS;
      const next = computeStreamWindow(text, windowChars);
      expect(next.visible.endsWith(previous.visible)).toBe(true);
      expect(next.visible.length).toBeGreaterThanOrEqual(previous.visible.length);
      expect(next.hiddenChars).toBeLessThanOrEqual(previous.hiddenChars);
      previous = next;
      if (next.hiddenChars === 0) break;
    }
  });

  it("'show all' reaches the WHOLE retained buffer", () => {
    const win = computeStreamWindow(text, Number.MAX_SAFE_INTEGER);
    expect(win.visible).toBe(text);
    expect(win.hiddenChars).toBe(0);
    expect(win.atStart).toBe(true);
  });

  it("distinguishes hidden-but-retained from permanently dropped", () => {
    const win = computeStreamWindow(text, Number.MAX_SAFE_INTEGER, 4242);
    expect(win.hiddenChars).toBe(0);
    expect(win.droppedChars).toBe(4242);
    // Nothing is hidden, but the buffer is not at the true start of the turn.
    expect(win.atStart).toBe(false);
  });

  it("short buffers render whole with no controls", () => {
    const win = computeStreamWindow("hello", STREAM_TAIL_CHARS);
    expect(win.visible).toBe("hello");
    expect(win.hiddenChars).toBe(0);
    expect(win.atStart).toBe(true);
  });

  it("buffer.window() composes the cap and the display window", () => {
    const buffer = new StreamingTextBuffer({ maxChars: 500, trimToChars: 300 });
    buffer.append("Q".repeat(1000));
    buffer.append("END");

    const win = buffer.window(10);
    expect(win.visible.length).toBe(10);
    expect(win.visible.endsWith("END")).toBe(true);
    expect(win.droppedChars).toBe(buffer.droppedChars);
    expect(win.hiddenChars).toBe(buffer.text.length - 10);
  });
});
