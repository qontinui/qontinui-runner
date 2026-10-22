import { describe, it, expect } from "vitest";
import {
  INITIAL_ERROR_BANNER_STATE,
  errorBannerReducer,
  errorBannerView,
  dismissLabel,
  residualLabel,
  type ErrorBannerAction,
  type ErrorBannerState,
  type ErrorEventPayload,
} from "./errorBannerState";

function fault(code: string, severity: ErrorEventPayload["severity"] = "warning"): ErrorEventPayload {
  return {
    title: `Title for ${code}`,
    message: "m",
    error_code: code,
    severity,
    recoverable: true,
  };
}

function run(actions: ErrorBannerAction[]): ErrorBannerState {
  return actions.reduce(errorBannerReducer, INITIAL_ERROR_BANNER_STATE);
}

const NO_LEADER = fault("COORD_LIVENESS_NO_LEADER");
const WORKER_DEAD = fault("COORD_LIVENESS_WORKER_DEAD");

describe("errorBannerReducer + errorBannerView", () => {
  const cases: Array<{ name: string; actions: ErrorBannerAction[]; view: string }> = [
    { name: "no fault shows nothing", actions: [], view: "none" },
    {
      name: "a new fault shows the banner",
      actions: [{ type: "received", error: NO_LEADER, at: 1 }],
      view: "banner",
    },
    {
      name: "dismiss collapses a non-info fault to the residual",
      actions: [{ type: "received", error: NO_LEADER, at: 1 }, { type: "dismissed" }],
      view: "residual",
    },
    {
      name: "the same code re-emitted while dismissed keeps the residual",
      actions: [
        { type: "received", error: NO_LEADER, at: 1 },
        { type: "dismissed" },
        { type: "received", error: NO_LEADER, at: 2 },
      ],
      view: "residual",
    },
    {
      name: "a different code re-shows the banner",
      actions: [
        { type: "received", error: NO_LEADER, at: 1 },
        { type: "dismissed" },
        { type: "received", error: WORKER_DEAD, at: 2 },
      ],
      view: "banner",
    },
    {
      name: "an observed clear of the current code removes the residual",
      actions: [
        { type: "received", error: NO_LEADER, at: 1 },
        { type: "dismissed" },
        { type: "cleared", errorCode: "COORD_LIVENESS_NO_LEADER" },
      ],
      view: "none",
    },
    {
      name: "an observed clear of the current code removes an undismissed banner",
      actions: [
        { type: "received", error: NO_LEADER, at: 1 },
        { type: "cleared", errorCode: "COORD_LIVENESS_NO_LEADER" },
      ],
      view: "none",
    },
    {
      name: "a clear for a DIFFERENT code leaves the current fault in place",
      actions: [
        { type: "received", error: NO_LEADER, at: 1 },
        { type: "dismissed" },
        { type: "cleared", errorCode: "COORD_LIVENESS_WORKER_DEAD" },
      ],
      view: "residual",
    },
    {
      name: "a never-cleared fault keeps the residual indefinitely",
      actions: [
        { type: "received", error: NO_LEADER, at: 1 },
        { type: "dismissed" },
        { type: "expired", errorCode: "COORD_LIVENESS_NO_LEADER", at: 1 },
      ],
      view: "residual",
    },
    {
      name: "the residual re-opens the banner on click",
      actions: [
        { type: "received", error: NO_LEADER, at: 1 },
        { type: "dismissed" },
        { type: "reopened" },
      ],
      view: "banner",
    },
    {
      name: "a dismissed info toast leaves no residual",
      actions: [{ type: "received", error: fault("INFO_1", "info"), at: 1 }, { type: "dismissed" }],
      view: "none",
    },
    {
      name: "an info toast auto-hides on expiry",
      actions: [
        { type: "received", error: fault("INFO_1", "info"), at: 1 },
        { type: "expired", errorCode: "INFO_1", at: 1 },
      ],
      view: "none",
    },
    {
      name: "a stale info expiry does not hide a newer warning",
      actions: [
        { type: "received", error: fault("INFO_1", "info"), at: 1 },
        { type: "received", error: NO_LEADER, at: 2 },
        { type: "expired", errorCode: "INFO_1", at: 1 },
      ],
      view: "banner",
    },
  ];

  cases.push({
    name: "an earlier timer for the same info code does not hide a newer occurrence",
    actions: [
      { type: "received", error: fault("INFO_1", "info"), at: 1 },
      { type: "received", error: fault("INFO_1", "info"), at: 2 },
      { type: "expired", errorCode: "INFO_1", at: 1 },
    ],
    view: "banner",
  });

  for (const c of cases) {
    it(c.name, () => {
      expect(errorBannerView(run(c.actions))).toBe(c.view);
    });
  }

  it("a re-emission while dismissed refreshes last-seen", () => {
    const s = run([
      { type: "received", error: NO_LEADER, at: 1000 },
      { type: "dismissed" },
      { type: "received", error: NO_LEADER, at: 5000 },
    ]);
    expect(s.lastSeenAt).toBe(5000);
  });
});

describe("labels", () => {
  it("the dismiss control is named after the fault", () => {
    expect(dismissLabel(NO_LEADER)).toBe("Dismiss: Title for COORD_LIVENESS_NO_LEADER");
  });

  it("the residual names the code and when it was last seen, never a clear", () => {
    expect(residualLabel(NO_LEADER, 0, 125_000)).toBe("COORD_LIVENESS_NO_LEADER · last seen 2m ago");
  });
});
