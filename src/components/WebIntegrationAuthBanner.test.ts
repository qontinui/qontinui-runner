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
  credentialDarkPresentation,
  makeRePairClickHandler,
  normalizeCredentialDarkSignal,
  RE_PAIR_CTA_GRACE_MS,
  shouldShowAuthBanner,
  shouldShowRePairCta,
  statusSignature,
  type AuthBannerStatus,
  type CredentialDarkSignal,
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

// ---------------------------------------------------------------------------
// Coord-credential posture (plan 2026-09-12, Phase 2)
//
// The defect: a runner booted holding an expired coord device JWT, `/health`
// read healthy, no banner fired, and every session it spawned silently had no
// coord access. The banner is the user-visible half of the fix, so the rules
// that could re-silence it — dismissal, and a signature that does not move
// when the posture does — are pinned here.
// ---------------------------------------------------------------------------

const darkExpired: CredentialDarkSignal = {
  dark: true,
  cause: "expired",
  message:
    "the coord credential this runner holds has EXPIRED. Sessions spawned now have no coord access.",
  cta: "retry_refresh",
  since: 1_757_649_240, // 2026-09-12T03:54:00Z
};

const stubTime = () => "03:54";

describe("credential-dark banner visibility", () => {
  it("shows while dark even though the user dismissed the CURRENT signature", () => {
    // A dismissed credential banner is a silent runner again — the exact state
    // the plan exists to end. Dismissal may not win over `dark`.
    const sig = statusSignature(baseFreshInstall, false, darkExpired);
    expect(shouldShowAuthBanner(baseFreshInstall, false, sig, darkExpired)).toBe(true);
  });

  it("shows while dark even when the runner is PAIRED and the user opted out", () => {
    // Both of these normally hide the banner, and both say nothing useful
    // here: a paired runner holding an expired credential has a device JWT and
    // is still spawning sessions with no coord access.
    const paired: AuthBannerStatus = { ...baseFreshInstall, enabled: false };
    expect(shouldShowAuthBanner(paired, true, null, darkExpired)).toBe(true);
  });

  it("shows while dark even before status has loaded", () => {
    // The credential signal arrives from the refresher's BOOT pass, which can
    // beat `get_web_integration_status`. A null status must not swallow it.
    expect(shouldShowAuthBanner(null, false, null, darkExpired)).toBe(true);
  });

  it("CLEARS on dark:false — a stale banner after recovery teaches users to ignore it", () => {
    const recovered: CredentialDarkSignal = {
      dark: false,
      cause: "recovered",
      message: "Coord access restored — this runner's credential is live again.",
      cta: null,
      since: null,
    };
    // Recovered + paired + nothing else wrong → nothing to show.
    expect(shouldShowAuthBanner(baseFreshInstall, true, null, recovered)).toBe(false);
    // …and the pre-existing rules are untouched by the recovered signal.
    expect(shouldShowAuthBanner(baseFreshInstall, false, null, recovered)).toBe(true);
  });

  it("keeps the historical behaviour when no credential signal exists", () => {
    // Every pre-existing call site passes no fourth argument.
    expect(shouldShowAuthBanner(baseFreshInstall, false, null)).toBe(true);
    expect(shouldShowAuthBanner(baseFreshInstall, true, null)).toBe(false);
  });
});

describe("statusSignature carries the credential posture", () => {
  it("differs between healthy and dark, so a dismissed banner RESURFACES", () => {
    // The hole this closes: the signature was
    // `enabled | deviceJwtPresent | registrationError` — none of which moves
    // when the coord credential expires. A banner dismissed while healthy
    // stayed dismissed through the transition that mattered.
    const dismissedWhileHealthy = statusSignature(baseFreshInstall, false, null);
    expect(statusSignature(baseFreshInstall, false, darkExpired)).not.toBe(dismissedWhileHealthy);
    expect(shouldShowAuthBanner(baseFreshInstall, false, dismissedWhileHealthy, darkExpired)).toBe(
      true,
    );
  });

  it("differs PER CAUSE, so expired -> unrefreshable resurfaces too", () => {
    const unrefreshable: CredentialDarkSignal = { ...darkExpired, cause: "unrefreshable" };
    const dismissedAtExpired = statusSignature(baseFreshInstall, false, darkExpired);
    expect(statusSignature(baseFreshInstall, false, unrefreshable)).not.toBe(dismissedAtExpired);
  });

  it("is unchanged for callers that pass no credential signal", () => {
    expect(statusSignature(baseFreshInstall, false)).toBe(
      statusSignature(baseFreshInstall, false, null),
    );
  });
});

describe("credentialDarkPresentation", () => {
  it("renders the cause, the time it started, and the CTA that cause needs", () => {
    const p = credentialDarkPresentation(darkExpired, stubTime);
    expect(p.title).toBe("Coord credential expired");
    expect(p.body).toContain("Since 03:54");
    expect(p.body).toContain("no coord access");
    expect(p.ctaLabel).toBe("Retry refresh now");
    // "Retry refresh now" needs NO new command — it kicks the existing
    // device-JWT refresher.
    expect(p.ctaAction).toBe("kick_refresher");
  });

  it("names each cause distinctly", () => {
    const titleFor = (cause: string) =>
      credentialDarkPresentation({ ...darkExpired, cause }, stubTime).title;
    const titles = [
      titleFor("cognito_hard"),
      titleFor("expired"),
      titleFor("unrefreshable"),
      titleFor("absent"),
      titleFor("upstream_401"),
    ];
    expect(new Set(titles).size).toBe(titles.length);
    expect(titleFor("unrefreshable")).toContain("automatic refresh failed");
    expect(titleFor("upstream_401")).toContain("rejecting");
  });

  it("routes the HARD Cognito cause to the interactive sign-in", () => {
    const p = credentialDarkPresentation(
      { ...darkExpired, cause: "cognito_hard", cta: "sign_in" },
      stubTime,
    );
    expect(p.ctaAction).toBe("cognito_sign_in");
    expect(p.ctaLabel).toBe("Sign in");
  });

  it("omits the time prefix when the runner did not say since when", () => {
    const p = credentialDarkPresentation({ ...darkExpired, since: null }, stubTime);
    expect(p.body.startsWith("Since")).toBe(false);
    expect(p.body).toContain("no coord access");
  });

  it("still renders a banner for a cause this frontend does not know", () => {
    // A newer runner binary may emit a cause this build has never heard of.
    // Falling through to nothing would re-create the silence.
    const p = credentialDarkPresentation({ ...darkExpired, cause: "some_future_cause" }, stubTime);
    expect(p.title).toBe("Coord credential problem");
    expect(p.body).toContain("no coord access");
  });
});

describe("normalizeCredentialDarkSignal", () => {
  it("accepts the widened payload verbatim", () => {
    expect(normalizeCredentialDarkSignal({ ...darkExpired })).toEqual(darkExpired);
  });

  it("upgrades a LEGACY {dark, message} payload from an older runner build", () => {
    // The event is emitted by the runner BINARY; this frontend can be running
    // against a build that predates the widened payload. A runner that says
    // "I am dark" in the old shape is exactly as dark.
    const legacy = normalizeCredentialDarkSignal({ dark: true, message: "paused" });
    expect(legacy).toEqual({
      dark: true,
      cause: "cognito_hard",
      message: "paused",
      cta: "sign_in",
      since: null,
    });
    expect(shouldShowAuthBanner(baseFreshInstall, true, null, legacy)).toBe(true);
  });

  it("drops a payload that is not a credential signal at all", () => {
    expect(normalizeCredentialDarkSignal(null)).toBeNull();
    expect(normalizeCredentialDarkSignal("dark")).toBeNull();
    expect(normalizeCredentialDarkSignal({ message: "no dark flag" })).toBeNull();
  });
});
