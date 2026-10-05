import { describe, it, expect } from "vitest";

import { pageDemoScriptStep } from "./pageDemoScript";
import { appliedStatuses, page, stubContext } from "./stubContext.testutil";

describe("pageDemoScriptStep", () => {
  it("marks the active step done and advances through ctx.flow on the LIVE context", () => {
    const ctx = stubContext();
    // The flow function reads the page queue when it runs. Set the queue after
    // ctx is built, and record what the flow actually saw.
    let queueSeen: string[] | null = null;
    ctx.flow.advanceToNextPage.mockImplementation(() => {
      queueSeen = ctx.pageQueueRef.current.map((p) => p.route);
    });
    ctx.pageQueueRef.current = [page({ route: "/next" })];

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
    expect(ctx.flow.advanceToNextPage).toHaveBeenCalledWith(ctx.controller.signal);
    expect(queueSeen).toEqual(["/next"]);
    // Not an AI step: the processed-message cursor is untouched.
    expect(ctx.processedMessageCountRef.current).toBe(0);
  });
});
