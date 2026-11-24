import { describe, it, expect } from "vitest";

import { pageSpecStep } from "./pageSpec";
import {
  NO_PAGE_OPTIONS,
  aiMessage,
  appliedFiles,
  appliedStatuses,
  contentOf,
  currentPage,
  stubContext,
} from "./stubContext.testutil";

const SPEC_JSON = '{"id":"settings","groups":[]}';
const SPEC_TURN = aiMessage(["Spec:", "```json", SPEC_JSON, "```"].join("\n"));

describe("pageSpecStep", () => {
  it("names the spec from the LIVE current page, stores it, and chains to the diagram", () => {
    const ctx = stubContext();
    // Set after ctx is built: the handler must read them at call time.
    const cur = currentPage();
    ctx.currentPageRef.current = cur;
    ctx.pageOptionsRef.current = { ...NO_PAGE_OPTIONS, generateArchitectureDiagrams: true };
    ctx.lastSpecJsonRef.current = "stale";

    pageSpecStep(ctx, contentOf([aiMessage("regs"), SPEC_TURN]));

    expect(ctx.processedMessageCountRef.current).toBe(2);
    expect(ctx.lastSpecJsonRef.current).toBe(SPEC_JSON);
    expect(appliedFiles(ctx, [])).toEqual([
      { filePath: "dashboard-settings.spec.uibridge.json", content: SPEC_JSON },
    ]);
    expect(appliedStatuses(ctx, [{ state: "active", label: "Spec: /dashboard/settings" }])).toEqual(
      [{ state: "done", label: "Spec: /dashboard/settings" }],
    );
    expect(ctx.flow.chainToArchitectureDiagram).toHaveBeenCalledWith(cur);
  });

  it("chains to the tutorial, passing the spec JSON into the prompt", () => {
    const ctx = stubContext();
    ctx.currentPageRef.current = currentPage();
    ctx.pageOptionsRef.current = { ...NO_PAGE_OPTIONS, generateTutorials: true };

    pageSpecStep(ctx, contentOf([SPEC_TURN]));

    expect(ctx.pendingStepRef.current).toBe("page-tutorial");
    expect(ctx.setPhase).toHaveBeenCalledWith("generating-page-tutorial");
    expect(ctx.sendMessage.mock.calls[0][0]).toMatch(/^Now generate a tutorial for this page\./);
    expect(ctx.sendMessage.mock.calls[0][0]).toContain(SPEC_JSON);
  });

  it("falls back to page.spec.uibridge.json without a current page, then advances", () => {
    const ctx = stubContext();

    pageSpecStep(ctx, contentOf([SPEC_TURN]));

    expect(appliedFiles(ctx, []).map((f) => f.filePath)).toEqual(["page.spec.uibridge.json"]);
    expect(ctx.flow.advanceToNextPage).toHaveBeenCalledWith(ctx.controller.signal);
  });

  it("without a JSON block leaves lastSpecJsonRef alone and still chains", () => {
    const ctx = stubContext();
    ctx.lastSpecJsonRef.current = "previous";
    ctx.currentPageRef.current = currentPage();
    ctx.pageOptionsRef.current = { ...NO_PAGE_OPTIONS, generateDemoVideos: true };

    pageSpecStep(ctx, contentOf([aiMessage("no json")]));

    expect(ctx.lastSpecJsonRef.current).toBe("previous");
    expect(ctx.setGeneratedFiles).not.toHaveBeenCalled();
    expect(ctx.flow.chainToDemoScript).toHaveBeenCalledWith(
      "/dashboard/settings",
      ctx.controller.signal,
    );
  });
});
