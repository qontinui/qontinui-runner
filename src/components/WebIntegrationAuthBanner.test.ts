/**
 * Tests for the pure visibility/signature logic that drives
 * WebIntegrationAuthBanner. The banner itself is a thin wrapper that
 * delegates the "should I render?" decision to {@link shouldShowAuthBanner}
 * — locking that logic down here keeps the UX contract honest without
 * needing a DOM testing harness (this repo is `environment: "node"` and
 * doesn't bundle jsdom).
 */

import { describe, it, expect } from "vitest";
import {
  shouldShowAuthBanner,
  statusSignature,
  type AuthBannerStatus,
} from "./web-integration-banner-logic";

const baseFreshInstall: AuthBannerStatus = {
  enabled: true,
  runnerTokenMasked: "",
  registrationError: null,
};

describe("shouldShowAuthBanner", () => {
  it("hides when status has not loaded yet", () => {
    expect(shouldShowAuthBanner(null, null)).toBe(false);
  });

  it("shows on a fresh install (enabled, no token, never dismissed)", () => {
    expect(shouldShowAuthBanner(baseFreshInstall, null)).toBe(true);
  });

  it("hides when the user has opted out (enabled === false)", () => {
    // Respect explicit opt-out — never bug them again until they re-enable.
    expect(shouldShowAuthBanner({ ...baseFreshInstall, enabled: false }, null)).toBe(false);
  });

  it("hides when the runner token is set", () => {
    expect(
      shouldShowAuthBanner(
        { ...baseFreshInstall, runnerTokenMasked: "qontinui_runner_aaaa…bbbb" },
        null,
      ),
    ).toBe(false);
  });

  it("hides when the user dismissed for the current status signature", () => {
    const sig = statusSignature(baseFreshInstall);
    expect(shouldShowAuthBanner(baseFreshInstall, sig)).toBe(false);
  });

  it("re-shows when status changes after a dismissal (e.g. new auth error)", () => {
    // User dismissed a clean "needs token" banner. A subsequent registration
    // error must resurface the banner — they need to know the runner is now
    // actively failing to authenticate, not just unconfigured.
    const dismissedSig = statusSignature(baseFreshInstall);
    const withError: AuthBannerStatus = {
      ...baseFreshInstall,
      registrationError: "401 Unauthorized",
    };
    expect(shouldShowAuthBanner(withError, dismissedSig)).toBe(true);
  });

  it("re-shows after token gets cleared (token-revoked path)", () => {
    // Banner was dismissed when the token was set (so `shouldShowAuthBanner`
    // would have returned false anyway). Now the token has been cleared —
    // the new status' signature differs, so the banner reappears.
    const withToken: AuthBannerStatus = {
      ...baseFreshInstall,
      runnerTokenMasked: "qontinui_runner_xxxx…yyyy",
    };
    const dismissedWhenTokenSet = statusSignature(withToken);
    // Token is now empty again:
    expect(shouldShowAuthBanner(baseFreshInstall, dismissedWhenTokenSet)).toBe(true);
  });
});

describe("statusSignature", () => {
  it("returns an empty string for null status", () => {
    expect(statusSignature(null)).toBe("");
  });

  it("differentiates token-set from token-empty even at the same enabled flag", () => {
    const a = statusSignature(baseFreshInstall);
    const b = statusSignature({ ...baseFreshInstall, runnerTokenMasked: "qontinui_x…y" });
    expect(a).not.toBe(b);
  });

  it("differentiates registration errors from a clean state", () => {
    const a = statusSignature(baseFreshInstall);
    const b = statusSignature({ ...baseFreshInstall, registrationError: "401" });
    expect(a).not.toBe(b);
  });

  it("is stable across calls with identical input", () => {
    expect(statusSignature(baseFreshInstall)).toBe(statusSignature({ ...baseFreshInstall }));
  });
});
