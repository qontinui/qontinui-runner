import { describe, it, expect, vi, afterEach } from "vitest";

vi.mock("@/lib/runner-api", () => ({ getApiBase: () => "http://runner.test" }));

import { architectureSpecStep } from "./architectureSpec";
import { aiMessage, appliedFiles, contentOf, stubContext } from "./stubContext.testutil";

const SPEC_TURN = aiMessage(
  ["Here is the spec:", "```json", '{"projectName":"My Cool App","pages":[]}', "```"].join("\n"),
);

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("architectureSpecStep", () => {
  it("adds the spec file named from projectName and goes to preview", () => {
    const ctx = stubContext({
      messages: [aiMessage("hooks turn"), SPEC_TURN],
      generatedFiles: [{ filePath: "src/hooks.tsx", content: "" }],
    });
    ctx.pendingStepRef.current = "architecture-spec";

    architectureSpecStep(ctx, contentOf(ctx.messages));

    expect(appliedFiles(ctx, [{ filePath: "src/hooks.tsx", content: "" }])).toEqual([
      { filePath: "src/hooks.tsx", content: "" },
      {
        filePath: "my-cool-app.architecture.uibridge.json",
        content: '{"projectName":"My Cool App","pages":[]}',
      },
    ]);
    expect(ctx.pendingStepRef.current).toBeNull();
    expect(ctx.setExpandedFiles).toHaveBeenCalledWith(new Set(["src/hooks.tsx", "spec"]));
    expect(ctx.setPhase).toHaveBeenCalledWith("preview");
    expect(ctx.setRetryTimer).not.toHaveBeenCalled();
  });

  it("reads ctx.specRetryTimerRef at call time and retries via the task-run output API", async () => {
    vi.useFakeTimers();
    const clearSpy = vi.spyOn(globalThis, "clearTimeout");
    const fetchMock = vi.fn(async () => ({
      json: async () => ({ output: '```json\n{"projectName":"Late"}\n```' }),
    }));
    vi.stubGlobal("fetch", fetchMock);

    const ctx = stubContext({ messages: [aiMessage("still streaming")], taskRunId: "run-7" });
    // A timer left by an EARLIER call, set after ctx was built.
    const earlier = setTimeout(() => {}, 60_000);
    ctx.specRetryTimerRef.current = earlier;

    architectureSpecStep(ctx, contentOf(ctx.messages));

    expect(clearSpy).toHaveBeenCalledWith(earlier);
    const timer = ctx.specRetryTimerRef.current;
    expect(timer).not.toBeNull();
    expect(timer).not.toBe(earlier);
    expect(ctx.setRetryTimer).toHaveBeenCalledWith(timer);

    await vi.advanceTimersByTimeAsync(1000);

    expect(fetchMock).toHaveBeenCalledWith(
      "http://runner.test/task-runs/run-7/output?tail_chars=200000",
      { signal: ctx.controller.signal },
    );
    expect(ctx.specRetryTimerRef.current).toBeNull();
    expect(ctx.setRetryTimer).toHaveBeenLastCalledWith(null);
    expect(appliedFiles(ctx, [])).toEqual([
      { filePath: "late.architecture.uibridge.json", content: '{"projectName":"Late"}' },
    ]);
    expect(ctx.setPhase).toHaveBeenCalledWith("preview");
    expect(ctx.setError).not.toHaveBeenCalled();
  });

  it("with no JSON and no task run, errors to preview after the retry delay", async () => {
    vi.useFakeTimers();
    const ctx = stubContext({ messages: [aiMessage("no json here")], taskRunId: undefined });
    ctx.pendingStepRef.current = "architecture-spec";

    architectureSpecStep(ctx, contentOf(ctx.messages));
    expect(ctx.setError).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1000);

    expect(ctx.setError).toHaveBeenCalledWith(
      "Architecture spec generation did not produce valid JSON. You can still apply the hook files.",
    );
    expect(ctx.pendingStepRef.current).toBeNull();
    expect(ctx.setPhase).toHaveBeenCalledWith("preview");
  });

  it("does nothing on the retry once the controller is aborted", async () => {
    vi.useFakeTimers();
    const ctx = stubContext({ messages: [aiMessage("no json")], taskRunId: undefined });
    architectureSpecStep(ctx, contentOf(ctx.messages));
    ctx.controller.abort();
    await vi.advanceTimersByTimeAsync(1000);
    expect(ctx.setError).not.toHaveBeenCalled();
    expect(ctx.setPhase).not.toHaveBeenCalled();
  });
});
