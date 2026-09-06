/**
 * Pure-helper tests for `SessionBriefingPanel`.
 *
 * The runner's vitest config uses `environment: "node"` (no jsdom) — see
 * `LockYieldPolicySettings.test.tsx` for the precedent — so the panel's honesty
 * rules are exercised through the exported helpers rather than by rendering.
 */

import { describe, it, expect } from "vitest";

import {
  describePlanCaptureClause,
  formatDocumentVersion,
  formatLastConfirmed,
  provenanceClasses,
  PROVENANCE_BUILTIN,
} from "./sessionBriefingHelpers";

describe("provenanceClasses", () => {
  it("gives coord and cached distinct, non-error colours", () => {
    expect(provenanceClasses("coord")).toContain("emerald");
    expect(provenanceClasses("cached")).toContain("amber");
  });

  it("renders builtin as neutral, never as an error", () => {
    const classes = provenanceClasses(PROVENANCE_BUILTIN);
    expect(classes).toContain("muted");
    expect(classes).not.toContain("destructive");
    expect(classes).not.toContain("red");
  });

  it("falls back to the neutral tone for an unrecognised token", () => {
    expect(provenanceClasses("something-new")).toBe(provenanceClasses(PROVENANCE_BUILTIN));
  });
});

describe("formatDocumentVersion", () => {
  it("prints a real version", () => {
    expect(formatDocumentVersion(7, "coord")).toBe("v7");
    expect(formatDocumentVersion(1, "cached")).toBe("v1");
  });

  it("names the compiled-in fallback when the builtin was rendered", () => {
    expect(formatDocumentVersion(null, PROVENANCE_BUILTIN)).toBe("— (compiled-in fallback)");
  });

  it("says UNKNOWN — not 'compiled-in' — when a document body has no version", () => {
    // The block DID come from a document; the runner just cannot state which
    // generation of it. Blaming the fallback here would be a different claim.
    expect(formatDocumentVersion(null, "coord")).toBe("— (unknown)");
    expect(formatDocumentVersion(undefined, "cached")).toBe("— (unknown)");
  });

  it("treats version 0 as UNKNOWN, matching fleet_policy_poller", () => {
    // `0` is what a coord list row with no `current_version` and an older
    // build's cache entry both decode to. Printing `v0` would present a
    // missing value as a real version.
    expect(formatDocumentVersion(0, "coord")).toBe("— (unknown)");
    expect(formatDocumentVersion(0, PROVENANCE_BUILTIN)).toBe("— (compiled-in fallback)");
  });
});

describe("formatLastConfirmed", () => {
  it("prints the stamp verbatim", () => {
    expect(formatLastConfirmed("2026-08-24T16:30:19.610575+00:00", "coord")).toBe(
      "2026-08-24T16:30:19.610575+00:00",
    );
  });

  it("says never for the compiled-in fallback", () => {
    expect(formatLastConfirmed(null, PROVENANCE_BUILTIN)).toBe("never (compiled-in fallback)");
  });

  it("says unknown for a document body with no stamp", () => {
    expect(formatLastConfirmed(null, "cached")).toBe("unknown");
  });

  it("treats an empty or blank stamp as an absence, not as a timestamp", () => {
    // The serde default for a cache entry written before the field existed.
    expect(formatLastConfirmed("", "coord")).toBe("unknown");
    expect(formatLastConfirmed("   ", "coord")).toBe("unknown");
    expect(formatLastConfirmed("", PROVENANCE_BUILTIN)).toBe("never (compiled-in fallback)");
  });
});

describe("describePlanCaptureClause", () => {
  it("names the dial in all four arms", () => {
    for (const provenance of ["coord", PROVENANCE_BUILTIN]) {
      expect(describePlanCaptureClause(true, provenance)).toContain("dial");
      expect(describePlanCaptureClause(false, provenance)).toContain("dial");
    }
  });

  it("says plainly whether the text is injected", () => {
    expect(describePlanCaptureClause(true, "coord")).toMatch(/^Included/);
    expect(describePlanCaptureClause(false, "coord")).toMatch(/^Omitted/);
    expect(describePlanCaptureClause(false, "coord")).toContain("not injected");
    expect(describePlanCaptureClause(true, PROVENANCE_BUILTIN)).toMatch(/^Included/);
    expect(describePlanCaptureClause(false, PROVENANCE_BUILTIN)).toMatch(/^Omitted/);
  });

  it("does not end the omitted arm at the dial", () => {
    // The whole point of reporting the document state in the omitted arm is
    // that "omitted" alone cannot answer "is my edit even cached?".
    expect(describePlanCaptureClause(false, "coord")).toContain("document");
    expect(describePlanCaptureClause(false, PROVENANCE_BUILTIN)).toContain("document");
  });

  it("claims a cached document ONLY when one was rendered", () => {
    // The sentence sits next to a provenance badge built from the same
    // reading. Saying "cached and ready" beside a `builtin-fallback` badge
    // answers the operator's question wrongly, which is worse than the
    // original bug of not answering it at all.
    expect(describePlanCaptureClause(false, "coord")).toContain("cached and ready");
    expect(describePlanCaptureClause(false, "cached")).toContain("cached and ready");
    expect(describePlanCaptureClause(false, PROVENANCE_BUILTIN)).not.toContain("cached and ready");
  });

  it("names the compiled-in text on the builtin arms, in both dial positions", () => {
    // `builtin` provenance means no coord body was rendered for the clause —
    // including the REJECTED case, which the route also reports as `builtin`.
    expect(describePlanCaptureClause(true, PROVENANCE_BUILTIN)).toContain("compiled-in");
    expect(describePlanCaptureClause(false, PROVENANCE_BUILTIN)).toContain("compiled-in");
    expect(describePlanCaptureClause(true, "coord")).not.toContain("compiled-in");
  });
});
