/**
 * Per-page flow of the hook-generation step machine: start a page, chain its
 * optional architecture-diagram and demo/tour steps, and advance the page
 * queue (rotating the AI session at the batch boundary, then recording demo
 * videos and saving product tours when the queue drains).
 *
 * Moved verbatim from `handleSessionTransition`'s nested helpers; closure
 * variables became `ctx.*` and calls to sibling helpers became `ctx.flow.*`.
 */

import { fetchRegisteredElements, planDemoScript } from "@/lib/demo-video/script-planner";
import { executeScript } from "@/lib/demo-video/script-executor";
import { generateNarration } from "@/lib/demo-video/narration-generator";
import { DEFAULT_RECORDING_CONFIG } from "@/lib/demo-video/types";
import { generateTour } from "@/lib/product-tour/tour-generator";
import type { ProductTour } from "@/types/product-tour";
import { PRODUCT_TOURS_STORAGE_KEY } from "@/types/product-tour";
import { instanceStorage } from "@/lib/instance-storage";
import type { SpecConfig } from "@/lib/spec-prompt-builder";
import {
  buildRegistrationPrompt,
  extractInlineRegistrations,
  buildPageSpecPrompt,
  buildTutorialPrompt,
  buildArchitectureDiagramPrompt,
} from "@/lib/page-analysis-prompt-builder";
import type { PageComponent } from "../../types";
import { readPageSource } from "../../integrationApi";
import { readProjectFile } from "./readProjectFile";
import { recordPreviewPrompt } from "../previewPrompts";
import { SESSION_PAGE_BUDGET, type CurrentPage, type StepContext } from "../steps/types";

// Helper: chain to the architecture-diagram step. Uses the same source
// + registrations we already fetched for this page.
export function chainToArchitectureDiagram(ctx: StepContext, cur: CurrentPage) {
  if (!cur.source) {
    ctx.flow.advanceToNextPage(ctx.controller.signal);
    return;
  }
  ctx.pendingStepRef.current = "page-architecture-diagram";
  ctx.setPhase("generating-page-architecture-diagram");
  ctx.setStepStatuses((prev) => [
    ...prev,
    { state: "active", label: `Architecture diagram: ${cur.page.route}` },
  ]);
  const archPrompt = buildArchitectureDiagramPrompt(
    cur.source.main_source,
    cur.source.imported_sources,
    cur.page.component_name,
    cur.page.route,
    cur.registrationOutput || "",
  );
  ctx.sendMessage(`Now generate the architecture diagram for this page.\n\n` + archPrompt);
}

// Helper: chain to demo script + product tour planning (non-AI — calls planner APIs directly)
// Exposed via ref so handleGeneratePages can invoke it for demo/tour-only
// re-runs that have no AI prompt to anchor the first-page flow on.
export function chainToDemoScript(ctx: StepContext, route: string, signal: AbortSignal) {
  ctx.pendingStepRef.current = "page-demo-script";
  const opts = ctx.pageOptionsRef.current;
  const parts = [opts.generateDemoVideos && "Demo", opts.generateProductTours && "Tour"].filter(
    Boolean,
  );
  ctx.setStepStatuses((prev) => [
    ...prev,
    { state: "active", label: `${parts.join(" + ")}: ${route}` },
  ]);

  (async () => {
    // If this run didn't generate specs (demo/tour-only re-run), the
    // in-memory lastSpecJsonRef is either empty or leftover from a
    // previous page — neither is right for the current page. Always
    // re-read the current page's spec from disk in that case so each
    // page's demo/tour uses its own spec.
    if (!opts.generateSpecs) {
      const slug = route.replace(/^\//, "").replace(/\//g, "-") || "root";
      const existing = await readProjectFile(
        ctx.projectPath,
        `src/specs/${slug}.spec.uibridge.json`,
        signal,
      );
      if (signal.aborted) return;
      ctx.lastSpecJsonRef.current = existing || "";
    }
    // Parse the last generated spec JSON into a SpecConfig
    let specConfig: SpecConfig | null = null;
    if (ctx.lastSpecJsonRef.current) {
      try {
        specConfig = JSON.parse(ctx.lastSpecJsonRef.current) as SpecConfig;
      } catch {
        // Spec JSON couldn't be parsed
      }
    }

    if (specConfig) {
      const elements = await fetchRegisteredElements();

      // Demo video script
      if (opts.generateDemoVideos) {
        try {
          const script = await planDemoScript(specConfig, elements);
          ctx.demoVideoScriptsRef.current.push(script);
        } catch (err) {
          console.warn("Demo script planning failed for", route, err);
        }
      }

      // Product tour
      if (opts.generateProductTours) {
        try {
          const tour = await generateTour(specConfig, elements, "new-user");
          ctx.productToursRef.current.push(tour);
        } catch (err) {
          console.warn("Product tour generation failed for", route, err);
        }
      }
    }

    ctx.setStepStatuses((prev) =>
      prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
    );
    ctx.flow.advanceToNextPage(signal);
  })();
}

// Helper: advance to the next page in the queue or go to preview/recording
export function advanceToNextPage(ctx: StepContext, signal: AbortSignal) {
  const queue = ctx.pageQueueRef.current;
  if (queue.length > 0) {
    const nextPage = queue.shift()!;
    // Count the page we just finished (the one before this advance).
    ctx.pagesInSessionRef.current += 1;
    // Rotate to a fresh AI session at the batch boundary so context
    // doesn't grow unbounded on large projects. Each batch is
    // independent — the per-page prompts already include the page
    // source, so a fresh session doesn't lose information.
    if (ctx.pagesInSessionRef.current >= SESSION_PAGE_BUDGET) {
      ctx.pagesInSessionRef.current = 0;
      ctx.setStepStatuses((prev) => [
        ...prev,
        { state: "active", label: `Rotating to fresh AI session...` },
      ]);
      (async () => {
        if (signal.aborted) return;
        await ctx.session.close();
        if (signal.aborted) return;
        ctx.session.resetSession();
        const id = await ctx.session.createSession("Page Preparation: AI Generation (batch)");
        if (signal.aborted) return;
        if (!id) {
          ctx.setError("Failed to rotate AI session");
          return;
        }
        ctx.setStepStatuses((prev) =>
          prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
        );
        ctx.flow.startPageGeneration(nextPage, signal);
      })();
      return;
    }
    ctx.flow.startPageGeneration(nextPage, signal);
  } else {
    finishAllPages(ctx);
  }
}

// All pages done: save product tours, record any planned demo videos, then
// start the explainer phase or go to preview. (Split verbatim out of
// advanceToNextPage's `else` branch.)
function finishAllPages(ctx: StepContext) {
  ctx.pendingStepRef.current = null;
  ctx.currentPageRef.current = null;
  ctx.setCurrentPageName("");

  // Save product tours if any were generated
  const tours = ctx.productToursRef.current;
  if (tours.length > 0) {
    const existing = instanceStorage.getJSON<ProductTour[]>(PRODUCT_TOURS_STORAGE_KEY, []);
    const newIds = new Set(tours.map((t) => t.id));
    const merged = [...existing.filter((t) => !newIds.has(t.id)), ...tours];
    instanceStorage.setJSON(PRODUCT_TOURS_STORAGE_KEY, merged);
    ctx.productToursRef.current = [];
  }

  // If demo videos were planned, start batch recording
  const scripts = ctx.demoVideoScriptsRef.current;
  if (scripts.length > 0 && ctx.pageOptionsRef.current.generateDemoVideos) {
    ctx.setStepStatuses((prev) => [
      ...prev,
      {
        state: "active",
        label: `Recording ${scripts.length} demo video${scripts.length !== 1 ? "s" : ""}...`,
      },
    ]);
    (async () => {
      for (let i = 0; i < scripts.length; i++) {
        const script = scripts[i];
        try {
          const result = await executeScript(script, DEFAULT_RECORDING_CONFIG);
          const narr = generateNarration(script, result);
          ctx.setGeneratedFiles((prev) => [
            ...prev,
            {
              filePath: `${script.targetPage.replace(/^\//, "").replace(/\//g, "-") || "demo"}-narration.srt`,
              content: narr.srt,
            },
            {
              filePath: `${script.targetPage.replace(/^\//, "").replace(/\//g, "-") || "demo"}-narration.md`,
              content: narr.markdown,
            },
          ]);
        } catch (err) {
          console.warn(`Demo video recording failed for ${script.title}:`, err);
        }
      }
      ctx.demoVideoScriptsRef.current = [];
      ctx.setStepStatuses((prev) =>
        prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
      );
      if (!(await ctx.flow.maybeStartExplainerPhase())) {
        ctx.setPhase("preview");
      }
    })();
  } else {
    (async () => {
      if (!(await ctx.flow.maybeStartExplainerPhase())) {
        ctx.setPhase("preview");
      }
    })();
  }
}

export async function startPageGeneration(
  ctx: StepContext,
  page: PageComponent,
  signal: AbortSignal,
) {
  ctx.currentPageRef.current = { page, source: null, registrationOutput: "" };
  ctx.setCurrentPageName(page.route);
  ctx.setStepStatuses((prev) => [
    ...prev,
    { state: "active", label: `Registrations: ${page.route}` },
  ]);

  // Fetch page source
  try {
    const data = await readPageSource(
      { projectPath: ctx.projectPath, componentPath: page.component_path, maxDepth: 2 },
      signal,
    );
    if (signal.aborted) return;
    if (data.success && data.data) {
      ctx.currentPageRef.current!.source = data.data;
    }
  } catch {
    if (signal.aborted) return;
  }

  const source = ctx.currentPageRef.current?.source;
  if (!source) {
    ctx.setStepStatuses((prev) =>
      prev.map((s) => (s.state === "active" ? { ...s, state: "error" } : s)),
    );
    ctx.flow.advanceToNextPage(signal);
    return;
  }

  const opts = ctx.pageOptionsRef.current;
  if (opts.generateRegistrations) {
    ctx.pendingStepRef.current = "page-registrations";
    ctx.setPhase("generating-page-registrations");

    // Load existing registrations for merge mode
    let existingRegs: string | undefined;
    if (page.has_registrations) {
      const pageName = page.component_name.toLowerCase().replace(/page$/, "");
      existingRegs = await readProjectFile(
        ctx.projectPath,
        `src/lib/ui-bridge/pages/${pageName}-registrations.tsx`,
        signal,
      );
      if (!existingRegs) {
        // Try reading from the component file itself (inline registrations)
        existingRegs = undefined;
      }
    }

    const prompt = buildRegistrationPrompt(
      source.main_source,
      source.imported_sources,
      page.component_name,
      page.route,
      (ctx.effectiveAnalysisRef.current ?? ctx.analysis).framework,
      existingRegs || undefined,
      // Inline `useUIElement` / `useUIComponent` calls the page already
      // contains. Without this, the side-file the LLM emits would
      // duplicate them and runtime would double-register.
      extractInlineRegistrations(source.main_source),
    );
    if (typeof window !== "undefined") recordPreviewPrompt(page.route, page.component_name, prompt);
    await ctx.sendMessage(
      `Analyze the page at ${page.route} and generate UI Bridge registrations.\n\n` + prompt,
    );
  } else if (opts.generateSpecs) {
    ctx.pendingStepRef.current = "page-spec";
    ctx.setPhase("generating-page-spec");
    ctx.setStepStatuses((prev) => [
      ...prev.filter((s) => s.state !== "active"),
      { state: "active", label: `Spec: ${page.route}` },
    ]);
    const specPrompt = buildPageSpecPrompt(
      source.main_source,
      source.imported_sources,
      page.component_name,
      page.route,
      "",
    );
    await ctx.sendMessage(`Generate a page spec for ${page.route}.\n\n` + specPrompt);
  } else if (opts.generateTutorials) {
    ctx.pendingStepRef.current = "page-tutorial";
    ctx.setPhase("generating-page-tutorial");
    ctx.setStepStatuses((prev) => [
      ...prev.filter((s) => s.state !== "active"),
      { state: "active", label: `Tutorial: ${page.route}` },
    ]);
    const tutPrompt = buildTutorialPrompt(
      source.main_source,
      page.component_name,
      page.route,
      "",
      "",
    );
    await ctx.sendMessage(`Generate a tutorial for ${page.route}.\n\n` + tutPrompt);
  } else if (opts.generateArchitectureDiagrams) {
    ctx.pendingStepRef.current = "page-architecture-diagram";
    ctx.setPhase("generating-page-architecture-diagram");
    ctx.setStepStatuses((prev) => [
      ...prev.filter((s) => s.state !== "active"),
      { state: "active", label: `Architecture diagram: ${page.route}` },
    ]);
    const archPrompt = buildArchitectureDiagramPrompt(
      source.main_source,
      source.imported_sources,
      page.component_name,
      page.route,
      "",
    );
    await ctx.sendMessage(`Generate an architecture diagram for ${page.route}.\n\n` + archPrompt);
  } else if (opts.generateDemoVideos || opts.generateProductTours) {
    // Only demo videos / product tours selected — chain directly
    ctx.flow.chainToDemoScript(page.route, signal);
  }
}
