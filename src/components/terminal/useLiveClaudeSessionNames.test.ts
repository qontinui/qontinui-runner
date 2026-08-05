import { describe, it, expect } from "vitest";

import { sameNames } from "./useLiveClaudeSessionNames";

/**
 * `sameNames` is the invariant the whole polling design rests on: the store
 * publishes a NEW map only when the content changed, which is what lets
 * `useSessionManager` list the map directly in its `allSessions` dependency
 * array. If this ever returns `false` for equal content, every 30s poll
 * rebuilds every `UnifiedSession` and the regression is invisible except as
 * jank — so it gets its own tests rather than riding on the callers'.
 *
 * The hook itself is not exercised here: the runner's vitest env is `node`
 * (no jsdom, no `@testing-library/react`), matching `PastSessionsView.test.ts`
 * and `LaunchMenu.test.tsx`. On-device behaviour is verified against a temp
 * runner.
 */
describe("sameNames", () => {
  it("is true for the same reference", () => {
    const m = new Map([["a", "one"]]);
    expect(sameNames(m, m)).toBe(true);
  });

  it("is true for distinct maps with equal content", () => {
    expect(
      sameNames(
        new Map([
          ["a", "one"],
          ["b", "two"],
        ]),
        new Map([
          ["a", "one"],
          ["b", "two"],
        ]),
      ),
    ).toBe(true);
  });

  it("ignores insertion order", () => {
    expect(
      sameNames(
        new Map([
          ["a", "one"],
          ["b", "two"],
        ]),
        new Map([
          ["b", "two"],
          ["a", "one"],
        ]),
      ),
    ).toBe(true);
  });

  it("is false when a name changed — the /rename case that must republish", () => {
    expect(sameNames(new Map([["a", "old"]]), new Map([["a", "new"]]))).toBe(false);
  });

  it("is false when an entry was added or removed", () => {
    expect(sameNames(new Map([["a", "one"]]), new Map())).toBe(false);
    expect(
      sameNames(
        new Map([["a", "one"]]),
        new Map([
          ["a", "one"],
          ["b", "two"],
        ]),
      ),
    ).toBe(false);
  });

  it("is false when a key was swapped for another at the same size", () => {
    // Guards against a size-only comparison.
    expect(sameNames(new Map([["a", "one"]]), new Map([["b", "one"]]))).toBe(false);
  });

  it("is true for two empty maps", () => {
    expect(sameNames(new Map(), new Map())).toBe(true);
  });
});
