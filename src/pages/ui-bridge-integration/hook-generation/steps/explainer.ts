import { extractMarkdownFiles } from "../parse";
import type { StepHandler } from "./types";

/** `explainer-index` / `explainer-cluster` / `explainer-page`: collect the markdown file, then advance the explainer queue. */
export const explainerStep: StepHandler = (ctx, { aiMessages, fullContent }) => {
  ctx.processedMessageCountRef.current = aiMessages.length;
  const files = extractMarkdownFiles(fullContent);
  if (files.length > 0) {
    ctx.setGeneratedFiles((prev) => [...prev, ...files]);
    ctx.setStepStatuses((prev) =>
      prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
    );
  } else {
    ctx.setStepStatuses((prev) =>
      prev.map((s) => (s.state === "active" ? { ...s, state: "error" } : s)),
    );
  }
  ctx.flow.advanceExplainerQueue();
};
