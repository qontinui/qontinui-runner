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
  envelopeOutcome,
  readVerdict,
  relayVerdict,
  INDETERMINATE_ACTION_ERROR,
  INDETERMINATE_NESTED_ACTION_ERROR,
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

/**
 * Case 3 of plan 2026-09-27-ui-bridge-action-failures-still-masked-after-the-strict-success-readers:
 * the SDK's in-process/HTTP `aiExecute` answers `success(nlActionResponse)`,
 * so a refused NL action arrives as `{ success: true, data: { success: false } }`.
 */
describe("relayVerdict — nested action result (APIResponse<NLActionResponse>)", () => {
  const ok = { ok: true, status: 200 };

  it("outer true + inner false is a FAILURE carrying the inner error", () => {
    const body = {
      success: true,
      data: { success: false, executedAction: "", error: "no element matched 'Save'" },
    };
    expect(relayVerdict(body, ok)).toEqual({
      success: false,
      error: "no element matched 'Save'",
      outcome: "failed",
    });
  });

  it("outer true + inner false with only failureInfo reports failureInfo.message", () => {
    const body = {
      success: true,
      data: { success: false, failureInfo: { errorCode: "UB-X", message: "timed out" } },
    };
    expect(relayVerdict(body, ok).error).toBe("timed out");
    expect(relayVerdict({ success: true, data: { success: false } }, ok)).toEqual({
      success: false,
      error: "nested action reported success: false",
      outcome: "failed",
    });
  });

  it("outer true + inner true succeeds", () => {
    expect(
      relayVerdict({ success: true, data: { success: true, executedAction: "click" } }, ok),
    ).toEqual({ success: true, error: undefined, outcome: "succeeded" });
  });

  it("outer true + inner non-boolean success is INDETERMINATE", () => {
    expect(relayVerdict({ success: true, data: { success: "yes" } }, ok)).toEqual({
      success: false,
      error: INDETERMINATE_NESTED_ACTION_ERROR,
      outcome: "indeterminate",
    });
  });

  it("the bare relay shape (no nested data.success) is governed by the outer flag only", () => {
    // Relay path: the NL response is lifted, so the body IS the action result.
    expect(relayVerdict({ success: true, executedAction: "click" }, ok).success).toBe(true);
    expect(relayVerdict({ success: false, error: "lifted" }, ok)).toEqual({
      success: false,
      error: "lifted",
      outcome: "failed",
    });
    // A `data` payload with no `success` key (or a non-object) is not an action result.
    expect(relayVerdict({ success: true, data: { a: 1 } }, ok).success).toBe(true);
    expect(relayVerdict({ success: true, data: [{ success: false }] }, ok).success).toBe(true);
    expect(relayVerdict({ success: true, data: null }, ok).success).toBe(true);
  });

  it("outer false is a failure whatever the inner says", () => {
    expect(relayVerdict({ success: false, error: "outer", data: { success: true } }, ok)).toEqual({
      success: false,
      error: "outer",
      outcome: "failed",
    });
    expect(
      relayVerdict({ success: true, data: { success: true } }, { ok: false, status: 500 }),
    ).toEqual({ success: false, error: "HTTP 500", outcome: "failed" });
  });

  it("envelopeOutcome mirrors the three states", () => {
    expect(envelopeOutcome({ success: true, data: { success: true } })).toEqual({
      kind: "succeeded",
    });
    expect(envelopeOutcome({ success: true, data: { success: false, error: "e" } })).toEqual({
      kind: "failed",
      error: "e",
    });
    expect(envelopeOutcome({ success: true, data: { success: undefined } })).toEqual({
      kind: "indeterminate",
    });
    expect(envelopeOutcome({ data: { success: true } })).toEqual({ kind: "indeterminate" });
  });

  it("readVerdict (plain reads) is unchanged: it does not look inside data", () => {
    expect(readVerdict({ success: true, data: { success: false } }, ok).success).toBe(true);
  });
});

describe("readVerdict", () => {
  it("a 2xx read with no success key succeeds (raw app JSON)", () => {
    expect(readVerdict({ elements: [] }, { ok: true, status: 200 }).success).toBe(true);
  });

  it("an explicit success:false or a non-2xx fails", () => {
    expect(readVerdict({ success: false, error: "x" }, { ok: true, status: 200 })).toEqual({
      success: false,
      error: "x",
      outcome: "failed",
    });
    expect(readVerdict({ elements: [] }, { ok: false, status: 500 }).error).toBe("HTTP 500");
  });
});
