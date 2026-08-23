import { describe, it, expect, vi } from "vitest";
import {
  RECOVERY_FAILED,
  RECOVERY_OUT_OF_SCOPE,
  RECOVERY_TARGET_MISSING,
  RECOVERY_UNSCOPED,
  RECOVERY_WRITE_REFUSED,
  RecoveryRefusedError,
  assertRecoveryDispatchAllowed,
  isWriteAction,
  recoveryFailureData,
  recoveryVerdict,
  scopeRecoveryExecutor,
} from "./recoveryScope";

describe("isWriteAction", () => {
  it("catches every input-mutating action, case- and space-insensitively", () => {
    for (const w of [
      "type",
      "setValue",
      "fill",
      "paste",
      "clear",
      "sendKeys",
      "writeToTerminal",
      " TYPE ",
    ]) {
      expect(isWriteAction(w), w).toBe(true);
    }
  });

  it("leaves repositioning actions alone", () => {
    for (const ok of ["click", "focus", "blur", "scrollIntoView", "hover"]) {
      expect(isWriteAction(ok), ok).toBe(false);
    }
  });
});

describe("assertRecoveryDispatchAllowed", () => {
  it("allows a non-write action on the addressed element", () => {
    expect(() => assertRecoveryDispatchAllowed("btn-1", "btn-1", "focus")).not.toThrow();
  });

  it("refuses an unscoped recovery rather than guessing a target", () => {
    expect(() => assertRecoveryDispatchAllowed("", "btn-1", "focus")).toThrow(
      expect.objectContaining({ code: RECOVERY_UNSCOPED }),
    );
  });

  it("refuses to touch a DIFFERENT element — the command-palette contamination", () => {
    expect(() =>
      assertRecoveryDispatchAllowed("terminal-input-term-3", "command-palette-input", "focus"),
    ).toThrow(expect.objectContaining({ code: RECOVERY_OUT_OF_SCOPE }));
  });

  it("refuses to write even on the addressed element", () => {
    expect(() => assertRecoveryDispatchAllowed("btn-1", "btn-1", "type")).toThrow(
      expect.objectContaining({ code: RECOVERY_WRITE_REFUSED }),
    );
  });
});

describe("scopeRecoveryExecutor", () => {
  const makeBridge = () => ({
    executeAction: vi.fn(async () => ({ success: true })),
    executeComponentAction: vi.fn(async () => ({ success: true })),
    fillForm: vi.fn(async () => ({ success: true })),
    discover: vi.fn(async () => ({ elements: [] })),
  });

  it("passes an in-scope, non-write action through to the bridge", async () => {
    const bridge = makeBridge();
    const scoped = scopeRecoveryExecutor(bridge, "btn-1");
    await scoped.executeAction("btn-1", { action: "focus" });
    expect(bridge.executeAction).toHaveBeenCalledWith("btn-1", { action: "focus" });
  });

  it("never dispatches a write, and never dispatches to another element", async () => {
    const bridge = makeBridge();
    const scoped = scopeRecoveryExecutor(bridge, "terminal-input-term-3");
    await expect(
      scoped.executeAction("command-palette-input", { action: "type" }),
    ).rejects.toBeInstanceOf(RecoveryRefusedError);
    await expect(
      scoped.executeAction("terminal-input-term-3", { action: "setValue" }),
    ).rejects.toBeInstanceOf(RecoveryRefusedError);
    expect(bridge.executeAction).not.toHaveBeenCalled();
  });

  it("refuses the two non-element-scoped surfaces outright", async () => {
    const bridge = makeBridge();
    const scoped = scopeRecoveryExecutor(bridge, "btn-1");
    await expect(scoped.executeComponentAction()).rejects.toBeInstanceOf(RecoveryRefusedError);
    await expect(scoped.fillForm()).rejects.toBeInstanceOf(RecoveryRefusedError);
    expect(bridge.executeComponentAction).not.toHaveBeenCalled();
    expect(bridge.fillForm).not.toHaveBeenCalled();
  });

  it("leaves every other bridge member reachable", async () => {
    const bridge = makeBridge();
    const scoped = scopeRecoveryExecutor(bridge, "btn-1");
    await scoped.discover();
    expect(bridge.discover).toHaveBeenCalled();
  });
});

describe("recoveryVerdict", () => {
  it("reports recovered ONLY when the executor itself succeeded", () => {
    expect(recoveryVerdict({ success: true })).toEqual({ recovered: true, reason: null });
  });

  it("does not launder a failed executor run into a recovery", () => {
    const v = recoveryVerdict({
      success: false,
      errorCode: "UB-ELEM-NOT-FOUND",
      error: 'Could not find element matching: "the thing"',
    });
    expect(v.recovered).toBe(false);
    expect(v.reason).toContain("UB-ELEM-NOT-FOUND");
  });

  it("treats a missing/garbage result as not recovered", () => {
    expect(recoveryVerdict(null).recovered).toBe(false);
    expect(recoveryVerdict(undefined).recovered).toBe(false);
    expect(recoveryVerdict("recovered!").recovered).toBe(false);
    expect(recoveryVerdict({}).recovered).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// Manual-test-loop iteration 10, item 4 — the last laundering edge.
//
// `/ai/recovery/attempt` answered a typed refusal with HTTP 200
// `{"success":true,"recovered":false}`. The frontend DID send
// `{success:false, error:"RECOVERY_UNSCOPED: …", data:{recovered:false}}`, but
// the runner's response dispatcher forwards only `response.data` when the
// handler supplies one — so `success` and `error` never reached the HTTP layer.
// ---------------------------------------------------------------------------
describe("recoveryFailureData (item 4)", () => {
  it("mirrors the failure envelope INTO data, which is the half that survives the seam", () => {
    const msg = `${RECOVERY_UNSCOPED}: ai_recovery_attempt requires params.elementId`;
    expect(recoveryFailureData(RECOVERY_UNSCOPED, msg)).toEqual({
      success: false,
      error: msg,
      code: RECOVERY_UNSCOPED,
      recovered: false,
    });
  });

  it("carries the caller's context through without letting it overwrite the verdict", () => {
    const out = recoveryFailureData(RECOVERY_TARGET_MISSING, "gone", { elementId: "btn-1" });
    expect(out.elementId).toBe("btn-1");
    expect(out.success).toBe(false);
    expect(out.recovered).toBe(false);
    expect(out.code).toBe(RECOVERY_TARGET_MISSING);
  });

  it("never reports recovered:true — a failure payload has exactly one verdict", () => {
    for (const code of [RECOVERY_UNSCOPED, RECOVERY_TARGET_MISSING, RECOVERY_FAILED]) {
      expect(recoveryFailureData(code, "x").recovered).toBe(false);
    }
  });
});
