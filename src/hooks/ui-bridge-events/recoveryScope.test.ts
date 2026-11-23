import { describe, it, expect, vi } from "vitest";
import {
  RECOVERY_FAILED,
  RECOVERY_NOT_DISPATCHED,
  RECOVERY_OUT_OF_SCOPE,
  RECOVERY_TARGET_MISSING,
  RECOVERY_UNSCOPED,
  RECOVERY_WRITE_REFUSED,
  RecoveryRefusedError,
  assertRecoveryDispatchAllowed,
  isWriteAction,
  newRecoveryDispatchRecord,
  originalActionFacts,
  recoveryFailureData,
  recoveryVerdict,
  scopeRecoveryExecutor,
  undispatchedRefusal,
} from "./recoveryScope";
import { NLActionExecutor } from "@qontinui/ui-bridge/ai";
import writeActionsGenerated from "./writeActions.generated.json";

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
    for (const ok of ["click", "focus", "blur", "scrollIntoView", "hover", "unhover"]) {
      expect(isWriteAction(ok), ok).toBe(false);
    }
  });

  // The list is the runner's `WRITE_ACTIONS` (recovery_executor.rs), generated
  // into writeActions.generated.json and drift-gated on the Rust side by
  // `write_actions_fixture_matches_rust_declaration`. This half pins that the
  // frontend check refuses exactly that set — every generated member, in any
  // case/spacing — so the two enforcement points agree on one list.
  it("refuses every action in the runner-generated write-action list", () => {
    expect(writeActionsGenerated.length).toBeGreaterThan(0);
    for (const w of writeActionsGenerated) {
      expect(w, "generated entries are normalized").toBe(w.trim().toLowerCase());
      expect(isWriteAction(w), w).toBe(true);
      expect(isWriteAction(` ${w.toUpperCase()} `), w).toBe(true);
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
  it("reports attemptSucceeded ONLY when the executor itself succeeded", () => {
    expect(recoveryVerdict({ success: true })).toEqual({ attemptSucceeded: true, reason: null });
  });

  it("does not launder a failed executor run into a recovery", () => {
    const v = recoveryVerdict({
      success: false,
      errorCode: "UB-ELEM-NOT-FOUND",
      error: 'Could not find element matching: "the thing"',
    });
    expect(v.attemptSucceeded).toBe(false);
    expect(v.reason).toContain("UB-ELEM-NOT-FOUND");
  });

  it("treats a missing/garbage result as a failed attempt", () => {
    expect(recoveryVerdict(null).attemptSucceeded).toBe(false);
    expect(recoveryVerdict(undefined).attemptSucceeded).toBe(false);
    expect(recoveryVerdict("recovered!").attemptSucceeded).toBe(false);
    expect(recoveryVerdict({}).attemptSucceeded).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// Manual-test-loop iteration 10, item 4 — the last laundering edge, since
// closed AT THE SEAM.
//
// `/ai/recovery/attempt` answered a typed refusal with HTTP 200
// `{"success":true,"recovered":false}`. The frontend DID send
// `{success:false, error:"RECOVERY_UNSCOPED: …", data:{recovered:false}}` (that field is
// `attemptSucceeded` since plan 2026-08-23-single-source-derived-facts item 8),
// but the runner's response dispatcher forwarded only `response.data` when the
// handler supplied one — so `success` and `error` never reached the HTTP layer.
// `extract_response_data` (`src-tauri/src/mcp/ui_bridge/request.rs`) now stamps
// the envelope's verdict onto `data` for every handler, so this payload no
// longer mirrors `success`/`error` — duplicating them here would only be a
// second place for the wording to drift.
// ---------------------------------------------------------------------------
describe("recoveryFailureData (item 4)", () => {
  it("carries the verdict fields the envelope cannot, and nothing it already carries", () => {
    expect(recoveryFailureData(RECOVERY_UNSCOPED)).toEqual({
      code: RECOVERY_UNSCOPED,
      attemptSucceeded: false,
    });
  });

  it("carries the caller's context through without letting it overwrite the verdict", () => {
    const out = recoveryFailureData(RECOVERY_TARGET_MISSING, {
      elementId: "btn-1",
      // Hostile/mistaken context: both keys must lose to the failure verdict.
      attemptSucceeded: true,
      code: "SOMETHING_ELSE",
    });
    expect(out.elementId).toBe("btn-1");
    expect(out.attemptSucceeded).toBe(false);
    expect(out.code).toBe(RECOVERY_TARGET_MISSING);
  });

  it("never reports attemptSucceeded:true — a failure payload has exactly one verdict", () => {
    for (const code of [RECOVERY_UNSCOPED, RECOVERY_TARGET_MISSING, RECOVERY_FAILED]) {
      expect(recoveryFailureData(code).attemptSucceeded).toBe(false);
    }
  });
});

describe("originalActionFacts (plan 2026-11-18 Phase 3)", () => {
  it("states nothing ran when the executor was never reached", () => {
    expect(originalActionFacts(false, true, false)).toEqual({
      originalActionAttempted: false,
      originalActionPerformed: false,
    });
    expect(originalActionFacts(false, false, false)).toEqual({
      originalActionAttempted: false,
      originalActionPerformed: false,
    });
  });

  it("states the original ran and was performed when the executor succeeded", () => {
    expect(originalActionFacts(true, true, true)).toEqual({
      originalActionAttempted: true,
      originalActionPerformed: true,
    });
  });

  it("states the original was ATTEMPTED, not performed, when it failed or threw", () => {
    expect(originalActionFacts(true, true, false)).toEqual({
      originalActionAttempted: true,
      originalActionPerformed: false,
    });
  });

  it("omits both (UNKNOWN) when no original instruction was named", () => {
    const facts = originalActionFacts(true, false, true);
    expect(facts).toEqual({});
    expect("originalActionAttempted" in facts).toBe(false);
    expect("originalActionPerformed" in facts).toBe(false);
  });

  it("rides a failure payload without touching its verdict", () => {
    const data = recoveryFailureData(RECOVERY_FAILED, originalActionFacts(true, true, false));
    expect(data).toEqual({
      originalActionAttempted: true,
      originalActionPerformed: false,
      code: RECOVERY_FAILED,
      attemptSucceeded: false,
    });
  });
});

describe("the dispatch record (review A1/A2: execute() never throws)", () => {
  const makeBridge = () => ({
    executeAction: vi.fn(async () => ({ success: true })),
    executeComponentAction: vi.fn(async () => ({ success: true })),
    fillForm: vi.fn(async () => ({ success: true })),
  });

  it("turns `dispatched` true only once a call passes the guard", async () => {
    const bridge = makeBridge();
    const record = newRecoveryDispatchRecord();
    const scoped = scopeRecoveryExecutor(bridge, "btn-1", record);
    await expect(scoped.executeAction("btn-1", { action: "type" })).rejects.toBeInstanceOf(
      RecoveryRefusedError,
    );
    expect(record.dispatched).toBe(false);
    expect(record.refusal?.code).toBe(RECOVERY_WRITE_REFUSED);
    await expect(scoped.fillForm()).rejects.toBeInstanceOf(RecoveryRefusedError);
    expect(record.refusal?.code).toBe(RECOVERY_OUT_OF_SCOPE);
    await scoped.executeAction("btn-1", { action: "focus" });
    expect(record.dispatched).toBe(true);
  });

  it("undispatchedRefusal: the guard's refusal, else NOT_DISPATCHED, else null", () => {
    const record = newRecoveryDispatchRecord();
    expect(undispatchedRefusal(record, "UB-ELEM-NOT-FOUND: no match")).toEqual({
      code: RECOVERY_NOT_DISPATCHED,
      error: `${RECOVERY_NOT_DISPATCHED}: UB-ELEM-NOT-FOUND: no match`,
    });
    record.refusal = new RecoveryRefusedError(RECOVERY_WRITE_REFUSED, "refused");
    expect(undispatchedRefusal(record, null)?.code).toBe(RECOVERY_WRITE_REFUSED);
    record.dispatched = true;
    expect(undispatchedRefusal(record, null)).toBeNull();
  });

  // The REAL executor, so the wiring is tested rather than hand-picked flags:
  // it RETURNS a failure for the guard's refusal and for a miss, and only a
  // call that passed the guard reaches the bridge.
  const element = {
    id: "save-btn",
    type: "button",
    label: "Save",
    tagName: "button",
    accessibleName: "Save",
    actions: ["click", "focus", "type"],
    state: { visible: true, enabled: true, focused: false },
  };
  const run = async (instruction: string) => {
    const bridge = makeBridge();
    const record = newRecoveryDispatchRecord();
    const executor = new NLActionExecutor();
    executor.updateElements([element] as never);
    executor.setActionExecutor(scopeRecoveryExecutor(bridge, "save-btn", record) as never);
    const result = await executor.execute({ instruction });
    return { bridge, record, result };
  };

  it("a write instruction is refused inside execute(): nothing dispatched", async () => {
    const { bridge, record, result } = await run('type "hello" into the Save button');
    expect(recoveryVerdict(result).attemptSucceeded).toBe(false);
    expect(bridge.executeAction).not.toHaveBeenCalled();
    expect(record.dispatched).toBe(false);
    expect(record.refusal?.code).toBe(RECOVERY_WRITE_REFUSED);
    expect(undispatchedRefusal(record, null)?.code).toBe(RECOVERY_WRITE_REFUSED);
  });

  it("an instruction that matches nothing dispatches nothing", async () => {
    const { bridge, record, result } = await run("click the nonexistent purple elephant widget");
    expect(recoveryVerdict(result).attemptSucceeded).toBe(false);
    expect(bridge.executeAction).not.toHaveBeenCalled();
    expect(undispatchedRefusal(record, recoveryVerdict(result).reason)?.code).toBe(
      RECOVERY_NOT_DISPATCHED,
    );
  });

  it("an in-scope click dispatches, so a failure would be RECOVERY_FAILED (no refusal)", async () => {
    const { bridge, record } = await run("click the Save button");
    expect(bridge.executeAction).toHaveBeenCalled();
    expect(record.dispatched).toBe(true);
    expect(undispatchedRefusal(record, null)).toBeNull();
  });
});
