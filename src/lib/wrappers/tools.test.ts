/**
 * Tool eligibility mirrors Rust `WrapperManager::spawn`, which refuses every
 * manifest transport except `api` (plan 2026-08-23-single-source-derived-facts
 * item 10 review follow-up).
 */

import { describe, expect, it } from "vitest";
import { isWrapperToolEligible } from "./tools";
import type { InstalledWrapper, WrapperTransport } from "./types";

function wrapper(transport: WrapperTransport): InstalledWrapper {
  return {
    id: `w-${transport}`,
    package_name: "@acme/w",
    version: "1.0.0",
    manifest: { manifestVersion: 1, id: `w-${transport}`, displayName: "W", transport },
    actions: [],
  };
}

describe("isWrapperToolEligible", () => {
  it("admits an api-transport wrapper (the only one spawn accepts)", () => {
    expect(isWrapperToolEligible(wrapper("api"))).toBe(true);
  });

  it.each(["headless", "headed", "live"] as const)(
    "refuses a %s-transport wrapper, whose every dispatch spawn would reject",
    (t) => {
      expect(isWrapperToolEligible(wrapper(t))).toBe(false);
    },
  );

  it("refuses a wrapper with no manifest rather than guessing", () => {
    const w = { ...wrapper("api"), manifest: undefined } as unknown as InstalledWrapper;
    expect(isWrapperToolEligible(w)).toBe(false);
  });
});
