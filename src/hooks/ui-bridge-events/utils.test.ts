/**
 * Tests for `awaitWithTimeout` and `isThenable` — the auto-await helper
 * shared by the runner's two `page_evaluate` handlers
 * (`usePageEvents.ts::page_evaluate` legacy IPC branch and
 * `useUIBridgeEvaluateHandler.ts` tagged Tauri-event handler).
 *
 * Covers the four cases the public spec promises for `page/evaluate`:
 *   - sync object passes through unchanged (regression guard)
 *   - top-level Promise resolves to the awaited value
 *   - top-level Promise rejection surfaces as a thrown Error
 *   - hanging Promise rejects with a timeout Error before exhausting the
 *     bridge's response budget
 *
 * Also pins the spec-correct thenable duck-test (cross-realm Promise
 * safety) since `instanceof Promise` would silently miss those.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import {
  awaitWithTimeout,
  isThenable,
  isElementActionAllowed,
  PAGE_EVALUATE_PROMISE_TIMEOUT_MS,
} from "./utils";

describe("isThenable", () => {
  it("accepts native Promise instances", () => {
    expect(isThenable(Promise.resolve(1))).toBe(true);
    expect(isThenable(Promise.reject(new Error("x")).catch(() => {}))).toBe(true);
  });

  it("accepts plain thenable objects (cross-realm safety)", () => {
    const thenable = { then: (onFulfilled: (v: unknown) => void) => onFulfilled(42) };
    expect(isThenable(thenable)).toBe(true);
  });

  it("rejects non-thenable objects", () => {
    expect(isThenable({ a: 1 })).toBe(false);
    expect(isThenable({ then: "not a function" })).toBe(false);
    expect(isThenable([])).toBe(false);
  });

  it("rejects primitives and null/undefined", () => {
    expect(isThenable(null)).toBe(false);
    expect(isThenable(undefined)).toBe(false);
    expect(isThenable(0)).toBe(false);
    expect(isThenable("then")).toBe(false);
    expect(isThenable(true)).toBe(false);
  });
});

describe("awaitWithTimeout", () => {
  it("returns sync values unchanged (no Promise wrap)", async () => {
    expect(await awaitWithTimeout({ a: 1 }, 1000)).toEqual({ a: 1 });
    expect(await awaitWithTimeout(42, 1000)).toBe(42);
    expect(await awaitWithTimeout(null, 1000)).toBeNull();
    expect(await awaitWithTimeout(undefined, 1000)).toBeUndefined();
  });

  it("resolves top-level Promises to their value", async () => {
    expect(await awaitWithTimeout(Promise.resolve({ a: 1 }), 1000)).toEqual({ a: 1 });
    expect(await awaitWithTimeout(Promise.resolve(42), 1000)).toBe(42);
  });

  it("resolves async-IIFE return values (the user-reported bug shape)", async () => {
    // Mirrors `(async () => ({a: 1}))()` — the exact form the bug report
    // calls out as silently returning `{}` before this fix.
    const result = await awaitWithTimeout((async () => ({ a: 1 }))(), 1000);
    expect(result).toEqual({ a: 1 });
  });

  it("propagates rejections from the awaited Promise", async () => {
    await expect(
      awaitWithTimeout(Promise.reject(new Error("boom")), 1000),
    ).rejects.toThrow("boom");
  });

  it("propagates rejections from async IIFE that throws", async () => {
    const failing = (async () => {
      throw new Error("async failure");
    })();
    await expect(awaitWithTimeout(failing, 1000)).rejects.toThrow("async failure");
  });

  it("times out hanging Promises with a descriptive error", async () => {
    vi.useFakeTimers();
    try {
      // Promise that never resolves
      const hanging = new Promise(() => {});
      const pending = awaitWithTimeout(hanging, 5_000);
      // Attach a swallow handler immediately so the unhandled-rejection
      // warning doesn't fire before the assertion runs.
      const observed = pending.catch((err: unknown) => err);
      await vi.advanceTimersByTimeAsync(5_000);
      const err = await observed;
      expect(err).toBeInstanceOf(Error);
      expect((err as Error).message).toMatch(/did not resolve within 5\.0s/);
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not time out when the Promise resolves before the cap", async () => {
    vi.useFakeTimers();
    try {
      const slow = new Promise((resolve) => setTimeout(() => resolve("done"), 1_000));
      const pending = awaitWithTimeout(slow, 5_000);
      await vi.advanceTimersByTimeAsync(1_000);
      expect(await pending).toBe("done");
    } finally {
      vi.useRealTimers();
    }
  });

  it("resolves plain thenables, not just native Promises", async () => {
    const thenable = {
      then(onFulfilled: (v: unknown) => void) {
        onFulfilled("from-thenable");
      },
    };
    expect(await awaitWithTimeout(thenable, 1000)).toBe("from-thenable");
  });
});

describe("PAGE_EVALUATE_PROMISE_TIMEOUT_MS", () => {
  it("matches the documented 30s cap", () => {
    // Pin the constant so a future drift to a smaller cap (which would
    // surprise integration tests doing real network work) is caught.
    expect(PAGE_EVALUATE_PROMISE_TIMEOUT_MS).toBe(30_000);
  });
});

// Suppress the unhandled-rejection bookkeeping that vi.useFakeTimers()
// can leak when a test exits between "rejection scheduled" and
// "rejection observed". Each test uses an explicit `.catch(...)` handler
// so this is just defensive.
beforeEach(() => {
  // No-op: kept for future stub points (e.g. console.warn spy).
});
afterEach(() => {
  vi.useRealTimers();
});

describe("isElementActionAllowed — execute_action per-element gate", () => {
  it("permits any action when the element declares no action set", () => {
    expect(isElementActionAllowed([], "hoverClick")).toBe(true);
    expect(isElementActionAllowed([], "type")).toBe(true);
  });

  it("permits an action that is explicitly advertised", () => {
    expect(isElementActionAllowed(["click", "focus"], "click")).toBe(true);
  });

  it("rejects an action not in a non-empty declared set", () => {
    expect(isElementActionAllowed(["click", "focus"], "type")).toBe(false);
    expect(isElementActionAllowed(["focus", "blur"], "click")).toBe(false);
  });

  it("exempts hoverClick wherever click is advertised (click-variant) — the regression", () => {
    // A hover-gated toolbar button (e.g. ZoneHoverActions "Send to window")
    // advertises click but not hoverClick; hoverClick must still be allowed so
    // it reaches actionExecutor.performHoverClick instead of being rejected
    // pre-dispatch (mirrors the runner Rust is_action_advertised exemption).
    expect(isElementActionAllowed(["focus", "blur", "click", "hover", "middleClick"], "hoverClick")).toBe(
      true,
    );
  });

  it("does NOT exempt hoverClick when click is absent", () => {
    expect(isElementActionAllowed(["focus", "blur", "hover"], "hoverClick")).toBe(false);
  });
});
