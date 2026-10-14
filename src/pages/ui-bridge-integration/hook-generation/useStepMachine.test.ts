/**
 * createStepContext wires `ctx.flow` to the real flow functions bound to that
 * same ctx, so arms and flow functions reach each other — and the live refs —
 * through one context.
 */

import { describe, it, expect, vi } from "vitest";

import { createStepContext } from "./useStepMachine";
import { page, stubContext } from "./steps/stubContext.testutil";

function realFlowContext() {
  const { flow: _stubFlow, ...base } = stubContext();
  return createStepContext(base);
}

describe("createStepContext", () => {
  it("binds every flow function", () => {
    const ctx = realFlowContext();
    expect(Object.keys(ctx.flow).sort()).toEqual(
      [
        "advanceExplainerQueue",
        "advanceToNextPage",
        "chainToArchitectureDiagram",
        "chainToDemoScript",
        "maybeStartExplainerPhase",
        "startPageGeneration",
      ].sort(),
    );
  });

  it("flow.advanceExplainerQueue reads the explainer refs at call time", () => {
    const ctx = realFlowContext();
    // Set after ctx is built: a running explainer with one index item queued.
    ctx.explainerContextRef.current = {
      specs: [],
      arch: new Map(),
      clusters: [],
      projectName: "Proj",
    };
    ctx.explainerQueueRef.current = [{ kind: "index" }];

    ctx.flow.advanceExplainerQueue();

    expect(ctx.pendingStepRef.current).toBe("explainer-index");
    expect(ctx.explainerCurrentRef.current).toEqual({ kind: "index" });
    expect(ctx.explainerQueueRef.current).toEqual([]);
    expect(ctx.explainerCallsInSessionRef.current).toBe(1);
    expect(ctx.setPhase).toHaveBeenCalledWith("generating-project-explainer");
    expect(vi.mocked(ctx.sendMessage)).toHaveBeenCalledTimes(1);
  });

  it("flow.advanceToNextPage with a drained queue finishes the run on the live refs", async () => {
    const ctx = realFlowContext();
    ctx.pendingStepRef.current = "page-spec";
    ctx.currentPageRef.current = { page: page(), source: null, registrationOutput: "" };

    ctx.flow.advanceToNextPage(ctx.controller.signal);
    await Promise.resolve();
    await Promise.resolve();

    expect(ctx.pendingStepRef.current).toBeNull();
    expect(ctx.currentPageRef.current).toBeNull();
    expect(ctx.setCurrentPageName).toHaveBeenCalledWith("");
    expect(ctx.setPhase).toHaveBeenCalledWith("preview");
  });
});
