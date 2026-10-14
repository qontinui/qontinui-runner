import { describe, it, expect } from "vitest";

import { pageTutorialStep } from "./pageTutorial";
import { pageArchitectureDiagramStep } from "./pageArchitectureDiagram";
import {
  NO_PAGE_OPTIONS,
  aiMessage,
  appliedFiles,
  appliedStatuses,
  contentOf,
  currentPage,
  stubContext,
} from "./stubContext.testutil";

describe("pageTutorialStep", () => {
  it("collects the tutorial files, marks done, and chains on the LIVE options", () => {
    const ctx = stubContext();
    // Set after ctx is built.
    const cur = currentPage();
    ctx.currentPageRef.current = cur;
    ctx.pageOptionsRef.current = { ...NO_PAGE_OPTIONS, generateArchitectureDiagrams: true };

    pageTutorialStep(
      ctx,
      contentOf([aiMessage("```ts\n// FILE: src/tutorial/data/settings.ts\nexport {};\n```")]),
    );

    expect(ctx.processedMessageCountRef.current).toBe(1);
    expect(appliedFiles(ctx, []).map((f) => f.filePath)).toEqual(["src/tutorial/data/settings.ts"]);
    expect(appliedStatuses(ctx, [{ state: "active", label: "Tutorial" }])).toEqual([
      { state: "done", label: "Tutorial" },
    ]);
    expect(ctx.flow.chainToArchitectureDiagram).toHaveBeenCalledWith(cur);
  });
});

describe("pageArchitectureDiagramStep", () => {
  it("collects the Mermaid file and advances when demo/tour are off", () => {
    const ctx = stubContext();
    ctx.currentPageRef.current = currentPage();

    pageArchitectureDiagramStep(
      ctx,
      contentOf([
        aiMessage("```mermaid\n%% FILE: src/specs/architecture/x.arch.mmd\ngraph TD\n```"),
      ]),
    );

    expect(appliedFiles(ctx, []).map((f) => f.filePath)).toEqual([
      "src/specs/architecture/x.arch.mmd",
    ]);
    expect(appliedStatuses(ctx, [{ state: "active", label: "Diagram" }])[0].state).toBe("done");
    expect(ctx.flow.advanceToNextPage).toHaveBeenCalledWith(ctx.controller.signal);
  });

  it("marks the step errored without a Mermaid block and chains to demo on the LIVE options", () => {
    const ctx = stubContext();
    // Set after ctx is built.
    ctx.currentPageRef.current = currentPage();
    ctx.pageOptionsRef.current = { ...NO_PAGE_OPTIONS, generateDemoVideos: true };

    pageArchitectureDiagramStep(ctx, contentOf([aiMessage("nothing")]));

    expect(ctx.setGeneratedFiles).not.toHaveBeenCalled();
    expect(appliedStatuses(ctx, [{ state: "active", label: "Diagram" }])[0].state).toBe("error");
    expect(ctx.flow.chainToDemoScript).toHaveBeenCalledWith(
      "/dashboard/settings",
      ctx.controller.signal,
    );
  });
});
