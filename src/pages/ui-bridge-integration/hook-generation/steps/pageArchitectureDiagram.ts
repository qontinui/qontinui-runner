import { extractMermaidFile } from "../parse";
import type { StepHandler } from "./types";

/** `page-architecture-diagram`: collect the Mermaid file, then chain to demo/tour or the next page. */
export const pageArchitectureDiagramStep: StepHandler = (ctx, { aiMessages, fullContent }) => {
  ctx.processedMessageCountRef.current = aiMessages.length;
  const diag = extractMermaidFile(fullContent);
  if (diag) {
    ctx.setGeneratedFiles((prev) => [...prev, diag]);
    ctx.setStepStatuses((prev) =>
      prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
    );
  } else {
    // No Mermaid block came back — mark the step errored but keep going.
    ctx.setStepStatuses((prev) =>
      prev.map((s) => (s.state === "active" ? { ...s, state: "error" } : s)),
    );
  }
  const opts = ctx.pageOptionsRef.current;
  const cur = ctx.currentPageRef.current;
  if (opts.generateDemoVideos || opts.generateProductTours) {
    ctx.flow.chainToDemoScript(cur?.page.route ?? "", ctx.controller.signal);
  } else {
    ctx.flow.advanceToNextPage(ctx.controller.signal);
  }
};
