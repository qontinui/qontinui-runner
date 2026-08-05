/**
 * Pure-helper tests for the project card's status derivation.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom — same
 * constraint as FleetHealthPanel.test.tsx), so the JSX is not renderable here.
 * The load-bearing logic is `deriveCardStatus`, tested below.
 */

import { describe, expect, it } from "vitest";

import { deriveCardStatus } from "./ProjectCard";
import type { ProjectSnapshot, ProcessState } from "./types";
import type { SavedProject } from "@/hooks/useSavedProjects";

const NOW = Date.UTC(2026, 6, 29, 12, 0, 0);
const DAY = 24 * 60 * 60 * 1000;

const project: SavedProject = {
  id: "p1",
  path: "D:/work/pizzeria",
  name: "Papa's Pizzeria",
  projectType: "node",
  manifest: "package.json",
};

function proc(state: ProcessState) {
  return { id: `proc-${state}`, name: state, state, cwd: "D:/work/pizzeria" };
}

function snapshot(over: Partial<ProjectSnapshot> = {}): ProjectSnapshot {
  return {
    project,
    processes: [],
    liveSessions: [],
    recentSessions: [],
    questions: [],
    health: { level: "unknown", reason: "", failingProcesses: [] },
    ...over,
  };
}

describe("deriveCardStatus", () => {
  it("reports pending — not 'not running' — while the snapshot is absent", () => {
    // This is the distinction the whole helper exists for. A card that says
    // "Not running" during a slow Postgres query is telling the user their
    // project is stopped when nobody has actually looked yet.
    const status = deriveCardStatus(project, undefined, NOW);
    expect(status.pending).toBe(true);
    expect(status.runningCount).toBe(0);
    expect(status.level).toBe("unknown");
  });

  it("still shows a recency line while pending, from the registry's own record", () => {
    // `lastOpenedMs` is local and always available, so the card need not blank
    // its "Worked on …" line waiting for the server-side join.
    const status = deriveCardStatus({ ...project, lastOpenedMs: NOW - 2 * DAY }, undefined, NOW);
    expect(status.pending).toBe(true);
    expect(status.worked).toBe("2 days ago");
  });

  it("prefers the snapshot's activity over the registry's last-opened stamp", () => {
    // "Worked on" means work happened, not "you clicked Open". A session that
    // touched files yesterday beats an activation from last week.
    const status = deriveCardStatus(
      { ...project, lastOpenedMs: NOW - 7 * DAY },
      snapshot({ lastActivityMs: NOW - 1 * DAY }),
      NOW,
    );
    expect(status.worked).toBe("1 day ago");
  });

  it("counts running, healthy and externally-owned processes as live", () => {
    // externally_owned means something IS serving on the port — the runner just
    // did not start it. From the user's point of view the project is up.
    const status = deriveCardStatus(
      project,
      snapshot({ processes: [proc("running"), proc("healthy"), proc("externally_owned")] }),
      NOW,
    );
    expect(status.runningCount).toBe(3);
  });

  it("does not count stopped, failed or transitional states as live", () => {
    const status = deriveCardStatus(
      project,
      snapshot({
        processes: [proc("stopped"), proc("failed"), proc("starting"), proc("building"), proc("stopping")],
      }),
      NOW,
    );
    expect(status.runningCount).toBe(0);
    expect(status.pending).toBe(false);
  });

  it("surfaces the question count and the health reason", () => {
    const status = deriveCardStatus(
      project,
      snapshot({
        questions: [
          { id: "q1", taskRunId: "t1", question: "Which colour?" },
          { id: "q2", taskRunId: "t1", question: "Ship it?" },
        ],
        health: { level: "amber", reason: "The website is still starting", failingProcesses: [] },
      }),
      NOW,
    );
    expect(status.questionCount).toBe(2);
    expect(status.level).toBe("amber");
    expect(status.reason).toBe("The website is still starting");
  });

  it("treats an empty health reason as no tooltip rather than an empty one", () => {
    const status = deriveCardStatus(project, snapshot(), NOW);
    expect(status.reason).toBeUndefined();
  });

  it("returns a null recency line for a project that was never opened", () => {
    const status = deriveCardStatus(project, snapshot(), NOW);
    expect(status.worked).toBeNull();
  });
});
