/**
 * Tests for the `sendKeys` → PTY-byte translation.
 *
 * THE DEFECT these cover: `@qontinui/ui-bridge` resolved built-in actions
 * before an element's own `customActions`, so `TerminalInstance`'s registered
 * `sendKeys` handler had never executed. 0.24.0 (ui-bridge#165) flipped that
 * precedence, handing the handler the two ARRAY grammars the built-in used to
 * serve. The handler read `keys` as a string and passed it to `writePty`, whose
 * `TextEncoder.encode` coerces via `String()` — so `["Enter"]` would have typed
 * the word "Enter" and `[{ key: "Enter" }]` the text "[object Object]" into a
 * live pane, and answered `success: true` with a byte count.
 */

import { describe, expect, it } from "vitest";

import { SEND_KEYS_INVALID, toPtySequence } from "./terminalKeySequence";

describe("toPtySequence — raw string form", () => {
  it("writes a string verbatim", () => {
    expect(toPtySequence("ls -la\r")).toBe("ls -la\r");
  });

  it("passes escape sequences through untouched", () => {
    expect(toPtySequence("\x1b[A")).toBe("\x1b[A");
  });

  it("rejects an empty string rather than writing nothing and claiming success", () => {
    expect(() => toPtySequence("")).toThrow(SEND_KEYS_INVALID);
  });
});

describe("toPtySequence — bare key-name array", () => {
  it("translates Enter to CR, not the literal text 'Enter'", () => {
    // The exact regression 0.24.0 would have introduced.
    expect(toPtySequence(["Enter"])).toBe("\r");
  });

  it("is case-insensitive on key names", () => {
    expect(toPtySequence(["enter"])).toBe("\r");
    expect(toPtySequence(["ESCAPE"])).toBe("\x1b");
  });

  it("concatenates a sequence in order", () => {
    expect(toPtySequence(["h", "i", "Enter"])).toBe("hi\r");
  });

  it("maps the cursor keys to normal-mode CSI", () => {
    expect(toPtySequence(["ArrowUp"])).toBe("\x1b[A");
    expect(toPtySequence(["ArrowDown"])).toBe("\x1b[B");
    expect(toPtySequence(["ArrowRight"])).toBe("\x1b[C");
    expect(toPtySequence(["ArrowLeft"])).toBe("\x1b[D");
  });

  it("maps the editing and function keys", () => {
    expect(toPtySequence(["Tab"])).toBe("\t");
    expect(toPtySequence(["Backspace"])).toBe("\x7f");
    expect(toPtySequence(["Delete"])).toBe("\x1b[3~");
    expect(toPtySequence(["Home"])).toBe("\x1b[H");
    expect(toPtySequence(["End"])).toBe("\x1b[F");
    expect(toPtySequence(["PageUp"])).toBe("\x1b[5~");
    expect(toPtySequence(["F1"])).toBe("\x1bOP");
    expect(toPtySequence(["F5"])).toBe("\x1b[15~");
    expect(toPtySequence(["F12"])).toBe("\x1b[24~");
  });
});

describe("toPtySequence — SDK descriptor array", () => {
  it("translates a descriptor, not '[object Object]'", () => {
    // The other regression 0.24.0 would have introduced.
    expect(toPtySequence([{ key: "Enter" }])).toBe("\r");
  });

  it("maps Ctrl+<letter> to its control code", () => {
    expect(toPtySequence([{ key: "c", modifiers: { ctrl: true } }])).toBe("\x03");
    expect(toPtySequence([{ key: "d", modifiers: { ctrl: true } }])).toBe("\x04");
    // Case of the letter must not change the control code.
    expect(toPtySequence([{ key: "C", modifiers: { ctrl: true } }])).toBe("\x03");
  });

  it("maps Ctrl on the punctuation that has a canonical control code", () => {
    expect(toPtySequence([{ key: "[", modifiers: { ctrl: true } }])).toBe("\x1b");
    expect(toPtySequence([{ key: " ", modifiers: { ctrl: true } }])).toBe("\x00");
  });

  it("prefixes Alt/Meta with ESC", () => {
    expect(toPtySequence([{ key: "b", modifiers: { alt: true } }])).toBe("\x1bb");
    expect(toPtySequence([{ key: "f", modifiers: { meta: true } }])).toBe("\x1bf");
  });

  it("uppercases a shifted letter but does not guess a layout for symbols", () => {
    expect(toPtySequence([{ key: "a", modifiers: { shift: true } }])).toBe("A");
    expect(toPtySequence([{ key: "1", modifiers: { shift: true } }])).toBe("1");
  });

  it("emits the CSI-modified form for a modified cursor key", () => {
    // Ctrl+Left — word-back in every readline shell. bitmask 4 + 1 = 5.
    expect(toPtySequence([{ key: "ArrowLeft", modifiers: { ctrl: true } }])).toBe("\x1b[1;5D");
    // Shift+Up — bitmask 1 + 1 = 2.
    expect(toPtySequence([{ key: "ArrowUp", modifiers: { shift: true } }])).toBe("\x1b[1;2A");
  });

  it("emits the CSI-modified form for a tilde-terminated key", () => {
    // Ctrl+Delete.
    expect(toPtySequence([{ key: "Delete", modifiers: { ctrl: true } }])).toBe("\x1b[3;5~");
  });

  it("accepts a mixed array of names and descriptors", () => {
    expect(toPtySequence(["l", "s", { key: "Enter" }])).toBe("ls\r");
  });
});

describe("toPtySequence — refuses what it cannot translate", () => {
  it("throws on a missing payload instead of writing 'undefined'", () => {
    expect(() => toPtySequence(undefined)).toThrow(SEND_KEYS_INVALID);
    expect(() => toPtySequence(null)).toThrow(SEND_KEYS_INVALID);
  });

  it("throws on an empty array", () => {
    expect(() => toPtySequence([])).toThrow(SEND_KEYS_INVALID);
  });

  it("throws on an unknown key NAME rather than typing the name", () => {
    // The whole hazard: a pane is usually a live Claude/PowerShell session, so
    // an untranslatable key must fail loudly, never land as literal text.
    expect(() => toPtySequence(["Enterr"])).toThrow(/unknown key 'Enterr'/);
    expect(() => toPtySequence([{ key: "SuperKey" }])).toThrow(/unknown key 'SuperKey'/);
  });

  it("throws on a descriptor with no key", () => {
    expect(() => toPtySequence([{ modifiers: { ctrl: true } }])).toThrow(SEND_KEYS_INVALID);
    expect(() => toPtySequence([{ key: "" }])).toThrow(SEND_KEYS_INVALID);
  });

  it("throws on a Ctrl pairing with no control code", () => {
    expect(() => toPtySequence([{ key: "é", modifiers: { ctrl: true } }])).toThrow(
      /no control-code equivalent/,
    );
  });

  it("carries a machine-readable code on the thrown error", () => {
    try {
      toPtySequence([]);
      expect.unreachable("should have thrown");
    } catch (err) {
      expect((err as { code?: string }).code).toBe(SEND_KEYS_INVALID);
    }
  });
});
