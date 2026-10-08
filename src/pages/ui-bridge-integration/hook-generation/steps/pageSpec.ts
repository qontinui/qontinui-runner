import { buildTutorialPrompt } from "@/lib/page-analysis-prompt-builder";
import { extractJsonBlock } from "../parse";
import type { StepHandler } from "./types";

/** `page-spec`: store the page's spec JSON, then chain to the page's next enabled step. */
export const pageSpecStep: StepHandler = (ctx, { aiMessages, fullContent }) => {
  ctx.processedMessageCountRef.current = aiMessages.length;
  const jsonBlock = extractJsonBlock(fullContent);
  if (jsonBlock) {
    ctx.lastSpecJsonRef.current = jsonBlock;
    const specName = ctx.currentPageRef.current
      ? `${ctx.currentPageRef.current.page.route.replace(/^\//, "").replace(/\//g, "-") || "root"}.spec.uibridge.json`
      : "page.spec.uibridge.json";
    ctx.setGeneratedFiles((prev) => [...prev, { filePath: specName, content: jsonBlock }]);
    ctx.setStepStatuses((prev) =>
      prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
    );
  }

  // Chain to tutorial if enabled
  const opts = ctx.pageOptionsRef.current;
  const cur = ctx.currentPageRef.current;
  if (opts.generateTutorials && cur?.source) {
    ctx.pendingStepRef.current = "page-tutorial";
    ctx.setPhase("generating-page-tutorial");
    ctx.setStepStatuses((prev) => [
      ...prev,
      { state: "active", label: `Tutorial: ${cur.page.route}` },
    ]);
    const tutPrompt = buildTutorialPrompt(
      cur.source.main_source,
      cur.page.component_name,
      cur.page.route,
      cur.registrationOutput || "",
      jsonBlock || "",
    );
    ctx.sendMessage("Now generate a tutorial for this page.\n\n" + tutPrompt);
  } else if (opts.generateArchitectureDiagrams && cur?.source) {
    ctx.flow.chainToArchitectureDiagram(cur);
  } else if (opts.generateDemoVideos || opts.generateProductTours) {
    ctx.flow.chainToDemoScript(cur?.page.route ?? "", ctx.controller.signal);
  } else {
    ctx.flow.advanceToNextPage(ctx.controller.signal);
  }
};
