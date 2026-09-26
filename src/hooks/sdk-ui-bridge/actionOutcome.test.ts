/**
 * The one strict TS reader of a UI Bridge action result's `success` (plan
 * 2026-09-10-two-runner-call-sites-still-report-success-for-an-action-that-did-not-happen).
 * The no-`success`-key case is the exact input that read as success before.
 */
import { describe, it, expect } from "vitest";
import {
  actionOutcome,
  actionOutcomeError,
  actionSucceeded,
  relayVerdict,
  INDETERMINATE_ACTION_ERROR,
} from "./actionOutcome";

describe("actionOutcome", () => {
  it("a body with NO success key is indeterminate, never success", () => {
    const o = actionOutcome({ elementId: "x" });
    expect(o).toEqual({ kind: "indeterminate" });
    expect(actionSucceeded(o)).toBe(false);
    expect(actionOutcomeError(o, "fb")).toBe(INDETERMINATE_ACTION_ERROR);
  });

  it("null / non-object / non-boolean success are indeterminate", () => {
    for (const v of [null, undefined, "ok", 1, [true], { success: "true" }, { success: 1 }]) {
      expect(actionOutcome(v).kind).toBe("indeterminate");
    }
  });

  it("explicit true succeeds", () => {
    const o = actionOutcome({ success: true });
    expect(o).toEqual({ kind: "succeeded" });
    expect(actionSucceeded(o)).toBe(true);
    expect(actionOutcomeError(o, "fb")).toBeUndefined();
  });

  it("explicit false fails with its own error, or the fallback", () => {
    expect(actionOutcome({ success: false, error: "boom" })).toEqual({
      kind: "failed",
      error: "boom",
    });
    const o = actionOutcome({ success: false });
    expect(actionSucceeded(o)).toBe(false);
    expect(actionOutcomeError(o, "fb")).toBe("fb");
  });
});

describe("relayVerdict", () => {
  const ok = { ok: true, status: 200 };

  it("2xx + success:true is the only success", () => {
    expect(relayVerdict({ success: true }, ok)).toEqual({
      success: true,
      error: undefined,
      outcome: "succeeded",
    });
  });

  it("2xx with an empty body is an INDETERMINATE failure", () => {
    expect(relayVerdict({}, ok)).toEqual({
      success: false,
      error: INDETERMINATE_ACTION_ERROR,
      outcome: "indeterminate",
    });
  });

  it("explicit false keeps the body's error", () => {
    expect(relayVerdict({ success: false, error: "nope" }, ok)).toEqual({
      success: false,
      error: "nope",
      outcome: "failed",
    });
  });

  it("a non-2xx is a failure even when the body claims success or says nothing", () => {
    expect(relayVerdict({ success: true }, { ok: false, status: 502 }).success).toBe(false);
    expect(relayVerdict({}, { ok: false, status: 404 })).toEqual({
      success: false,
      error: "HTTP 404",
      outcome: "failed",
    });
  });
});
