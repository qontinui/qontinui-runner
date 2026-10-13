/**
 * The two-axis status classifier (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * D2 / Phase 1), enumerated: every location, every activity, the
 * location × activity cells the sources can produce, the UNKNOWN arms, and the
 * per-session registry join that replaced guessing "external" from a count.
 */

import { describe, expect, it } from "vitest";

import {
  ACTIVITY_PRIORITY,
  SESSION_ACTIVITIES,
  SESSION_LOCATIONS,
  SESSION_STATUS_LABELS,
  activityText,
  classifyLocation,
  classifySession,
  countUnidentified,
  nextWithActivity,
  parseActivityFilter,
  type DigestInput,
  type SessionActivity,
  type SessionLocation,
  type SessionStatusInputs,
  type StatusTabInput,
} from "./sessionStatusModel";

const NOW = Date.UTC(2026, 9, 13, 12, 0, 0);

function tab(over: Partial<StatusTabInput> = {}): StatusTabInput {
  return {
    alive: true,
    visible: true,
    exitCode: null,
    state: undefined,
    silent: false,
    lockWaiting: false,
    ...over,
  };
}

const assistantIdle: DigestInput = {
  lastMessageType: "assistant",
  lastAssistantHadToolUse: false,
  lastMessageTimestamp: new Date(NOW - 5 * 60_000).toISOString(),
};
const pendingTool: DigestInput = {
  lastMessageType: "assistant",
  lastAssistantHadToolUse: true,
  lastMessageTimestamp: new Date(NOW - 12 * 60_000).toISOString(),
};

function inputs(over: Partial<SessionStatusInputs> = {}): SessionStatusInputs {
  return {
    tab: null,
    registry: { kind: "ok", match: null },
    externalScan: "ok",
    digest: null,
    finished: null,
    ...over,
  };
}

const live = (status: string, runnerManaged: boolean) =>
  ({ kind: "ok", match: { pid: 4242, status, runnerManaged } }) as const;

describe("location axis — every value", () => {
  const cases: [SessionLocation, Partial<SessionStatusInputs>][] = [
    ["on-screen", { tab: tab({ visible: true }) }],
    ["hidden-here", { tab: tab({ visible: false }) }],
    ["hidden-here", { registry: live("busy", true) }],
    ["outside-runner", { registry: live("busy", false) }],
    ["other-device", { fleet: { thisDevice: false, sessionStatus: "working" } }],
    ["not-running", {}],
    ["unknown", { registry: { kind: "failed" } }],
    ["unknown", { registry: { kind: "pending" } }],
    ["unknown", { externalScan: "degraded" }],
    ["unknown", { externalScan: "pending" }],
  ];
  it.each(cases)("%s", (expected, over) => {
    expect(classifyLocation(inputs(over)).value).toBe(expected);
  });

  it("covers every SessionLocation", () => {
    expect(new Set(cases.map(([l]) => l))).toEqual(new Set(SESSION_LOCATIONS));
  });

  it("a degraded scan reads Unknown, never Not running", () => {
    const s = classifySession(inputs({ externalScan: "degraded" }));
    expect(s.location).toBe("unknown");
    expect(s.location).not.toBe("not-running");
    expect(s.activity).toBe("unknown");
  });

  it("a stale scan is the runner's labelled previous answer — as good as ok", () => {
    expect(classifyLocation(inputs({ externalScan: "stale" })).value).toBe("not-running");
  });

  it("a tab here decides location even when the registry read failed", () => {
    expect(classifyLocation(inputs({ tab: tab(), registry: { kind: "failed" } })).value).toBe(
      "on-screen",
    );
  });
});

describe("activity axis — every value", () => {
  const cases: [SessionActivity, Partial<SessionStatusInputs>][] = [
    ["needs-you", { tab: tab({ state: "needs-input" }) }],
    ["waiting-on-lock", { tab: tab({ lockWaiting: true }) }],
    ["working", { tab: tab({ silent: false }) }],
    ["idle-at-prompt", { tab: tab({ silent: true }), digest: assistantIdle }],
    ["no-output-mid-tool", { tab: tab({ silent: true }), digest: pendingTool }],
    ["finished", { tab: tab({ silent: true }), digest: assistantIdle, finished: true }],
    ["exited", { tab: tab({ alive: false, exitCode: 0 }) }],
    ["exited-error", { tab: tab({ alive: false, exitCode: 2 }) }],
    ["unknown", { tab: tab({ silent: true }), digest: null }],
  ];
  it.each(cases)("%s", (expected, over) => {
    expect(classifySession(inputs(over)).activity).toBe(expected);
  });

  it("covers every SessionActivity", () => {
    expect(new Set(cases.map(([a]) => a))).toEqual(new Set(SESSION_ACTIVITIES));
  });
});

describe("location × activity cells", () => {
  // The cells each location can actually reach, from real inputs.
  const cells: [SessionLocation, SessionActivity, Partial<SessionStatusInputs>][] = [];
  const tabActivities: [SessionActivity, Partial<StatusTabInput>, Partial<SessionStatusInputs>][] =
    [
      ["needs-you", { state: "needs-input" }, {}],
      ["waiting-on-lock", { lockWaiting: true }, {}],
      ["working", {}, {}],
      ["idle-at-prompt", { silent: true }, { digest: assistantIdle }],
      ["no-output-mid-tool", { silent: true }, { digest: pendingTool }],
      ["finished", { silent: true }, { digest: assistantIdle, finished: true }],
      ["exited", { alive: false, exitCode: 0 }, {}],
      ["exited-error", { alive: false, exitCode: 1 }, {}],
      ["unknown", { silent: true }, { digest: null }],
    ];
  for (const visible of [true, false]) {
    for (const [activity, t, rest] of tabActivities) {
      cells.push([
        visible ? "on-screen" : "hidden-here",
        activity,
        { tab: tab({ visible, ...t }), ...rest },
      ]);
    }
  }
  for (const [loc, managed] of [
    ["outside-runner", false],
    ["hidden-here", true],
  ] as const) {
    cells.push([loc, "needs-you", { registry: live("waiting", managed) }]);
    cells.push([loc, "working", { registry: live("busy", managed) }]);
    cells.push([loc, "idle-at-prompt", { registry: live("idle", managed) }]);
    cells.push([loc, "finished", { registry: live("idle", managed), finished: true }]);
    cells.push([loc, "unknown", { registry: live("compacting", managed) }]);
  }
  cells.push([
    "other-device",
    "needs-you",
    { fleet: { thisDevice: false, sessionStatus: "waiting_human" } },
  ]);
  cells.push([
    "other-device",
    "finished",
    { fleet: { thisDevice: false, sessionStatus: "finished" } },
  ]);
  cells.push(["other-device", "unknown", { fleet: { thisDevice: false, sessionStatus: null } }]);
  cells.push(["not-running", "exited", {}]);
  cells.push(["not-running", "finished", { finished: true }]);
  cells.push(["unknown", "unknown", { registry: { kind: "failed" } }]);
  cells.push(["unknown", "finished", { registry: { kind: "failed" }, finished: true }]);

  it.each(cells)("%s × %s", (location, activity, over) => {
    const s = classifySession(inputs(over));
    expect([s.location, s.activity]).toEqual([location, activity]);
    expect(s.reason.length).toBeGreaterThan(0);
  });
});

describe("registry status is an activity source above transcript/screen/regex (D2)", () => {
  it("an Outside-runner row takes its activity from registry status", () => {
    expect(classifySession(inputs({ registry: live("waiting", false) }))).toMatchObject({
      location: "outside-runner",
      activity: "needs-you",
    });
    expect(classifySession(inputs({ registry: live("busy", false) })).activity).toBe("working");
    expect(classifySession(inputs({ registry: live("idle", false) })).activity).toBe(
      "idle-at-prompt",
    );
  });

  it("registry idle outranks a silent tab whose digest says mid-tool", () => {
    const s = classifySession(
      inputs({ tab: tab({ silent: true }), digest: pendingTool, registry: live("idle", true) }),
    );
    expect(s.activity).toBe("idle-at-prompt");
  });

  it("registry busy outranks a screen that went quiet", () => {
    const s = classifySession(
      inputs({ tab: tab({ silent: true }), digest: assistantIdle, registry: live("busy", true) }),
    );
    expect(s.activity).toBe("working");
  });

  it("an unrecognized registry status falls through to the transcript", () => {
    const s = classifySession(
      inputs({
        tab: tab({ silent: true }),
        digest: pendingTool,
        registry: live("compacting", true),
      }),
    );
    expect(s.activity).toBe("no-output-mid-tool");
  });

  it("event-sourced state, when present, outranks the registry", () => {
    const s = classifySession(
      inputs({ tab: tab(), registry: live("idle", true), events: "working" }),
    );
    expect(s.activity).toBe("working");
  });
});

describe("the per-session external match", () => {
  it("a stray unmanaged pid no longer flips an unrelated session", () => {
    // The old heuristic: ANY external process + a transcript touched in the
    // last 2 minutes ⇒ "Active (External)". Here the stray process has no
    // registry row for this session, so the session is simply not running.
    const s = classifySession(
      inputs({ registry: { kind: "ok", match: null }, externalScan: "ok" }),
    );
    expect(s.location).toBe("not-running");
  });

  it("a process this runner owns is never Outside runner", () => {
    expect(classifyLocation(inputs({ registry: live("busy", true) })).value).toBe("hidden-here");
  });
});

describe("Unknown never renders as a default", () => {
  it("a missing finished marker is not 'not finished'", () => {
    const unknownMarker = classifySession(inputs({ finished: null }));
    expect(unknownMarker.activity).toBe("exited");
    const marked = classifySession(inputs({ finished: true }));
    expect(marked.activity).toBe("finished");
  });

  it("a silent transcript-backed tab with no digest is Unknown, not idle", () => {
    expect(classifySession(inputs({ tab: tab({ silent: true }), digest: null })).activity).toBe(
      "unknown",
    );
  });

  it("a silent tab with no transcript at all is idle at its prompt", () => {
    expect(
      classifySession(inputs({ tab: tab({ silent: true }), digest: "not-applicable" })).activity,
    ).toBe("idle-at-prompt");
  });
});

describe("labels", () => {
  it("one word per value, both axes", () => {
    for (const l of SESSION_LOCATIONS) expect(SESSION_STATUS_LABELS.location[l]).toBeTruthy();
    for (const a of SESSION_ACTIVITIES) {
      expect(SESSION_STATUS_LABELS.activity[a]).toBeTruthy();
      expect(ACTIVITY_PRIORITY[a]).toBeTypeOf("number");
    }
  });

  it("No output carries its duration; an error exit carries its code", () => {
    const silent = classifySession(inputs({ tab: tab({ silent: true }), digest: pendingTool }));
    expect(activityText(silent, NOW)).toBe("No output 12m (mid-tool)");
    const crashed = classifySession(inputs({ tab: tab({ alive: false, exitCode: 2 }) }));
    expect(activityText(crashed, NOW)).toBe("Exited — error (code 2)");
  });

  it("Needs you sorts first", () => {
    const min = Math.min(...SESSION_ACTIVITIES.map((a) => ACTIVITY_PRIORITY[a]));
    expect(ACTIVITY_PRIORITY["needs-you"]).toBe(min);
  });

  it("a filter persisted under the retired enum reads as all", () => {
    expect(parseActivityFilter("frozen")).toBe("all");
    expect(parseActivityFilter("active-in-zone")).toBe("all");
    expect(parseActivityFilter(null)).toBe("all");
    expect(parseActivityFilter("needs-you")).toBe("needs-you");
  });
});

describe("countUnidentified — 0 / 1 / many unmatched pids", () => {
  it("counts scan pids with no registry row, and only those", () => {
    const registry = new Set([10, 11]);
    expect(countUnidentified({ state: "ok", pids: [10, 11] }, registry)).toBe(0);
    expect(countUnidentified({ state: "ok", pids: [10, 99] }, registry)).toBe(1);
    expect(countUnidentified({ state: "stale", pids: [97, 98, 99, 10] }, registry)).toBe(3);
  });

  it("is UNKNOWN (null), not zero, when the scan or the registry did not answer", () => {
    expect(countUnidentified({ state: "degraded", pids: [] }, new Set())).toBeNull();
    expect(countUnidentified({ state: "pending", pids: [] }, new Set())).toBeNull();
    expect(countUnidentified({ state: "ok", pids: [1] }, null)).toBeNull();
  });
});

describe("nextWithActivity — the Ctrl+Shift+J cycle", () => {
  const row = (sessionId: string, activity: SessionActivity) => ({
    sessionId,
    status: { activity },
  });
  const rows = [
    row("a", "working"),
    row("b", "no-output-mid-tool"),
    row("c", "no-output-mid-tool"),
  ];

  it("starts at the first match, advances, and wraps", () => {
    expect(nextWithActivity(rows, "no-output-mid-tool", null)?.sessionId).toBe("b");
    expect(nextWithActivity(rows, "no-output-mid-tool", "b")?.sessionId).toBe("c");
    expect(nextWithActivity(rows, "no-output-mid-tool", "c")?.sessionId).toBe("b");
    expect(nextWithActivity(rows, "no-output-mid-tool", "gone")?.sessionId).toBe("b");
  });

  it("is null when nothing matches", () => {
    expect(nextWithActivity(rows, "needs-you", null)).toBeNull();
  });
});
