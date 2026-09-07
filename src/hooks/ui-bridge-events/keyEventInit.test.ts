import { describe, it, expect } from "vitest";
import { buildKeyboardEventInit, keyToCode, keyToKeyCode, shouldKeypress } from "./keyEventInit";

/**
 * `dispatch_key`'s event construction had no test at all — it lived inline in a
 * `switch` arm inside a `useCallback` — which is how a `keypress` built on
 * `keydown` terms and six named keys dispatching with `keyCode: 0` both shipped
 * inside the very change that added the legacy fields.
 *
 * The rule these tests pin: a key the handler ACCEPTS must never dispatch with
 * `keyCode: 0` for want of a table entry, and the legacy triple must say what a
 * browser says for the event actually being fired.
 *
 * These assert the `KeyboardEventInit` rather than a constructed
 * `KeyboardEvent`: this repo's vitest environment is `node` (no `jsdom` or
 * `happy-dom` is installed, and no test here uses one), and the init IS the
 * whole contribution of the code under test — the constructor is the platform's.
 */
describe("keyToCode", () => {
  it("maps letters, digits and space to physical codes", () => {
    expect(keyToCode("a")).toBe("KeyA");
    expect(keyToCode("A")).toBe("KeyA");
    expect(keyToCode("7")).toBe("Digit7");
    expect(keyToCode(" ")).toBe("Space");
  });

  it("passes named keys through unchanged", () => {
    expect(keyToCode("Enter")).toBe("Enter");
    expect(keyToCode("ArrowLeft")).toBe("ArrowLeft");
  });

  it("returns an empty string rather than throwing on a non-key", () => {
    expect(keyToCode("")).toBe("");
    expect(keyToCode(undefined as unknown as string)).toBe("");
  });
});

describe("keyToKeyCode", () => {
  it("reports the PHYSICAL code for letters, so case does not change it", () => {
    expect(keyToKeyCode("b")).toBe(66);
    expect(keyToKeyCode("B")).toBe(66);
  });

  it("reports digits and space", () => {
    expect(keyToKeyCode("1")).toBe(49);
    expect(keyToKeyCode(" ")).toBe(32);
  });

  it("reports US-layout virtual-key codes for punctuation", () => {
    // `;` and `:` are the same physical key, so both are 186. This is the one
    // deliberate divergence from the SDK's table (which reports 59 / 58); see
    // the module header.
    expect(keyToKeyCode(";")).toBe(186);
    expect(keyToKeyCode(":")).toBe(186);
    expect(keyToKeyCode("/")).toBe(191);
    expect(keyToKeyCode("?")).toBe(191);
  });

  it("covers every named key the six-key gap left at 0", () => {
    // These were absent from the inline table, so `dispatch_key` sent them with
    // `keyCode: 0` — the same silent no-op the legacy fields exist to prevent,
    // reached through a key the API accepts.
    expect(keyToKeyCode("Cancel")).toBe(3);
    expect(keyToKeyCode("Clear")).toBe(12);
    expect(keyToKeyCode("Select")).toBe(41);
    expect(keyToKeyCode("PrintScreen")).toBe(44);
    expect(keyToKeyCode("Help")).toBe(47);
    expect(keyToKeyCode("AltGraph")).toBe(225);
  });

  it("still covers the named keys that were already tabled", () => {
    expect(keyToKeyCode("Enter")).toBe(13);
    expect(keyToKeyCode("Escape")).toBe(27);
    expect(keyToKeyCode("ArrowDown")).toBe(40);
    expect(keyToKeyCode("Meta")).toBe(91);
    expect(keyToKeyCode("ScrollLock")).toBe(145);
  });

  it("covers F1-F24 as a range, not just the twelve that were listed", () => {
    expect(keyToKeyCode("F1")).toBe(112);
    expect(keyToKeyCode("F12")).toBe(123);
    // F13-F24 previously fell through the table and dispatched as 0.
    expect(keyToKeyCode("F13")).toBe(124);
    expect(keyToKeyCode("F24")).toBe(135);
    // F25 is not a key, so 0 is the honest answer.
    expect(keyToKeyCode("F25")).toBe(0);
  });

  it("returns 0 — never a fabricated code — for a key it cannot place", () => {
    expect(keyToKeyCode("Undo")).toBe(0);
    expect(keyToKeyCode("Paste")).toBe(0);
    expect(keyToKeyCode("")).toBe(0);
    expect(keyToKeyCode(undefined as unknown as string)).toBe(0);
  });
});

describe("buildKeyboardEventInit", () => {
  it("carries the modern fields and mirrors which onto keyCode", () => {
    const init = buildKeyboardEventInit("Enter", {}, "keydown");
    expect(init.key).toBe("Enter");
    expect(init.code).toBe("Enter");
    expect(init.keyCode).toBe(13);
    expect(init.which).toBe(13);
    expect(init.bubbles).toBe(true);
    expect(init.cancelable).toBe(true);
  });

  it("projects the modifier flags", () => {
    const init = buildKeyboardEventInit("s", { ctrl: true, shift: true }, "keydown");
    expect(init.ctrlKey).toBe(true);
    expect(init.shiftKey).toBe(true);
    expect(init.altKey).toBe(false);
    expect(init.metaKey).toBe(false);
  });

  it("defaults to keydown terms when no type is given", () => {
    expect(buildKeyboardEventInit("b").keyCode).toBe(66);
    expect(buildKeyboardEventInit("b").charCode).toBe(0);
  });

  it("reports the PHYSICAL key on keydown and keyup, with charCode 0", () => {
    for (const type of ["keydown", "keyup"] as const) {
      const lower = buildKeyboardEventInit("b", {}, type);
      const upper = buildKeyboardEventInit("B", { shift: true }, type);
      expect(lower.keyCode).toBe(66);
      expect(lower.which).toBe(66);
      expect(lower.charCode).toBe(0);
      expect(upper.keyCode).toBe(66);
      expect(upper.charCode).toBe(0);
    }
  });

  it("reports the CHARACTER on keypress, case intact", () => {
    // The defect this fixes: one init was built on keydown terms and reused for
    // the whole triple, so a keypress carried charCode 0 and a case-folded
    // which — `String.fromCharCode(e.charCode || e.which)` recovered "B" for a
    // typed "b", or nothing at all.
    const lower = buildKeyboardEventInit("b", {}, "keypress");
    expect(lower.charCode).toBe(98);
    expect(lower.keyCode).toBe(98);
    expect(lower.which).toBe(98);
    expect(String.fromCharCode(lower.charCode as number)).toBe("b");

    const upper = buildKeyboardEventInit("B", { shift: true }, "keypress");
    expect(upper.charCode).toBe(66);
    expect(String.fromCharCode(upper.charCode as number)).toBe("B");
  });

  it("keeps a keypress init's code on the physical key", () => {
    // `code` is layout-physical for every event in the triple; only the legacy
    // trio changes meaning on keypress.
    expect(buildKeyboardEventInit("b", {}, "keypress").code).toBe("KeyB");
  });

  it("does not invent a charCode for a named key", () => {
    // A named key never reaches keypress via `shouldKeypress`, but the builder
    // must not fabricate a code point from a multi-character name if it does.
    expect(buildKeyboardEventInit("Enter", {}, "keypress").charCode).toBe(0);
  });
});

describe("shouldKeypress", () => {
  it("fires for a bare printable character", () => {
    expect(shouldKeypress("b", {})).toBe(true);
    expect(shouldKeypress("b", { shift: true })).toBe(true);
    expect(shouldKeypress(" ", {})).toBe(true);
  });

  it("does not fire for a named key or a ctrl/alt/meta combo", () => {
    expect(shouldKeypress("Enter", {})).toBe(false);
    expect(shouldKeypress("b", { ctrl: true })).toBe(false);
    expect(shouldKeypress("b", { alt: true })).toBe(false);
    expect(shouldKeypress("b", { meta: true })).toBe(false);
  });

  it("treats absent modifiers as none", () => {
    expect(shouldKeypress("b")).toBe(true);
  });
});
