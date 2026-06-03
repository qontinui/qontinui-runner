import { describe, it, expect } from "vitest";
import { buildPastePayload } from "./pastePayload";

const BEGIN = "\x1b[200~";
const END = "\x1b[201~";

describe("buildPastePayload", () => {
  it("wraps in bracketed-paste markers when mode 2004 is enabled", () => {
    expect(buildPastePayload("hello", true)).toBe(`${BEGIN}hello${END}`);
  });

  it("does not wrap when bracketed paste is disabled", () => {
    expect(buildPastePayload("hello", false)).toBe("hello");
  });

  it("normalizes CRLF and LF to bare CR", () => {
    expect(buildPastePayload("a\r\nb\nc", false)).toBe("a\rb\rc");
  });

  it("keeps a large multi-line paste as a single bracketed block", () => {
    const lines = Array.from({ length: 20 }, (_, i) => `line ${i}`);
    const payload = buildPastePayload(lines.join("\n"), true);
    // Exactly one begin marker and one end marker — the whole block is atomic.
    expect(payload.startsWith(BEGIN)).toBe(true);
    expect(payload.endsWith(END)).toBe(true);
    expect(payload.split(BEGIN)).toHaveLength(2);
    expect(payload.split(END)).toHaveLength(2);
    // Newlines inside the block are CRs, not the closing marker.
    expect(payload).toBe(`${BEGIN}${lines.join("\r")}${END}`);
  });

  it("does not append a trailing submit CR (paste must not auto-submit)", () => {
    expect(buildPastePayload("cmd", true)).toBe(`${BEGIN}cmd${END}`);
    expect(buildPastePayload("cmd", false)).toBe("cmd");
  });

  it("handles empty input", () => {
    expect(buildPastePayload("", true)).toBe(`${BEGIN}${END}`);
    expect(buildPastePayload("", false)).toBe("");
  });
});
