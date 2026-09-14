/**
 * RunnerDrainBanner tests — plan `2026-09-13-drained-runner-never-reaches-idle`
 * Phase 3. The runner's vitest config is `environment: "node"`, so, like
 * `SessionRecoveryBanner.test.tsx`, this exercises the PURE model the banner
 * renders from; the JSX glue is verified through the UI Bridge.
 */

import { describe, it, expect } from "vitest";
import { drainBannerModel, deferredLabel, UNKNOWN_HEADING } from "./RunnerDrainBanner";
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
    ...over,
  };
}

describe("drainBannerModel", () => {
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
