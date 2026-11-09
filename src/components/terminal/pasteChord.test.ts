// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

import { claimPasteChord, isPasteChord } from "./pasteChord";

function key(init: KeyboardEventInit & { type?: string }): KeyboardEvent {
  const { type = "keydown", ...rest } = init;
  return new KeyboardEvent(type, { cancelable: true, ...rest });
}

describe("claimPasteChord", () => {
  it("cancels Ctrl+V keydown so the webview fires no second, native paste", () => {
    const ev = key({ key: "v", ctrlKey: true });
    expect(claimPasteChord(ev)).toBe(true);
    expect(ev.defaultPrevented).toBe(true);
  });

  it("cancels Cmd+V keydown", () => {
    const ev = key({ key: "v", metaKey: true });
    expect(claimPasteChord(ev)).toBe(true);
    expect(ev.defaultPrevented).toBe(true);
  });

  it("treats a caps-locked Ctrl+V (key 'V') as the chord", () => {
    const ev = key({ key: "V", ctrlKey: true });
    expect(claimPasteChord(ev)).toBe(true);
    expect(ev.defaultPrevented).toBe(true);
  });

  it.each([
    [
      "Ctrl+Shift+V (xterm's own paste is its only writer)",
      { key: "V", ctrlKey: true, shiftKey: true },
    ],
    ["AltGr/Ctrl+Alt+V", { key: "v", ctrlKey: true, altKey: true }],
    ["bare v", { key: "v" }],
    ["Ctrl+C", { key: "c", ctrlKey: true }],
  ])("leaves %s alone", (_label, init) => {
    const ev = key(init);
    expect(claimPasteChord(ev)).toBe(false);
    expect(ev.defaultPrevented).toBe(false);
  });

  it("ignores the keyup and keypress of Ctrl+V", () => {
    for (const type of ["keyup", "keypress"]) {
      const ev = key({ type, key: "v", ctrlKey: true });
      expect(isPasteChord(ev)).toBe(false);
      expect(claimPasteChord(ev)).toBe(false);
      expect(ev.defaultPrevented).toBe(false);
    }
  });
});

describe("TerminalInstance's Ctrl+V branch", () => {
  // The predicate above is only half the fix: the handler must CALL it, or the
  // keystroke is not cancelled and the native paste writes a second copy. An
  // inline `event.key === "v"` test restored there would keep every case above
  // green while the double paste came back.
  it("gates the manual paste on claimPasteChord", () => {
    const src = readFileSync(resolve(__dirname, "TerminalInstance.tsx"), "utf8");
    expect(src).toMatch(/if \(claimPasteChord\(event\)\) \{\s*navigator\.clipboard/);
  });
});
