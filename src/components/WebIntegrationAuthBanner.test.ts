/**
 * Tests for the pure visibility/signature logic that drives
 * WebIntegrationAuthBanner. The banner itself is a thin wrapper that
 * delegates the "should I render?" decision to {@link shouldShowAuthBanner}
 * — locking that logic down here keeps the UX contract honest without
 * needing a DOM testing harness (this repo is `environment: "node"` and
 * doesn't bundle jsdom).
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import {
  makeRePairClickHandler,
  RE_PAIR_CTA_GRACE_MS,
  shouldShowAuthBanner,
  shouldShowRePairCta,
  statusSignature,
  type AuthBannerStatus,
  type RePairCtaInputs,
} from "./web-integration-banner-logic";

const baseFreshInstall: AuthBannerStatus = {
  enabled: true,
  runnerTokenMasked: "",
  registrationError: null,
};

describe("shouldShowAuthBanner", () => {
  it("hides when status has not loaded yet", () => {
    expect(shouldShowAuthBanner(null, false, null)).toBe(false);
  });

  it("shows on a fresh install (enabled, not paired, never dismissed)", () => {
    expect(shouldShowAuthBanner(baseFreshInstall, false, null)).toBe(true);
  });

  it("hides when the user has opted out (enabled === false)", () => {
    // Respect explicit opt-out — never bug them again until they re-enable.
    expect(shouldShowAuthBanner({ ...baseFreshInstall, enabled: false }, false, null)).toBe(false);
  });

  it("hides when the runner is paired (device JWT present), even with an empty runner_token", () => {
    // Regression: post-Cognito a paired runner has an EMPTY runner_token but a
    // valid device JWT. The banner must NOT show — it previously keyed off
    // runner_token and so nagged every Cognito-/pair-code-paired runner.
    expect(shouldShowAuthBanner(baseFreshInstall, true, null)).toBe(false);
  });

  it("hides when the user dismissed for the current status signature", () => {
    const sig = statusSignature(baseFreshInstall, false);
    expect(shouldShowAuthBanner(baseFreshInstall, false, sig)).toBe(false);
  });

  it("re-shows when status changes after a dismissal (e.g. new auth error)", () => {
    // User dismissed a clean "needs authorization" banner. A subsequent
    // registration error must resurface it — they need to know the runner is
    // now actively failing to authenticate, not just unpaired.
    const dismissedSig = statusSignature(baseFreshInstall, false);
    const withError: AuthBannerStatus = {
      ...baseFreshInstall,
      registrationError: "401 Unauthorized",
    };
    expect(shouldShowAuthBanner(withError, false, dismissedSig)).toBe(true);
  });

  it("re-shows after the device un-pairs (device JWT cleared)", () => {
    // Banner was dismissed while paired (device JWT present, so it was hidden
    // anyway). The JWT is now gone — the signature differs, so it reappears.
    const dismissedWhilePaired = statusSignature(baseFreshInstall, true);
    expect(shouldShowAuthBanner(baseFreshInstall, false, dismissedWhilePaired)).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// Phase 4 (unified-devices migration): re-pair CTA
// ---------------------------------------------------------------------------

const baseMigrationInputs: RePairCtaInputs = {
  tier: "qontinui_account",
  runnerTokenPresent: true,
  deviceJwtPresent: false,
  firstDetectedAt: null,
  now: 1_000_000,
};

describe("shouldShowRePairCta (post-upgrade migration)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(1_000_000));
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("does_not_render_re_pair_cta_immediately_after_boot", () => {
    // Fresh mount: the migration state JUST got detected. The grace
    // period hasn't elapsed yet, so the CTA stays hidden — we don't
    // want to flash a "re-pair required" message on every boot while
    // the refresher is doing its job.
    const detectedAt = Date.now();
    expect(
      shouldShowRePairCta({
        ...baseMigrationInputs,
        firstDetectedAt: detectedAt,
        now: detectedAt,
      }),
    ).toBe(false);
  });

  it("renders_re_pair_cta_after_5min_timeout", () => {
    // Same migration state, but `RE_PAIR_CTA_GRACE_MS` has elapsed —
    // the refresher hasn't healed the gap, so surface the manual CTA.
    const detectedAt = Date.now();
    vi.advanceTimersByTime(RE_PAIR_CTA_GRACE_MS);
    expect(
      shouldShowRePairCta({
        ...baseMigrationInputs,
        firstDetectedAt: detectedAt,
        now: Date.now(),
      }),
    ).toBe(true);
  });

  it("re_pair_cta_calls_kick_refresher_on_click", async () => {
    // The component delegates the click handler to `makeRePairClickHandler`
    // so the click → invoke contract can be locked down in node without
    // a DOM. Wire a spy into the invoker slot, fire the handler, assert.
    const invokeSpy = vi.fn().mockResolvedValue(undefined);
    const click = makeRePairClickHandler(invokeSpy);
    await click();
    expect(invokeSpy).toHaveBeenCalledTimes(1);
    expect(invokeSpy).toHaveBeenCalledWith("kick_device_jwt_refresher_cmd", {});
  });

  it("hides re-pair CTA on Tier 0/1 even when the rest of the state qualifies", () => {
    const detectedAt = Date.now();
    vi.advanceTimersByTime(RE_PAIR_CTA_GRACE_MS);
    expect(
      shouldShowRePairCta({
        ...baseMigrationInputs,
        tier: "local",
        firstDetectedAt: detectedAt,
        now: Date.now(),
      }),
    ).toBe(false);
  });

  it("hides re-pair CTA once a device-JWT lands", () => {
    const detectedAt = Date.now();
    vi.advanceTimersByTime(RE_PAIR_CTA_GRACE_MS);
    expect(
      shouldShowRePairCta({
        ...baseMigrationInputs,
        deviceJwtPresent: true,
        firstDetectedAt: detectedAt,
        now: Date.now(),
      }),
    ).toBe(false);
  });
});

describe("statusSignature", () => {
  it("returns an empty string for null status", () => {
    expect(statusSignature(null, false)).toBe("");
  });

  it("differentiates paired from unpaired even at the same enabled flag", () => {
    const a = statusSignature(baseFreshInstall, false);
    const b = statusSignature(baseFreshInstall, true);
    expect(a).not.toBe(b);
  });

  it("differentiates registration errors from a clean state", () => {
    const a = statusSignature(baseFreshInstall, false);
    const b = statusSignature({ ...baseFreshInstall, registrationError: "401" }, false);
    expect(a).not.toBe(b);
  });

  it("is stable across calls with identical input", () => {
    expect(statusSignature(baseFreshInstall, false)).toBe(
      statusSignature({ ...baseFreshInstall }, false),
    );
  });
});
