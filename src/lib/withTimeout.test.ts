/**
 * Unit tests for `withTimeout` — the defensive timeout wrapper used by
 * AuthProvider against the initial-mount Tauri IPC hang. The runner's
 * vitest config is `environment: "node"`, so we exercise the helper
 * purely (no React, no Tauri mock).
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

import { withTimeout } from "./withTimeout";

describe("withTimeout", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("resolves with the inner value when the promise settles before the timeout", async () => {
    const inner = Promise.resolve("ok");
    await expect(withTimeout(inner, 5000, "probe")).resolves.toBe("ok");
  });

  it("rejects with a labelled error when the inner promise never settles before the timeout", async () => {
    // Inner promise that never resolves — simulates the Tauri IPC hang.
    const hanging = new Promise<string>(() => {});
    const racing = withTimeout(hanging, 5000, "check_auth_status");
    // Pre-attach a catch handler so the race rejection is observed
    // synchronously when the fake timer fires; otherwise vitest+Node-22
    // logs a spurious PromiseRejectionHandledWarning even though the
    // rejection IS handled below.
    const captured = racing.catch((e: unknown) => e as Error);
    await vi.advanceTimersByTimeAsync(5001);
    const err = await captured;
    expect(err).toBeInstanceOf(Error);
    expect(err.message).toBe("check_auth_status timed out after 5000ms");
  });

  it("propagates the inner promise's rejection unchanged when it rejects before the timeout", async () => {
    const failing = Promise.reject(new Error("backend went away"));
    await expect(withTimeout(failing, 5000, "probe")).rejects.toThrow("backend went away");
  });

  it("does not leak a pending setTimeout after the inner promise resolves", async () => {
    // Tripwire: if `.finally(clearTimeout)` ever regresses, the fake-timer
    // queue would still have a pending callback. Track active timer count.
    expect(vi.getTimerCount()).toBe(0);
    const inner = Promise.resolve(42);
    const result = await withTimeout(inner, 5000, "probe");
    expect(result).toBe(42);
    // Flush any microtasks in flight (the .finally on the Promise chain).
    await vi.advanceTimersByTimeAsync(0);
    expect(vi.getTimerCount()).toBe(0);
  });
});
