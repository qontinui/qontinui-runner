import { describe, expect, it } from "vitest";

import { buildFixPrompt, isFixable, shouldOfferFix } from "./fixThis";
import type { HealthLevel, ProjectSnapshot } from "./types";

function snap(
  level: HealthLevel,
  reason: string,
  processes: ProjectSnapshot["processes"] = [],
): ProjectSnapshot {
  return {
    project: {
      id: "p1",
      name: "Papa's Pizzeria",
      path: "D:/work/pizzeria",
    } as ProjectSnapshot["project"],
    processes,
    liveSessions: [],
    recentSessions: [],
    questions: [],
    health: { level, reason, failingProcesses: [] },
  };
}

describe("isFixable / shouldOfferFix", () => {
  it("offers the fix only on the two levels that mean something is wrong", () => {
    expect(isFixable("amber")).toBe(true);
    expect(isFixable("red")).toBe(true);
    expect(isFixable("green")).toBe(false);
  });

  it("does NOT offer a fix on `unknown`", () => {
    // `unknown` is grey, not amber: a project declaring no processes is not
    // degraded, there is simply nothing to be healthy about. Offering "Fix
    // this" would send an agent hunting a failure nobody observed.
    expect(isFixable("unknown")).toBe(false);
    expect(shouldOfferFix(snap("unknown", ""))).toBe(false);
  });

  it("does NOT offer a fix while the snapshot is still loading", () => {
    // Pending is not healthy AND not broken. An action that appears mid-load
    // and then vanishes is worse than one that arrives a beat late.
    expect(shouldOfferFix(undefined)).toBe(false);
  });
});

describe("buildFixPrompt", () => {
  it("attaches the observed error text, which is the whole point of the button", () => {
    const prompt = buildFixPrompt({
      projectName: "Papa's Pizzeria",
      projectPath: "D:/work/pizzeria",
      snapshot: snap("red", "The website stopped: EADDRINUSE on port 3000.", [
        { id: "web", name: "Website", state: "stopped", cwd: "D:/work/pizzeria" },
      ]),
    });

    expect(prompt).not.toBeNull();
    // The user never has to describe the failure themselves.
    expect(prompt).toContain("EADDRINUSE on port 3000");
    expect(prompt).toContain("Papa's Pizzeria");
    expect(prompt).toContain("D:/work/pizzeria");
    // The concrete handle on the failure, not just the level.
    expect(prompt).toContain("Website (stopped)");
    // Diagnose before changing — a symptom-only prompt gets the thing
    // restarted rather than understood.
    expect(prompt).toMatch(/why this is failing before changing anything/i);
  });

  it("omits the process block when everything is running", () => {
    const prompt = buildFixPrompt({
      projectName: "Papa's Pizzeria",
      snapshot: snap("amber", "The site is up but returning 500s.", [
        { id: "web", name: "Website", state: "running", cwd: "D:/work/pizzeria" },
      ]),
    });
    expect(prompt).toContain("returning 500s");
    expect(prompt).not.toContain("Processes not running");
  });

  it("says so rather than fabricating a cause when no reason was recorded", () => {
    const prompt = buildFixPrompt({
      projectName: "Papa's Pizzeria",
      snapshot: snap("red", "   "),
    });
    expect(prompt).toContain("no reason text was recorded");
  });

  it("truncates a huge reason instead of pasting an unbounded stderr dump", () => {
    const prompt = buildFixPrompt({
      projectName: "Papa's Pizzeria",
      snapshot: snap("red", "E".repeat(5000)),
    })!;
    expect(prompt).toContain("(truncated)");
    expect(prompt.length).toBeLessThan(2500);
  });

  it("returns null when the project is not in a fixable state", () => {
    // The caller must then not render the button: a "Fix this" that opens an
    // agent with an empty prompt is precisely the blank text box §8.4 avoids.
    expect(buildFixPrompt({ projectName: "x", snapshot: snap("green", "All good") })).toBeNull();
    expect(buildFixPrompt({ projectName: "x", snapshot: undefined })).toBeNull();
  });
});
