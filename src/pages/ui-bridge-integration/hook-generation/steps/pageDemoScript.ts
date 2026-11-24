import type { StepHandler } from "./types";

/** `page-demo-script`: the completion signal of the (non-AI) demo/tour planning step. */
export const pageDemoScriptStep: StepHandler = (ctx) => {
  // Demo script planning is handled inline (non-AI) — this case handles
  // the completion signal. The script was already added to demoVideoScriptsRef
  // by chainToDemoScript. Just advance.
  ctx.setStepStatuses((prev) =>
    prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
  );
  ctx.flow.advanceToNextPage(ctx.controller.signal);
};
