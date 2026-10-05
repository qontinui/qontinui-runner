import { extractGeneratedFiles } from "../parse";
import type { StepHandler } from "./types";

/** `page-tutorial`: collect the tutorial files, then chain to the page's next enabled step. */
export const pageTutorialStep: StepHandler = (ctx, { aiMessages, fullContent }) => {
  ctx.processedMessageCountRef.current = aiMessages.length;
  const files = extractGeneratedFiles(fullContent);
  if (files.length > 0) {
    ctx.setGeneratedFiles((prev) => [...prev, ...files]);
  }
  ctx.setStepStatuses((prev) =>
    prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
  );
  const opts = ctx.pageOptionsRef.current;
  const cur = ctx.currentPageRef.current;
  if (opts.generateArchitectureDiagrams && cur?.source) {
    ctx.flow.chainToArchitectureDiagram(cur);
  } else if (opts.generateDemoVideos || opts.generateProductTours) {
    ctx.flow.chainToDemoScript(cur?.page.route ?? "", ctx.controller.signal);
  } else {
    ctx.flow.advanceToNextPage(ctx.controller.signal);
  }
};
