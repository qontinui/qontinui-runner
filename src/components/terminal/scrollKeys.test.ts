import { describe, expect, it } from "vitest";
import { matchScrollShortcut, type KeyLike } from "./scrollKeys";

const base: KeyLike = {
  key: "",
  shiftKey: false,
  ctrlKey: false,
  altKey: false,
  metaKey: false,
};

const k = (over: Partial<KeyLike>): KeyLike => ({ ...base, ...over });

describe("matchScrollShortcut", () => {
  it("maps Shift+PageUp / Shift+PageDown to page scroll", () => {
    expect(matchScrollShortcut(k({ key: "PageUp", shiftKey: true }))).toEqual({
      kind: "pages",
      amount: -1,
    });
    expect(matchScrollShortcut(k({ key: "PageDown", shiftKey: true }))).toEqual({
      kind: "pages",
      amount: 1,
    });
  });

  it("maps Ctrl+Alt+PageUp / Ctrl+Alt+PageDown to line scroll", () => {
    expect(matchScrollShortcut(k({ key: "PageUp", ctrlKey: true, altKey: true }))).toEqual({
      kind: "lines",
      amount: -1,
    });
    expect(matchScrollShortcut(k({ key: "PageDown", ctrlKey: true, altKey: true }))).toEqual({
      kind: "lines",
      amount: 1,
    });
  });

  it("maps Ctrl+Home / Ctrl+End to top / bottom", () => {
    expect(matchScrollShortcut(k({ key: "Home", ctrlKey: true }))).toEqual({ kind: "top" });
    expect(matchScrollShortcut(k({ key: "End", ctrlKey: true }))).toEqual({ kind: "bottom" });
  });

  it("maps Ctrl+Up / Ctrl+Down to previous / next command navigation", () => {
    expect(matchScrollShortcut(k({ key: "ArrowUp", ctrlKey: true }))).toEqual({
      kind: "prevCommand",
    });
    expect(matchScrollShortcut(k({ key: "ArrowDown", ctrlKey: true }))).toEqual({
      kind: "nextCommand",
    });
  });

  it("accepts Cmd (metaKey) as a Ctrl alias for macOS", () => {
    expect(matchScrollShortcut(k({ key: "Home", metaKey: true }))).toEqual({ kind: "top" });
    expect(matchScrollShortcut(k({ key: "End", metaKey: true }))).toEqual({ kind: "bottom" });
    expect(matchScrollShortcut(k({ key: "PageUp", metaKey: true, altKey: true }))).toEqual({
      kind: "lines",
      amount: -1,
    });
    expect(matchScrollShortcut(k({ key: "ArrowUp", metaKey: true }))).toEqual({
      kind: "prevCommand",
    });
  });

  it("does not match a bare PageUp/PageDown/Home/End (left for the app)", () => {
    expect(matchScrollShortcut(k({ key: "PageUp" }))).toBeNull();
    expect(matchScrollShortcut(k({ key: "PageDown" }))).toBeNull();
    expect(matchScrollShortcut(k({ key: "Home" }))).toBeNull();
    expect(matchScrollShortcut(k({ key: "End" }))).toBeNull();
  });

  it("does not match unrelated keys or wrong modifier combos", () => {
    expect(matchScrollShortcut(k({ key: "a", ctrlKey: true }))).toBeNull();
    // Ctrl+PageUp (no Alt) is NOT a scroll shortcut — leave it for the app/tabs.
    expect(matchScrollShortcut(k({ key: "PageUp", ctrlKey: true }))).toBeNull();
    // Shift+Home is selection in most apps, not a terminal scroll binding.
    expect(matchScrollShortcut(k({ key: "Home", shiftKey: true }))).toBeNull();
    // Ctrl+Shift+Home should not scroll (Shift disqualifies the top binding).
    expect(matchScrollShortcut(k({ key: "Home", ctrlKey: true, shiftKey: true }))).toBeNull();
    // Bare arrows belong to the app (history navigation in shells/TUIs).
    expect(matchScrollShortcut(k({ key: "ArrowUp" }))).toBeNull();
    expect(matchScrollShortcut(k({ key: "ArrowDown" }))).toBeNull();
    // Ctrl+Shift+Arrows are page-level focus-history shortcuts — leave them.
    expect(matchScrollShortcut(k({ key: "ArrowUp", ctrlKey: true, shiftKey: true }))).toBeNull();
    // Ctrl+Alt+Arrows are not bound — leave them for the app.
    expect(matchScrollShortcut(k({ key: "ArrowDown", ctrlKey: true, altKey: true }))).toBeNull();
  });
});
