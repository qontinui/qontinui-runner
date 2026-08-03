/**
 * Pure-logic tests for the Projects step's advance-button label.
 *
 * The runner's vitest config uses `environment: "node"` (no jsdom/RTL), so we
 * follow the repo precedent (`GithubRepoPicker.test.ts`) and exercise the
 * exported pure helper rather than rendering JSX.
 *
 * The regression these guard: the button used to carry `disabled={!scanned}`,
 * where `scanned` only flips true after a successful local scan or clone. A user
 * who opened "Clone from GitHub" and hit the sign-in wall saw a button labelled
 * "Skip" that could not be clicked — a hard dead end in first-run onboarding,
 * escapable only by switching to local mode and scanning an unrelated folder.
 * The button is now never disabled, and "Skip" is what it says when nothing is
 * selected.
 */

import { describe, it, expect } from "vitest";

import { projectsAdvanceLabel } from "./ProjectStep";

describe("projectsAdvanceLabel", () => {
  it("offers Skip when nothing is selected — the step is optional", () => {
    expect(projectsAdvanceLabel(0)).toBe("Skip");
  });

  it("uses the singular for exactly one project", () => {
    expect(projectsAdvanceLabel(1)).toBe("Continue with 1 project");
  });

  it("uses the plural beyond one", () => {
    expect(projectsAdvanceLabel(2)).toBe("Continue with 2 projects");
    expect(projectsAdvanceLabel(17)).toBe("Continue with 17 projects");
  });

  it("always yields a non-empty label, so the button is never blank", () => {
    for (const n of [0, 1, 2, 5, 100]) {
      expect(projectsAdvanceLabel(n).length).toBeGreaterThan(0);
    }
  });
});
