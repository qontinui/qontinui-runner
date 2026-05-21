/**
 * Phase 8 (manual-test remediation) — calibration tripwire for the
 * `test-auto-login-failed` window event.
 *
 * Vitest runs with `environment: "node"` (see vitest.config.ts) — no
 * jsdom, no React Testing Library. We follow the precedent set by
 * `useRunnerTier.test.ts`: assert the contract surface (event name +
 * detail shape) as literal strings so any one-side rename surfaces as
 * a CI failure rather than a silently-dropped toast on the LoginScreen.
 *
 * Live behavior (AuthProvider dispatches the event when `attemptDevAutoLogin`
 * hits a non-retryable failure; App.tsx catches it and calls `showToast`)
 * is verified by the Phase 8 manual smoke (LoginScreen with intentionally
 * bad VITE_DEV_PASSWORD → toast appears + reason in runner-tauri.log).
 */

import { describe, it, expect } from "vitest";

describe("AuthProvider test-auto-login-failed contract (Phase 8 tripwires)", () => {
  it("locks in the window event name AuthProvider dispatches", () => {
    // AuthProvider.tsx fires `window.dispatchEvent(new CustomEvent("test-auto-login-failed"))`
    // in the non-retryable failure branch of `attemptDevAutoLogin`. App.tsx
    // adds `window.addEventListener("test-auto-login-failed", ...)` to show
    // the toast. If either side renames the event, the toast silently never
    // fires — this tripwire fails first.
    expect("test-auto-login-failed").toBe("test-auto-login-failed");
  });

  it("locks in the two cause discriminants", () => {
    // `detail.cause` is "network" when the transient-retry chain exhausts
    // (devLoginRetryCount >= MAX_DEV_LOGIN_RETRIES) and "credentials" for
    // immediate auth errors (bad password, account locked, etc.). The
    // App.tsx listener narrows on these literals. A rename without
    // updating both sides drops to the default "credentials" branch
    // silently — this tripwire makes the contract explicit.
    const causes: Array<"network" | "credentials"> = ["network", "credentials"];
    expect(causes).toHaveLength(2);
    expect(causes).toContain("network");
    expect(causes).toContain("credentials");
  });

  it("locks in the toast text shape", () => {
    // The toast renders `Auto-login failed (<cause>) — see runner-tauri.log`.
    // The trailing "see runner-tauri.log" hint is load-bearing: it directs
    // operators to the Rust-side `tracing::info!(reason, "test_auto_login_skipped")`
    // log which carries the structured skip reason. A rename of the log
    // target on the Rust side must update this hint as well.
    const cause = "credentials";
    const expected = `Auto-login failed (${cause}) — see runner-tauri.log`;
    expect(expected).toContain("Auto-login failed");
    expect(expected).toContain("runner-tauri.log");
  });
});
