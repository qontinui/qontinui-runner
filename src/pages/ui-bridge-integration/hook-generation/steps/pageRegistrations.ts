import { buildPageSpecPrompt, buildTutorialPrompt } from "@/lib/page-analysis-prompt-builder";
import { extractGeneratedFiles } from "../parse";
import { readProjectFile } from "../flow/readProjectFile";
import type { StepHandler } from "./types";

/** `page-registrations`: collect the registration files, then chain to the page's next enabled step. */
export const pageRegistrationsStep: StepHandler = (ctx, { aiMessages, fullContent }) => {
  ctx.processedMessageCountRef.current = aiMessages.length; // Mark messages as processed
  const files = extractGeneratedFiles(fullContent);
  if (files.length > 0) {
    ctx.setGeneratedFiles((prev) => [...prev, ...files]);
    // Store registration output for spec/tutorial prompts
    if (ctx.currentPageRef.current) {
      ctx.currentPageRef.current.registrationOutput = files.map((f) => f.content).join("\n\n");
    }
    ctx.setStepStatuses((prev) =>
      prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
    );
  }

  // Chain to page-spec if enabled
  const opts = ctx.pageOptionsRef.current;
  const cur = ctx.currentPageRef.current;
  if (opts.generateSpecs && cur?.source) {
    ctx.pendingStepRef.current = "page-spec";
    ctx.setPhase("generating-page-spec");
    ctx.setStepStatuses((prev) => [...prev, { state: "active", label: `Spec: ${cur.page.route}` }]);

    // Load existing spec for merge mode if available
    (async () => {
      let existingSpec: string | undefined;
      if (cur.page.has_spec) {
        const specName = `${cur.page.route.replace(/^\//, "").replace(/\//g, "-") || "root"}.spec.uibridge.json`;
        existingSpec =
          (await readProjectFile(
            ctx.projectPath,
            `src/specs/${specName}`,
            ctx.controller.signal,
          )) ||
          (await readProjectFile(ctx.projectPath, specName, ctx.controller.signal)) ||
          undefined;
      }
      const specPrompt = buildPageSpecPrompt(
        cur.source!.main_source,
        cur.source!.imported_sources,
        cur.page.component_name,
        cur.page.route,
        cur.registrationOutput || "",
        existingSpec,
      );
      ctx.sendMessage(
        "Now generate a page spec (.spec.uibridge.json) for this page.\n\n" + specPrompt,
      );
    })();
  } else if (opts.generateTutorials && cur?.source) {
    // Skip to tutorial
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
      "",
    );
    ctx.sendMessage("Now generate a tutorial for this page.\n\n" + tutPrompt);
  } else if (opts.generateArchitectureDiagrams && cur?.source) {
    ctx.flow.chainToArchitectureDiagram(cur);
  } else if (opts.generateDemoVideos || opts.generateProductTours) {
    // Skip to demo script / product tour planning
    ctx.flow.chainToDemoScript(cur?.page.route ?? "", ctx.controller.signal);
  } else {
    // Advance to next page
    ctx.flow.advanceToNextPage(ctx.controller.signal);
  }
};
