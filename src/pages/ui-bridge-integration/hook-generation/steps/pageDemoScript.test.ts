import { describe, it, expect } from "vitest";

import { pageDemoScriptStep } from "./pageDemoScript";
import { appliedStatuses, stubContext } from "./stubContext.testutil";

describe("pageDemoScriptStep", () => {
  it("marks the active step done and advances with THIS call's controller signal", () => {
    const ctx = stubContext();
    // Swap the per-call controller in after construction, and park a different
    // controller in currentControllerRef: the handler must pass the signal of
    // the controller on ctx at call time, not a stale or ref-held one.
    const controller = new AbortController();
    ctx.controller = controller;
    ctx.currentControllerRef.current = new AbortController();

    pageDemoScriptStep(ctx, { aiMessages: [], fullContent: "" });

    expect(
      appliedStatuses(ctx, [
        { state: "done", label: "Registrations: /a" },
        { state: "active", label: "Demo + Tour: /a" },
      ]),
    ).toEqual([
      { state: "done", label: "Registrations: /a" },
      { state: "done", label: "Demo + Tour: /a" },
    ]);
    expect(ctx.flow.advanceToNextPage).toHaveBeenCalledTimes(1);
    // Identity: the exact signal of the controller on ctx at call time.
    expect(ctx.flow.advanceToNextPage.mock.calls[0][0]).toBe(controller.signal);
    // Not an AI step: the processed-message cursor is untouched.
    expect(ctx.processedMessageCountRef.current).toBe(0);
  });
});
