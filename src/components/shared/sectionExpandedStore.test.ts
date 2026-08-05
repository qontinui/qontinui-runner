/**
 * Tests for the AI response section expand/collapse store.
 *
 * The regression this pins: the store used to be a `Set` of EXPANDED keys, so
 * "collapsed" was unrepresentable. A section the operator collapsed looked
 * identical to one they had never touched, and any remount — a parent re-render
 * that changes element identity, a session switch and back, a windowed row
 * scrolling out of view — re-applied `defaultExpanded` and silently re-opened
 * it (re-parsing its markdown on the way).
 *
 * `readSectionExpanded` stands in for the component's `useState` initializer:
 * calling it again IS a remount, which is the only way to exercise this under
 * the runner's `environment: "node"` vitest config (no jsdom).
 */

import { beforeEach, describe, expect, it } from "vitest";
import {
  readSectionExpanded,
  resetSectionExpandedStore,
  setSectionExpanded,
} from "./sectionExpandedStore";

describe("sectionExpandedStore", () => {
  beforeEach(() => {
    resetSectionExpandedStore();
  });

  it("falls back to the caller's default while untouched", () => {
    expect(readSectionExpanded("resp-1", true)).toBe(true);
    expect(readSectionExpanded("resp-2", false)).toBe(false);
  });

  it("keeps a COLLAPSE across a remount, against a true default", () => {
    // The exact case the old Set could not represent.
    expect(readSectionExpanded("resp-1", true)).toBe(true);
    setSectionExpanded("resp-1", false);
    expect(readSectionExpanded("resp-1", true)).toBe(false);
    // ...and across any number of further remounts.
    expect(readSectionExpanded("resp-1", true)).toBe(false);
  });

  it("keeps an EXPAND across a remount, against a false default", () => {
    setSectionExpanded("resp-9", true);
    expect(readSectionExpanded("resp-9", false)).toBe(true);
  });

  it("round-trips a collapse → expand → collapse sequence", () => {
    setSectionExpanded("resp-3", false);
    expect(readSectionExpanded("resp-3", true)).toBe(false);
    setSectionExpanded("resp-3", true);
    expect(readSectionExpanded("resp-3", true)).toBe(true);
    setSectionExpanded("resp-3", false);
    expect(readSectionExpanded("resp-3", true)).toBe(false);
  });

  it("keys sections independently", () => {
    setSectionExpanded("resp-a", false);
    expect(readSectionExpanded("resp-a", true)).toBe(false);
    expect(readSectionExpanded("resp-b", true)).toBe(true);
  });
});
