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

describe("toPtySequence — a malformed `modifiers` is REJECTED, never ignored (iter 25 P0)", () => {
  // The P0 in one line: `const mods = desc.modifiers ?? {}` was a CAST over an
  // untrusted HTTP body, so anything that was not the expected object shape
  // silently degraded to "no modifiers" and the WRONG BYTES went to a live PTY.
  // Every case below answered HTTP 200 with `bytes: 1` before this guard —
  // byte-count-identical to the correct answer, so undetectable by the caller.

  it("still encodes the well-formed descriptor to the interrupt byte", () => {
    // The control. If this ever goes red the guard has over-reached.
    expect(toPtySequence([{ key: "c", modifiers: { ctrl: true } }])).toBe("\x03");
  });

  it.each([
    ['a string, {modifiers:"ctrl"}', "ctrl"],
    ['an array, {modifiers:["ctrl"]}', ["ctrl"]],
    ["a number, {modifiers:4}", 4],
    ["a boolean", true],
    ["null", null],
  ])("throws SEND_KEYS_INVALID for %s instead of typing the bare character", (_label, mods) => {
    expect(() => toPtySequence([{ key: "c", modifiers: mods as never }])).toThrow(
      SEND_KEYS_INVALID,
    );
  });

  it("does NOT let an array's Array.prototype.shift read as Shift", () => {
    // The sharpest case: `["ctrl"]` produced a CAPITAL `C` on a live PTY,
    // because `mods.shift` resolved to `Array.prototype.shift` — a function,
    // and therefore truthy. The old code returned a byte here; the new one
    // must return none at all.
    let produced: string | undefined;
    try {
      produced = toPtySequence([{ key: "c", modifiers: ["ctrl"] as never }]);
    } catch {
      produced = undefined;
    }
    expect(produced).toBeUndefined();
  });

  it("rejects a MIS-CASED modifier name rather than silently dropping it", () => {
    // `{Ctrl: true}` is a caller who plainly means Ctrl. It typed a literal
    // `c` into a live shell. Rejecting is the stated call: a modifier this
    // module does not recognise must never quietly mean NO modifiers.
    expect(() => toPtySequence([{ key: "c", modifiers: { Ctrl: true } as never }])).toThrow(
      SEND_KEYS_INVALID,
    );
    expect(() => toPtySequence([{ key: "c", modifiers: { CTRL: true } as never }])).toThrow(
      /unknown modifier/,
    );
  });

  it("rejects an unknown modifier name", () => {
    expect(() => toPtySequence([{ key: "c", modifiers: { hyper: true } as never }])).toThrow(
      /unknown modifier 'hyper'/,
    );
  });

  it("names the lower-case spelling that was probably meant", () => {
    expect(() => toPtySequence([{ key: "c", modifiers: { Ctrl: true } as never }])).toThrow(
      /'ctrl' may be what was meant/,
    );
  });

  it("rejects a non-boolean flag value rather than coercing it", () => {
    // `"false"` is truthy: coercing would turn an explicit OFF into Ctrl ON.
    expect(() => toPtySequence([{ key: "c", modifiers: { ctrl: "false" } as never }])).toThrow(
      SEND_KEYS_INVALID,
    );
    expect(() => toPtySequence([{ key: "c", modifiers: { ctrl: 1 } as never }])).toThrow(
      /must be true or false/,
    );
  });

  it("carries the machine-readable .code the SDK hoists onto the response", () => {
    try {
      toPtySequence([{ key: "c", modifiers: "ctrl" as never }]);
      expect.unreachable("should have thrown");
    } catch (err) {
      expect((err as { code?: string }).code).toBe(SEND_KEYS_INVALID);
    }
  });

  it("never leaks the rejected value's CONTENTS into the message", () => {
    // A rejected payload can be derived from a credential; its TYPE is the
    // diagnosis it deserves.
    try {
      toPtySequence([{ key: "c", modifiers: "s3cr3t-token" as never }]);
      expect.unreachable("should have thrown");
    } catch (err) {
      expect((err as Error).message).not.toContain("s3cr3t-token");
      expect((err as Error).message).toContain("a string");
    }
  });

  // ── what must KEEP working ────────────────────────────────────────────────
  it("accepts an absent `modifiers` — the documented, common shape", () => {
    expect(toPtySequence(["Enter"])).toBe("\r");
    expect(toPtySequence([{ key: "Enter" }])).toBe("\r");
    expect(toPtySequence([{ key: "c" }])).toBe("c");
  });

  it("accepts an explicitly undefined `modifiers`", () => {
    expect(toPtySequence([{ key: "c", modifiers: undefined }])).toBe("c");
  });

  it("accepts an empty modifiers object", () => {
    expect(toPtySequence([{ key: "c", modifiers: {} }])).toBe("c");
  });

  it("accepts an optional flag explicitly set to undefined", () => {
    // Idiomatic TS for `ctrl?: boolean`, and identical in meaning to omitting it.
    expect(toPtySequence([{ key: "c", modifiers: { ctrl: undefined } }])).toBe("c");
  });

  it("accepts every known flag, true and false", () => {
    expect(
      toPtySequence([
        { key: "c", modifiers: { ctrl: false, shift: false, alt: false, meta: false } },
      ]),
    ).toBe("c");
    expect(toPtySequence([{ key: "c", modifiers: { ctrl: true, alt: true } }])).toBe("\x1b\x03");
  });
});
