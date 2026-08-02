import { describe, expect, it } from "vitest";

import { buildDigest, describeLastVisit, describeSessionWhen } from "./projectDigest";
import type { HealthLevel, ProjectSnapshot, SessionLite } from "./types";

const DAY = 24 * 60 * 60 * 1000;
// A fixed Wednesday so the weekday assertions are not clock-dependent.
const NOW = new Date("2026-07-29T12:00:00Z").getTime();

function session(overrides: Partial<SessionLite> = {}): SessionLite {
  return { id: "s1", source: "terminal", filesTouched: 0, ...overrides };
}

function snap(overrides: Partial<ProjectSnapshot> = {}): ProjectSnapshot {
  return {
    project: {} as ProjectSnapshot["project"],
    processes: [],
    liveSessions: [],
    recentSessions: [],
    questions: [],
    health: { level: "unknown", reason: "", failingProcesses: [] },
    ...overrides,
  };
}

function health(level: HealthLevel, reason = "") {
  return { level, reason, failingProcesses: [] };
}

describe("describeLastVisit", () => {
  it("uses the weekday inside a week — what a person actually remembers", () => {
    // NOW is a Wednesday; 8 hours short of 7 days back is the Thursday before.
    expect(describeLastVisit(NOW - 6 * DAY, NOW)).toBe("Thursday");
  });

  it("prefers today/yesterday over a weekday", () => {
    expect(describeLastVisit(NOW - 2 * 60 * 60 * 1000, NOW)).toBe("today");
    expect(describeLastVisit(NOW - 30 * 60 * 60 * 1000, NOW)).toBe("yesterday");
  });

  it("switches to relative phrasing past a week", () => {
    expect(describeLastVisit(NOW - 8 * DAY, NOW)).toBe("last week");
    expect(describeLastVisit(NOW - 20 * DAY, NOW)).toBe("2 weeks ago");
    expect(describeLastVisit(NOW - 40 * DAY, NOW)).toBe("a month ago");
    expect(describeLastVisit(NOW - 100 * DAY, NOW)).toBe("3 months ago");
  });

  it("clamps a future stamp instead of rendering 'in 3 hours'", () => {
    // Clock skew between the registry write and this render is not information.
    expect(describeLastVisit(NOW + 3 * 60 * 60 * 1000, NOW)).toBe("just now");
  });

  it("is null for a project never opened", () => {
    expect(describeLastVisit(undefined, NOW)).toBeNull();
    expect(describeLastVisit(Number.NaN, NOW)).toBeNull();
  });
});

describe("buildDigest", () => {
  it("suppresses every line when no snapshot has loaded", () => {
    const d = buildDigest(undefined, NOW - 3 * DAY, NOW);
    expect(d.lines).toEqual([]);
    // The header is still derivable from the registry alone.
    expect(d.since).toBe("Sunday");
  });

  it("counts sessions and files since the last visit", () => {
    const d = buildDigest(
      snap({
        recentSessions: [
          session({ id: "a", lastActivityMs: NOW - DAY, filesTouched: 9 }),
          session({ id: "b", lastActivityMs: NOW - 2 * DAY, filesTouched: 5 }),
        ],
      }),
      NOW - 3 * DAY,
      NOW,
    );
    const line = d.lines.find((l) => l.id === "sessions");
    expect(line?.text).toBe("2 agent sessions ran, 14 files changed");
  });

  it("excludes work that happened before the last visit", () => {
    // The load-bearing cut-off: `lastOpenedMs`, not `snapshot.lastActivityMs`.
    const d = buildDigest(
      snap({
        recentSessions: [
          session({ id: "old", lastActivityMs: NOW - 10 * DAY, filesTouched: 40 }),
          session({ id: "new", lastActivityMs: NOW - DAY, filesTouched: 2 }),
        ],
      }),
      NOW - 3 * DAY,
      NOW,
    );
    expect(d.lines.find((l) => l.id === "sessions")?.text).toBe("1 agent session ran, 2 files changed");
  });

  it("counts an undated session rather than under-reporting work", () => {
    const d = buildDigest(
      snap({ recentSessions: [session({ id: "x", filesTouched: 3 })] }),
      NOW - 3 * DAY,
      NOW,
    );
    expect(d.lines.find((l) => l.id === "sessions")?.text).toBe("1 agent session ran, 3 files changed");
  });

  it("omits the files clause when nothing was touched", () => {
    const d = buildDigest(
      snap({ recentSessions: [session({ lastActivityMs: NOW - DAY, filesTouched: 0 })] }),
      NOW - 3 * DAY,
      NOW,
    );
    expect(d.lines.find((l) => l.id === "sessions")?.text).toBe("1 agent session ran");
  });

  it("says nothing about health when there is nothing to observe", () => {
    // `unknown` earns neither reassurance nor alarm.
    const d = buildDigest(snap({ health: health("unknown") }), NOW - DAY, NOW);
    expect(d.lines.find((l) => l.id === "health")).toBeUndefined();
  });

  it("reassures on green and prefers the snapshot's own sentence on red", () => {
    const green = buildDigest(snap({ health: health("green") }), NOW - DAY, NOW);
    expect(green.lines.find((l) => l.id === "health")).toMatchObject({
      text: "It still starts up fine",
      tone: "good",
      actionable: false,
    });

    const red = buildDigest(
      snap({ health: health("red", "The website stopped a few minutes after starting.") }),
      NOW - DAY,
      NOW,
    );
    expect(red.lines.find((l) => l.id === "health")).toMatchObject({
      text: "The website stopped a few minutes after starting.",
      tone: "bad",
      actionable: true,
    });
  });

  it("falls back to plain wording when the health reason is blank", () => {
    const d = buildDigest(snap({ health: health("amber", "   ") }), NOW - DAY, NOW);
    expect(d.lines.find((l) => l.id === "health")?.text).toBe("Something isn't starting up");
  });

  it("pluralizes the waiting questions and marks them actionable", () => {
    const one = buildDigest(
      snap({ questions: [{ id: "q", taskRunId: "t", question: "?" }] }),
      NOW - DAY,
      NOW,
    );
    expect(one.lines.find((l) => l.id === "questions")).toMatchObject({
      text: "1 question is waiting for you",
      actionable: true,
    });

    const two = buildDigest(
      snap({
        questions: [
          { id: "q1", taskRunId: "t", question: "?" },
          { id: "q2", taskRunId: "t", question: "?" },
        ],
      }),
      NOW - DAY,
      NOW,
    );
    expect(two.lines.find((l) => l.id === "questions")?.text).toBe(
      "2 questions are waiting for you",
    );
  });

  it("orders the lines sessions → health → questions", () => {
    const d = buildDigest(
      snap({
        recentSessions: [session({ lastActivityMs: NOW - DAY, filesTouched: 1 })],
        health: health("green"),
        questions: [{ id: "q", taskRunId: "t", question: "?" }],
      }),
      NOW - 2 * DAY,
      NOW,
    );
    expect(d.lines.map((l) => l.id)).toEqual(["sessions", "health", "questions"]);
  });
});

describe("describeSessionWhen", () => {
  it("renders day plus duration when both ends are known", () => {
    const end = NOW - DAY;
    expect(describeSessionWhen(session({ lastActivityMs: end }), end - 40 * 60000, NOW)).toBe(
      "yesterday, 40 min",
    );
  });

  it("renders hours past sixty minutes", () => {
    const end = NOW - DAY;
    expect(describeSessionWhen(session({ lastActivityMs: end }), end - 125 * 60000, NOW)).toBe(
      "yesterday, 2h 5m",
    );
    expect(describeSessionWhen(session({ lastActivityMs: end }), end - 120 * 60000, NOW)).toBe(
      "yesterday, 2h",
    );
  });

  it("reports the day alone rather than fabricating a duration", () => {
    // A session still running, or one with no recorded start.
    expect(describeSessionWhen(session({ lastActivityMs: NOW - DAY }), undefined, NOW)).toBe("yesterday");
  });

  it("is empty when the session has no timestamp at all", () => {
    expect(describeSessionWhen(session(), undefined, NOW)).toBe("");
  });
});
