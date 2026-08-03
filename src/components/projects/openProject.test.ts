import { describe, expect, it } from "vitest";

import {
  describeStartFailure,
  isOpenBusy,
  openProse,
  planOpen,
  stderrTailOf,
  type OpenPhase,
} from "./openProject";
import type { ProcessState, ProcessStatusLite, ProjectSnapshot } from "./types";

function proc(id: string, state: ProcessState, name = id): ProcessStatusLite {
  return { id, name, state, cwd: "C:/proj" };
}

function snap(processes: ProcessStatusLite[]): ProjectSnapshot {
  return {
    // The card/plan paths only read `processes`; the rest is filler so the
    // fixture satisfies the type without pretending to be a real snapshot.
    project: {} as ProjectSnapshot["project"],
    processes,
    liveSessions: [],
    recentSessions: [],
    questions: [],
    health: { level: "unknown", reason: "", failingProcesses: [] },
  };
}

describe("planOpen", () => {
  it("refuses to run without a front page address, and FLAGS it as fixable", () => {
    const plan = planOpen({ frontPageUrl: null, processIds: ["a"], snapshot: snap([]) });
    expect(plan.url).toBeNull();
    expect(plan.blockedReason).toContain("address");
    expect(plan.toStart).toEqual([]);
    // `needsFrontPage` is what turns a dead end into an in-place remedy. The
    // copy no longer says "add one and try again" — that instructed an action
    // the product did not offer, since nothing ever called
    // `set_project_front_page`. The flag drives the affordance instead.
    expect(plan.needsFrontPage).toBe(true);
    expect(plan.blockedReason).not.toContain("try again");
  });

  it("treats a whitespace-only address as absent", () => {
    const plan = planOpen({ frontPageUrl: "   ", processIds: [], snapshot: snap([]) });
    expect(plan.blockedReason).toContain("address");
    expect(plan.needsFrontPage).toBe(true);
  });

  it("does NOT flag needsFrontPage on other blocks", () => {
    // A pending snapshot is a wait, not a missing-configuration problem.
    // Offering "set up an address" there would invite the user to change
    // something that is already correct.
    const plan = planOpen({ frontPageUrl: "http://localhost:3000", processIds: ["a"] });
    expect(plan.blockedReason).toContain("Still checking");
    expect(plan.needsFrontPage).toBeFalsy();
  });

  it("opens a project that declares no processes", () => {
    // A static site or an already-deployed URL: there is nothing to start, but
    // showing it is still the right answer to Open.
    const plan = planOpen({
      frontPageUrl: "http://localhost:3000",
      processIds: [],
      snapshot: undefined,
    });
    expect(plan.blockedReason).toBeNull();
    expect(plan.url).toBe("http://localhost:3000");
    expect(plan.toStart).toEqual([]);
  });

  it("waits rather than guessing when the snapshot has not arrived", () => {
    // The load-bearing case: without a snapshot we cannot tell a stopped
    // process from an unread one, and starting one that is already up takes
    // the user's site down in response to them asking to see it.
    const plan = planOpen({
      frontPageUrl: "http://localhost:3000",
      processIds: ["web"],
      snapshot: undefined,
    });
    expect(plan.toStart).toEqual([]);
    expect(plan.blockedReason).toContain("Still checking");
  });

  it("starts only the processes that are not already live", () => {
    const plan = planOpen({
      frontPageUrl: "http://localhost:3000",
      processIds: ["web", "api", "worker"],
      snapshot: snap([
        proc("web", "healthy", "Website"),
        proc("api", "stopped", "API"),
        proc("worker", "failed", "Worker"),
      ]),
    });
    expect(plan.toStart).toEqual([
      { id: "api", name: "API" },
      { id: "worker", name: "Worker" },
    ]);
  });

  it("never restarts a healthy or externally-owned process", () => {
    for (const state of ["running", "healthy", "externally_owned"] as ProcessState[]) {
      const plan = planOpen({
        frontPageUrl: "http://localhost:3000",
        processIds: ["web"],
        snapshot: snap([proc("web", state)]),
      });
      expect(plan.toStart, `${state} must not be restarted`).toEqual([]);
    }
  });

  it("skips a stale binding instead of failing the whole run", () => {
    // `deleted` was bound to the project and its process config has since been
    // removed, so the snapshot does not carry it.
    const plan = planOpen({
      frontPageUrl: "http://localhost:3000",
      processIds: ["deleted", "api"],
      snapshot: snap([proc("api", "stopped", "API")]),
    });
    expect(plan.toStart).toEqual([{ id: "api", name: "API" }]);
    expect(plan.blockedReason).toBeNull();
  });

  it("preserves the project's declared process order", () => {
    const plan = planOpen({
      frontPageUrl: "http://localhost:3000",
      processIds: ["db", "api", "web"],
      snapshot: snap([proc("web", "stopped"), proc("db", "stopped"), proc("api", "stopped")]),
    });
    expect(plan.toStart.map((p) => p.id)).toEqual(["db", "api", "web"]);
  });
});

describe("openProse", () => {
  it("names the project, not the process, when there is only one", () => {
    const phase: OpenPhase = {
      kind: "starting",
      processId: "web",
      processName: "Website",
      step: 1,
      total: 1,
    };
    expect(openProse(phase, "Pizzeria")).toBe("Starting Pizzeria…");
  });

  it("counts the steps when there is more than one process", () => {
    const phase: OpenPhase = {
      kind: "starting",
      processId: "api",
      processName: "API",
      step: 2,
      total: 3,
    };
    expect(openProse(phase, "Pizzeria")).toBe("Starting API (2 of 3)…");
  });

  it("says nothing at rest", () => {
    expect(openProse({ kind: "idle" }, "Pizzeria")).toBe("");
  });

  it("surfaces the failure message verbatim", () => {
    const phase: OpenPhase = { kind: "failed", message: "API didn't start.", detail: "boom" };
    expect(openProse(phase, "Pizzeria")).toBe("API didn't start.");
  });

  it("covers opening and done", () => {
    expect(openProse({ kind: "opening", url: "u" }, "Pizzeria")).toBe("Opening Pizzeria…");
    expect(openProse({ kind: "done", url: "u" }, "Pizzeria")).toBe("Pizzeria is open.");
  });
});

describe("isOpenBusy", () => {
  it("is true only while work is in flight", () => {
    expect(isOpenBusy({ kind: "idle" })).toBe(false);
    expect(
      isOpenBusy({ kind: "starting", processId: "a", processName: "A", step: 1, total: 1 }),
    ).toBe(true);
    expect(isOpenBusy({ kind: "opening", url: "u" })).toBe(true);
    // Terminal states re-enable the button so the user can retry.
    expect(isOpenBusy({ kind: "done", url: "u" })).toBe(false);
    expect(isOpenBusy({ kind: "failed", message: "m" })).toBe(false);
  });
});

describe("describeStartFailure", () => {
  it("leads with plain English and keeps the raw diagnostic as detail", () => {
    const err = new Error(
      "Process 'API' exited before reaching ready state (exit code: Some(1)); stderr: ModuleNotFoundError: No module named 'fastapi'",
    );
    const phase = describeStartFailure("API", err);
    expect(phase.kind).toBe("failed");
    if (phase.kind !== "failed") return;
    expect(phase.message).toBe("API didn't start. The details below say why.");
    expect(phase.detail).toContain("fastapi");
  });

  it("handles a non-Error rejection", () => {
    // Tauri `invoke` rejects with a plain string, not an Error.
    const phase = describeStartFailure("Website", "Process capture manager not initialized");
    if (phase.kind !== "failed") throw new Error("expected failed");
    expect(phase.detail).toBe("Process capture manager not initialized");
  });

  it("drops an empty detail rather than rendering a blank disclosure", () => {
    const phase = describeStartFailure("Website", "   ");
    if (phase.kind !== "failed") throw new Error("expected failed");
    expect(phase.detail).toBeUndefined();
  });
});

describe("stderrTailOf", () => {
  it("keeps the last stderr lines in order, ignoring stdout", () => {
    const lines = [
      { stream: "stdout", line: "listening" },
      { stream: "stderr", line: "one" },
      { stream: "stdout", line: "noise" },
      { stream: "stderr", line: "two" },
      { stream: "stderr", line: "three" },
    ];
    expect(stderrTailOf(lines, 2)).toBe("two\nthree");
  });

  it("is empty for absent or stdout-only output", () => {
    expect(stderrTailOf(undefined)).toBe("");
    expect(stderrTailOf([])).toBe("");
    expect(stderrTailOf([{ stream: "stdout", line: "fine" }])).toBe("");
  });
});
