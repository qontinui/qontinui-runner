import { describe, it, expect } from "vitest";

import { explainerStep } from "./explainer";
import {
  aiMessage,
  appliedFiles,
  appliedStatuses,
  contentOf,
  stubContext,
} from "./stubContext.testutil";

describe("explainerStep", () => {
  it("collects the markdown file, marks done, and advances the explainer queue", () => {
    const ctx = stubContext();
    // The cursor is overwritten with the AI message count, not incremented.
    ctx.processedMessageCountRef.current = 99;

    explainerStep(
      ctx,
      contentOf([
        aiMessage("old"),
        aiMessage("<!-- FILE: src/specs/explainer/index.md -->\n# Project\n\nOverview."),
      ]),
    );

    expect(ctx.processedMessageCountRef.current).toBe(2);
    expect(appliedFiles(ctx, [])).toEqual([
      {
        filePath: "src/specs/explainer/index.md",
        content: "<!-- FILE: src/specs/explainer/index.md -->\n# Project\n\nOverview.\n",
      },
    ]);
    expect(appliedStatuses(ctx, [{ state: "active", label: "Explainer: index.md" }])).toEqual([
      { state: "done", label: "Explainer: index.md" },
    ]);
    expect(ctx.flow.advanceExplainerQueue).toHaveBeenCalledTimes(1);
  });

  it("marks the step errored when no FILE comment came back, and still advances", () => {
    const ctx = stubContext();

    explainerStep(ctx, contentOf([aiMessage("Sorry, no file.")]));

    expect(ctx.setGeneratedFiles).not.toHaveBeenCalled();
    expect(appliedStatuses(ctx, [{ state: "active", label: "Explainer: x.md" }])).toEqual([
      { state: "error", label: "Explainer: x.md" },
    ]);
    expect(ctx.flow.advanceExplainerQueue).toHaveBeenCalledTimes(1);
  });
});
