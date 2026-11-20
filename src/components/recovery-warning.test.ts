/**
 * The "no automatic recovery is left" warning's pure logic (plan
 * 2026-11-15-headless-runner-recovers-its-coord-credential-without-a-human,
 * E2): parsing the `get_recovery_warning` answer, and when the banner shows it.
 */
import { describe, it, expect } from "vitest";
import {
  DARK_SOURCE_POSTURE,
  normalizeRecoveryWarning,
  recoveryWarningSignature,
  shouldShowRecoveryWarning,
  type CredentialDarkSignal,
} from "./web-integration-banner-logic";

const WIRE = {
  missing: ["no_cognito_session", "no_device_machine_key"],
  message: "This runner holds neither a Cognito session nor a device machine key.",
};

const dark: CredentialDarkSignal = {
  source: DARK_SOURCE_POSTURE,
  dark: true,
  cause: "unrefreshable",
  message: "x",
  cta: "sign_in",
  since: null,
  tenantId: null,
  pinnedTenant: false,
};

describe("normalizeRecoveryWarning", () => {
  it("parses the Rust wire shape", () => {
    expect(normalizeRecoveryWarning(WIRE)).toEqual(WIRE);
  });

  it("reads null, junk and an empty message as no warning", () => {
    expect(normalizeRecoveryWarning(null)).toBeNull();
    expect(normalizeRecoveryWarning("x")).toBeNull();
    expect(normalizeRecoveryWarning({ missing: [], message: "  " })).toBeNull();
    expect(normalizeRecoveryWarning({ missing: ["a"] })).toBeNull();
  });

  it("drops non-string tokens rather than the warning", () => {
    expect(normalizeRecoveryWarning({ missing: ["a", 3], message: "m" })).toEqual({
      missing: ["a"],
      message: "m",
    });
  });
});

describe("shouldShowRecoveryWarning", () => {
  const w = normalizeRecoveryWarning(WIRE)!;

  it("shows a warning nobody dismissed", () => {
    expect(shouldShowRecoveryWarning(w, null, null)).toBe(true);
  });

  it("shows nothing without a warning", () => {
    expect(shouldShowRecoveryWarning(null, null, null)).toBe(false);
  });

  it("yields to the credential-dark banner", () => {
    expect(shouldShowRecoveryWarning(w, dark, null)).toBe(false);
    expect(shouldShowRecoveryWarning(w, { ...dark, dark: false }, null)).toBe(true);
  });

  it("stays dismissed until what is missing changes", () => {
    const sig = recoveryWarningSignature(w);
    expect(shouldShowRecoveryWarning(w, null, sig)).toBe(false);
    const changed = { ...w, missing: ["no_cognito_session"] };
    expect(shouldShowRecoveryWarning(changed, null, sig)).toBe(true);
  });
});
