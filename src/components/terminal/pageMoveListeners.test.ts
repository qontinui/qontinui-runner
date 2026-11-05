/**
 * The readiness signal a mover waits on before it moves a pane onto a page: a
 * page that is not yet listening for `terminal-page-changed` misses the move —
 * the source evicts the tab at once, and the target does not list it until its
 * next re-sync (`resyncTabs`, on mount or after a terminal exit).
 */

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  holdPageMoveListener,
  isPageMoveListening,
  markPageMoveListening,
  waitForPageMoveListener,
} from "./pageMoveListeners";

/** A `listen()` registration the test resolves by hand. */
function deferredRegistration() {
  let resolve!: (unlisten: () => void) => void;
  const registration = new Promise<() => void>((r) => {
    resolve = r;
  });
  return { registration, resolve };
}

/** Let the registration's `.then` run. */
const flush = () => new Promise((r) => setTimeout(r, 0));

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

  it("marks a page listening only once its listen() registration resolves, and releases it on cleanup", async () => {
    const { registration, resolve } = deferredRegistration();
    const unlisten = vi.fn();
    const cleanup = holdPageMoveListener("effect-page", registration);
    await flush();
    expect(isPageMoveListening("effect-page")).toBe(false);

    resolve(unlisten);
    await flush();
    expect(isPageMoveListening("effect-page")).toBe(true);

    cleanup();
    expect(unlisten).toHaveBeenCalledTimes(1);
    expect(isPageMoveListening("effect-page")).toBe(false);
  });

  it("unlistens a registration that resolves after the effect was cleaned up, and never marks it", async () => {
    const { registration, resolve } = deferredRegistration();
    const unlisten = vi.fn();
    const cleanup = holdPageMoveListener("unmounted-page", registration);
    cleanup();
    resolve(unlisten);
    await flush();
    expect(unlisten).toHaveBeenCalledTimes(1);
    expect(isPageMoveListening("unmounted-page")).toBe(false);
  });

  it("logs a rejected listen() registration and never marks the page listening", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    let reject!: (err: Error) => void;
    const registration = new Promise<() => void>((_resolve, r) => {
      reject = r;
    });
    const cleanup = holdPageMoveListener("failed-page", registration);
    reject(new Error("ipc down"));
    await flush();
    expect(isPageMoveListening("failed-page")).toBe(false);
    expect(warn).toHaveBeenCalledWith(expect.stringContaining("failed-page"));
    cleanup();
    warn.mockRestore();
  });
});
