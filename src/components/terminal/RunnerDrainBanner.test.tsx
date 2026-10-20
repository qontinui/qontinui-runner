/**
 * RunnerDrainBanner tests — plan `2026-09-13-drained-runner-never-reaches-idle`
 * Phase 3. The runner's vitest config is `environment: "node"`, so, like
 * `SessionRecoveryBanner.test.tsx`, this exercises the PURE model the banner
 * renders from; the JSX glue is verified through the UI Bridge.
 */

import { describe, it, expect } from "vitest";
import {
  drainBannerModel,
  deferredLabel,
  RESTORE_NOTE,
  ROLE_RESTORE_NOTE,
  ROLE_UNKNOWN_HEADING,
  UNKNOWN_HEADING,
} from "./RunnerDrainBanner";
import {
  autonomousResumeDetector,
  isCoordDrainSnapshot,
  type CoordDrainSnapshot,
} from "@/hooks/useCoordDrainState";

function snap(over: Partial<CoordDrainSnapshot>): CoordDrainSnapshot {
  return {
    state: "clear",
    autonomousSpawnsAllowed: true,
    until: null,
    reason: null,
    since: null,
    cause: null,
    deferredCount: 0,
    deferredByOrigin: {},
    lastReadAt: "2026-09-14T10:00:00Z",
    bootReadPending: false,
    ...over,
  };
}

describe("drainBannerModel", () => {
  it("says autonomous spawns are off when the drain is clear but the dispatch role closes the agent lane", () => {
    const m = drainBannerModel(
      snap({
        state: "clear",
        autonomousSpawnsAllowed: false,
        dispatchRole: "ci_node",
        agentLaneClosedByRole: true,
        deferredCount: 2,
      }),
    );
    expect(m?.status).toBe("warning");
    expect(m?.kind).toBe("role_closed");
    expect(m?.heading).toMatch(/ci_node/);
    expect(m?.restoreNote).toBe(ROLE_RESTORE_NOTE);
    expect(m?.deferredLabel).toBe("2 deferred work items");
    // Drained AND role-closed: the drain banner, but no promise that lifting
    // the drain brings the tabs back.
    const both = drainBannerModel(
      snap({ state: "drained", autonomousSpawnsAllowed: false, agentLaneClosedByRole: true }),
    );
    expect(both?.kind).toBe("drained");
    expect(both?.restoreNote).toBe(ROLE_RESTORE_NOTE);
    // Unknown AND role-closed: same, no promise that a readable drain helps.
    const unknownAndRole = drainBannerModel(
      snap({
        state: "unknown",
        autonomousSpawnsAllowed: false,
        bootReadPending: false,
        agentLaneClosedByRole: true,
      }),
    );
    expect(unknownAndRole?.kind).toBe("unknown");
    expect(unknownAndRole?.restoreNote).toBe(ROLE_RESTORE_NOTE);
    // An open role, or a runner that predates the field, keeps today's banner.
    expect(
      drainBannerModel(snap({ dispatchRole: "workhorse", agentLaneClosedByRole: false })),
    ).toBeNull();
    expect(drainBannerModel(snap({ state: "clear" }))).toBeNull();
  });

  it("says autonomous spawns are paused when the drain is clear but the dispatch role is unknown", () => {
    const m = drainBannerModel(
      snap({
        state: "clear",
        autonomousSpawnsAllowed: false,
        dispatchRole: null,
        agentLaneClosedByRole: false,
        dispatchRoleSource: "unknown",
        deferredCount: 1,
      }),
    );
    expect(m?.kind).toBe("unknown");
    expect(m?.status).toBe("error");
    expect(m?.heading).toBe(ROLE_UNKNOWN_HEADING);
    expect(m?.detail).toMatch(/dispatch role/);
    expect(m?.restoreNote).toBe(RESTORE_NOTE);
    expect(m?.deferredLabel).toBe("1 deferred work item");
    // A known source, or a runner predating the field, shows nothing.
    expect(drainBannerModel(snap({ dispatchRoleSource: "coord" }))).toBeNull();
    expect(
      drainBannerModel(snap({ dispatchRoleSource: "not_enrolled", state: "not_enrolled" })),
    ).toBeNull();
    // A drained device still shows the drain.
    expect(
      drainBannerModel(
        snap({ state: "drained", autonomousSpawnsAllowed: false, dispatchRoleSource: "unknown" }),
      )?.kind,
    ).toBe("drained");
  });

  it("says a closing role carried over from before the restart is not re-confirmed yet", () => {
    const m = drainBannerModel(
      snap({
        state: "clear",
        autonomousSpawnsAllowed: false,
        dispatchRole: "ci_node",
        agentLaneClosedByRole: true,
        dispatchRoleSource: "persisted",
        dispatchRoleObservedAt: "2026-10-20T08:00:00Z",
      }),
    );
    expect(m?.kind).toBe("role_closed");
    expect(m?.detail).toMatch(/before this runner started/);
    expect(m?.detail).toMatch(/not re-confirmed/);
    const fresh = drainBannerModel(
      snap({
        state: "clear",
        autonomousSpawnsAllowed: false,
        dispatchRole: "ci_node",
        agentLaneClosedByRole: true,
        dispatchRoleSource: "coord",
      }),
    );
    expect(fresh?.detail).toBeNull();
  });

  it("renders nothing before the first read, when clear, and when not a coord device", () => {
    expect(drainBannerModel(null)).toBeNull();
    expect(drainBannerModel(snap({ state: "clear" }))).toBeNull();
    expect(drainBannerModel(snap({ state: "not_enrolled", cause: "no coord_url" }))).toBeNull();
  });

  it("shows until, reason and the deferred count while drained", () => {
    const m = drainBannerModel(
      snap({
        state: "drained",
        autonomousSpawnsAllowed: false,
        until: "2026-09-14T12:00:00Z",
        reason: "rebuild runner",
        deferredCount: 3,
      }),
    );
    expect(m?.status).toBe("warning");
    expect(m?.heading).toMatch(/drained by coord/);
    expect(m?.detail).toMatch(/^until .+ · rebuild runner$/);
    expect(m?.deferredLabel).toBe("3 deferred work items");
  });

  it("carries the exact unknown copy and the cause", () => {
    const m = drainBannerModel(
      snap({
        state: "unknown",
        autonomousSpawnsAllowed: false,
        cause: "3 consecutive failed drain read(s)",
        deferredCount: 1,
      }),
    );
    expect(m?.status).toBe("error");
    expect(m?.heading).toBe("drain state unknown — autonomous spawns paused");
    expect(m?.heading).toBe(UNKNOWN_HEADING);
    expect(m?.detail).toBe("3 consecutive failed drain read(s)");
    expect(m?.deferredLabel).toBe("1 deferred work item");
  });

  it("omits the detail line for a drain with no until or reason", () => {
    expect(
      drainBannerModel(snap({ state: "drained", autonomousSpawnsAllowed: false }))?.detail,
    ).toBeNull();
  });
});

describe("deferredLabel", () => {
  it("is null for nothing deferred", () => {
    expect(deferredLabel(0)).toBeNull();
  });

  it("says the count is a floor once the backend cap was hit", () => {
    expect(deferredLabel(512, false)).toBe("512 deferred work items");
    expect(deferredLabel(512, true)).toBe("512+ deferred work items");
    // The singular only applies to a real one, never to a floor of one.
    expect(deferredLabel(1, false)).toBe("1 deferred work item");
    expect(deferredLabel(1, true)).toBe("1+ deferred work items");
  });
});

describe("the withheld-restore note (review N3)", () => {
  it("is carried by both deferring states and by neither allowing one", () => {
    expect(
      drainBannerModel(snap({ state: "drained", autonomousSpawnsAllowed: false }))?.restoreNote,
    ).toBe(RESTORE_NOTE);
    expect(
      drainBannerModel(snap({ state: "unknown", autonomousSpawnsAllowed: false }))?.restoreNote,
    ).toBe(RESTORE_NOTE);
    expect(drainBannerModel(snap({ state: "clear" }))).toBeNull();
  });

  it("says the tabs come back rather than leaving a blank grid unexplained", () => {
    expect(RESTORE_NOTE).toMatch(/not restored while this holds/);
    expect(RESTORE_NOTE).toMatch(/nothing is lost/);
  });
});

describe("isCoordDrainSnapshot", () => {
  it("accepts the backend shape and rejects anything else", () => {
    expect(isCoordDrainSnapshot(snap({}))).toBe(true);
    expect(
      isCoordDrainSnapshot({ state: "paused", autonomousSpawnsAllowed: true, deferredCount: 0 }),
    ).toBe(false);
    expect(isCoordDrainSnapshot(null)).toBe(false);
  });
});

describe("autonomousResumeDetector", () => {
  it("seeds from the deferral, so an edge that happened before the first snapshot still fires", () => {
    // Review N1: the drain lifted between the deferred call and this
    // subscription, so the stream only ever shows `allowed`. Seeded from the
    // stream alone, `last` would be `true` on the first snapshot and the
    // deferred restore would never re-run.
    let fired = 0;
    const feed = autonomousResumeDetector(
      () => {
        fired += 1;
      },
      () => true,
    );
    feed(snap({ state: "clear" }));
    expect(fired).toBe(1);
    feed(snap({ state: "clear" }));
    // Only the edge fires, not every allowing snapshot.
    expect(fired).toBe(1);
  });

  it("does not fire on the first snapshot when nothing was deferred", () => {
    let fired = 0;
    const feed = autonomousResumeDetector(
      () => {
        fired += 1;
      },
      () => false,
    );
    feed(snap({ state: "clear" }));
    expect(fired).toBe(0);
  });

  it("fires only on a transition from paused to allowed", () => {
    let fired = 0;
    const feed = autonomousResumeDetector(() => {
      fired += 1;
    });
    feed(snap({ state: "clear" })); // first observation: no edge
    feed(snap({ state: "drained", autonomousSpawnsAllowed: false }));
    feed(snap({ state: "unknown", autonomousSpawnsAllowed: false }));
    expect(fired).toBe(0);
    feed(snap({ state: "clear" }));
    expect(fired).toBe(1);
    feed(snap({ state: "clear" }));
    expect(fired).toBe(1);
  });
});

describe("drainBannerModel — boot", () => {
  it("does not flash the unknown banner while the boot read is in flight", () => {
    expect(
      drainBannerModel(
        snap({
          state: "unknown",
          autonomousSpawnsAllowed: false,
          cause: "coord drain state not read yet",
          bootReadPending: true,
        }),
      ),
    ).toBeNull();
    expect(
      drainBannerModel(
        snap({ state: "unknown", autonomousSpawnsAllowed: false, bootReadPending: false }),
      )?.heading,
    ).toBe(UNKNOWN_HEADING);
  });
});
