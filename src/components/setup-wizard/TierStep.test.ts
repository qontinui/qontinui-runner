/**
 * R3 — the Tier step must never become permanently dead.
 *
 * `busy` disables all three tier cards, the Retry button and the "I'll decide
 * later" link. It used to be cleared ONLY inside the `catch` arm, so an
 * `invoke("set_runner_tier")` that never settles left the whole screen
 * disabled forever with no message and no escape. That is not theoretical:
 * two framework paths drop the response outright rather than rejecting —
 * an `__TAURI_INVOKE_KEY__` mismatch returns before the `InvokeResolver` is
 * constructed (`tauri-2.11.1/src/webview/mod.rs:1747`), which this app can hit
 * because it destroys and rebuilds the main window (`webview_recovery.rs`), and
 * a panic inside the command drops the resolver without resolve/reject (the
 * workspace profile does not set `panic = "abort"`).
 *
 * The component itself needs jsdom to render, which this repo's vitest setup
 * (`environment: "node"`) deliberately does not have, so the novel logic — the
 * timeout race — is exported and tested directly. The `finally` that clears
 * `busy` is a structural guarantee in `select()`.
 */

import { describe, expect, it, vi } from "vitest";

import { TierTimeoutError, withTierTimeout } from "./TierStep";

describe("withTierTimeout", () => {
  it("passes a resolved value straight through", async () => {
    await expect(withTierTimeout(Promise.resolve("ok"), 1000)).resolves.toBe("ok");
  });

  it("passes a rejection straight through (a real command error is not a timeout)", async () => {
    await expect(withTierTimeout(Promise.reject(new Error("boom")), 1000)).rejects.toThrow("boom");
  });

  it("rejects with TierTimeoutError when the invoke NEVER settles", async () => {
    vi.useFakeTimers();
    try {
      // The exact hazard: a promise that neither resolves nor rejects.
      const never = new Promise<never>(() => {});
      const raced = withTierTimeout(never, 15_000);
      const assertion = expect(raced).rejects.toBeInstanceOf(TierTimeoutError);
      await vi.advanceTimersByTimeAsync(15_000);
      await assertion;
    } finally {
      vi.useRealTimers();
    }
  });

  it("says the tier was NOT changed, so the message can never read as a silent downgrade", async () => {
    vi.useFakeTimers();
    try {
      const raced = withTierTimeout(new Promise<never>(() => {}), 15_000);
      const assertion = expect(raced).rejects.toThrow(/tier was NOT changed/);
      await vi.advanceTimersByTimeAsync(15_000);
      await assertion;
    } finally {
      vi.useRealTimers();
    }
  });

  it("clears the timer once the invoke settles, so no stray rejection follows", async () => {
    vi.useFakeTimers();
    try {
      const clearSpy = vi.spyOn(globalThis, "clearTimeout");
      await withTierTimeout(Promise.resolve("ok"), 15_000);
      expect(clearSpy).toHaveBeenCalled();
      clearSpy.mockRestore();
    } finally {
      vi.useRealTimers();
    }
  });
});
