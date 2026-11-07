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

/** The view as literal code lists: banners, then residual chips. */
function view(actions: ErrorBannerAction[]): { banners: string[]; residuals: string[] } {
  const v = errorBannerView(run(actions));
  const banners = v.faults.map((e) => e.error_code);
  if (v.info) banners.push(`info:${v.info.error_code}`);
  return { banners, residuals: v.residuals.map((f) => f.error.error_code) };
}

const NL = "COORD_LIVENESS_NO_LEADER";
const WD = "COORD_LIVENESS_WORKER_DEAD";
const NO_LEADER = fault(NL);
const WORKER_DEAD = fault(WD);
const INFO = fault("INFO_1", "info");

describe("errorBannerReducer + errorBannerView", () => {
  const cases: Array<{
    name: string;
    actions: ErrorBannerAction[];
    banners: string[];
    residuals: string[];
  }> = [
    { name: "no fault shows nothing", actions: [], banners: [], residuals: [] },
    {
      name: "a new fault shows the banner",
      actions: [{ type: "received", error: NO_LEADER, at: 1 }],
      banners: [NL],
      residuals: [],
    },
    {
      name: "dismiss collapses a non-info fault to the residual",
      actions: [
        { type: "received", error: NO_LEADER, at: 1 },
        { type: "dismissed", errorCode: NL, slot: "fault" },
      ],
      banners: [],
      residuals: [NL],
    },
    {
      name: "the same code re-emitted while dismissed keeps the residual",
      actions: [
        { type: "received", error: NO_LEADER, at: 1 },
        { type: "dismissed", errorCode: NL, slot: "fault" },
        { type: "received", error: NO_LEADER, at: 2 },
      ],
      banners: [],
      residuals: [NL],
    },
    {
      name: "a different code shows its banner and keeps the dismissed fault's residual",
      actions: [
        { type: "received", error: NO_LEADER, at: 1 },
        { type: "dismissed", errorCode: NL, slot: "fault" },
        { type: "received", error: WORKER_DEAD, at: 2 },
      ],
      banners: [WD],
      residuals: [NL],
    },
    {
      name: "an observed clear of a code removes its residual",
      actions: [
        { type: "received", error: NO_LEADER, at: 1 },
        { type: "dismissed", errorCode: NL, slot: "fault" },
        { type: "cleared", errorCode: NL },
      ],
      banners: [],
      residuals: [],
    },
    {
      name: "an observed clear of a code removes its undismissed banner",
      actions: [
        { type: "received", error: NO_LEADER, at: 1 },
        { type: "cleared", errorCode: NL },
      ],
      banners: [],
      residuals: [],
    },
    {
      name: "a clear for a different code leaves the fault in place",
      actions: [
        { type: "received", error: NO_LEADER, at: 1 },
        { type: "dismissed", errorCode: NL, slot: "fault" },
        { type: "cleared", errorCode: WD },
      ],
      banners: [],
      residuals: [NL],
    },
    {
      name: "a never-cleared fault keeps the residual",
      actions: [
        { type: "received", error: NO_LEADER, at: 1 },
        { type: "dismissed", errorCode: NL, slot: "fault" },
        { type: "expired", errorCode: NL, at: 1 },
      ],
      banners: [],
      residuals: [NL],
    },
    {
      name: "the residual re-opens the banner on click",
      actions: [
        { type: "received", error: NO_LEADER, at: 1 },
        { type: "dismissed", errorCode: NL, slot: "fault" },
        { type: "reopened", errorCode: NL },
      ],
      banners: [NL],
      residuals: [],
    },
    {
      name: "a dismissed info leaves no residual",
      actions: [
        { type: "received", error: INFO, at: 1 },
        { type: "dismissed", errorCode: "INFO_1", slot: "info" },
      ],
      banners: [],
      residuals: [],
    },
    {
      name: "an info auto-hides on expiry",
      actions: [
        { type: "received", error: INFO, at: 1 },
        { type: "expired", errorCode: "INFO_1", at: 1 },
      ],
      banners: [],
      residuals: [],
    },
    {
      name: "an info followed within 5 s by a warning keeps the warning after the info expires",
      actions: [
        { type: "received", error: INFO, at: 1 },
        { type: "received", error: NO_LEADER, at: 2 },
        { type: "expired", errorCode: "INFO_1", at: 1 },
      ],
      banners: [NL],
      residuals: [],
    },
    {
      name: "an earlier timer for the same info code does not hide a newer occurrence",
      actions: [
        { type: "received", error: INFO, at: 1 },
        { type: "received", error: INFO, at: 2 },
        { type: "expired", errorCode: "INFO_1", at: 1 },
      ],
      banners: ["info:INFO_1"],
      residuals: [],
    },
    {
      name: "an info that arrives and expires never erases a dismissed live fault",
      actions: [
        { type: "received", error: NO_LEADER, at: 1 },
        { type: "dismissed", errorCode: NL, slot: "fault" },
        { type: "received", error: INFO, at: 2 },
        { type: "expired", errorCode: "INFO_1", at: 2 },
      ],
      banners: [],
      residuals: [NL],
    },
    {
      name: "dismissing a fault's banner never dismisses an info that shares its code",
      actions: [
        { type: "received", error: NO_LEADER, at: 1 },
        { type: "received", error: fault(NL, "info"), at: 2 },
        { type: "dismissed", errorCode: NL, slot: "fault" },
      ],
      banners: [`info:${NL}`],
      residuals: [NL],
    },
    {
      name: "a dismissed fault re-reported with a different message re-opens its banner",
      actions: [
        { type: "received", error: WORKER_DEAD, at: 1 },
        { type: "dismissed", errorCode: WD, slot: "fault" },
        { type: "received", error: { ...WORKER_DEAD, message: "a second worker died" }, at: 2 },
      ],
      banners: [WD],
      residuals: [],
    },
  ];

  for (const c of cases) {
    it(c.name, () => {
      expect(view(c.actions)).toEqual({ banners: c.banners, residuals: c.residuals });
    });
  }

  it("a re-emission while dismissed refreshes last-seen", () => {
    const s = run([
      { type: "received", error: NO_LEADER, at: 1000 },
      { type: "dismissed", errorCode: NL, slot: "fault" },
      { type: "received", error: NO_LEADER, at: 5000 },
    ]);
    expect(s.faults.map((f) => f.lastSeenAt)).toEqual([5000]);
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
