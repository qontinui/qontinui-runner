/**
 * The readiness signal a mover waits on before it moves a pane onto a page: a
 * page that is not yet listening for `terminal-page-changed` would let the move
 * go unadopted (the source evicts the tab, nobody mounts it).
 */

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  isPageMoveListening,
  markPageMoveListening,
  waitForPageMoveListener,
} from "./pageMoveListeners";

describe("pageMoveListeners", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("answers at once for a page that already listens", async () => {
    const release = markPageMoveListening("ready");
    await expect(waitForPageMoveListener("ready", 10)).resolves.toBe(true);
    release();
    expect(isPageMoveListening("ready")).toBe(false);
  });

  it("wakes a waiter when the page's listener registers", async () => {
    const waiting = waitForPageMoveListener("late", 5_000);
    const release = markPageMoveListening("late");
    await expect(waiting).resolves.toBe(true);
    release();
  });

  it("answers false for a page that never comes up", async () => {
    vi.useFakeTimers();
    const waiting = waitForPageMoveListener("never", 1_000);
    vi.advanceTimersByTime(1_000);
    await expect(waiting).resolves.toBe(false);
    // A listener that registers after the deadline does not resurrect it.
    const release = markPageMoveListening("never");
    expect(isPageMoveListening("never")).toBe(true);
    release();
  });

  it("stays listening until every registration is released, and releasing twice is a no-op", () => {
    const first = markPageMoveListening("double");
    const second = markPageMoveListening("double");
    first();
    first();
    expect(isPageMoveListening("double")).toBe(true);
    second();
    expect(isPageMoveListening("double")).toBe(false);
  });
});
