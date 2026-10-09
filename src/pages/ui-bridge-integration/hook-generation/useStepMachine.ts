/**
 * useStepMachine — owns HookGenerationPanel's AI step machine.
 *
 * Holds the step machine's refs (D3: refs are passed, not rehoused) and
 * returns `handleSessionTransition`, which on each processing → ready
 * transition gathers the AI content, builds a FRESH `StepContext` from the
 * current values, and dispatches to `STEP_HANDLERS[currentStep]`.
 */

import { useCallback, useEffect, useMemo, useRef } from "react";
import type { DemoScript } from "@/lib/demo-video/types";
import type { ProductTour } from "@/types/product-tour";
import type { PageComponent, PageGenerationOptions, ProjectAnalysis } from "../types";
import type { GeneratedFile } from "./parse";
import { STEP_HANDLERS } from "./steps";
import type {
  AiMessage,
  AiSession,
  CurrentPage,
  ExplainerContext,
  ExplainerQueueItem,
  IntegrationStep,
  StepContext,
  StepFlow,
  StepMachineRefs,
  StepMachineSetters,
} from "./steps/types";
import {
  advanceToNextPage,
  chainToArchitectureDiagram,
  chainToDemoScript,
  startPageGeneration,
} from "./flow/pageFlow";
import { advanceExplainerQueue, maybeStartExplainerPhase } from "./flow/explainerFlow";

/**
 * Wire `ctx.flow` to flow functions bound to `ctx` itself, so arms and flow
 * functions reach each other through the same context.
 */
export function createStepContext(base: Omit<StepContext, "flow">): StepContext {
  const flow = {} as StepFlow;
  const ctx: StepContext = { ...base, flow };
  flow.chainToArchitectureDiagram = (cur) => chainToArchitectureDiagram(ctx, cur);
  flow.chainToDemoScript = (route, signal) => chainToDemoScript(ctx, route, signal);
  flow.advanceToNextPage = (signal) => advanceToNextPage(ctx, signal);
  flow.startPageGeneration = (page, signal) => startPageGeneration(ctx, page, signal);
  flow.maybeStartExplainerPhase = () => maybeStartExplainerPhase(ctx);
  flow.advanceExplainerQueue = () => advanceExplainerQueue(ctx);
  return ctx;
}

/** Steps whose content is only the AI messages NEW since the last processed step. */
function isPerPageStep(currentStep: IntegrationStep): boolean {
  return (
    currentStep === "page-registrations" ||
    currentStep === "page-spec" ||
    currentStep === "page-tutorial" ||
    currentStep === "page-architecture-diagram" ||
    currentStep === "explainer-index" ||
    currentStep === "explainer-cluster" ||
    currentStep === "explainer-page"
  );
}

export interface SessionTransitionParams {
  controller: AbortController;
  setRetryTimer: (t: ReturnType<typeof setTimeout> | null) => void;
  prevState: string;
  sessionState: string;
  messages: AiMessage[];
  streamingContent: string;
  taskRunId: string | undefined;
  sendMessage: AiSession["sendMessage"];
}

export interface UseStepMachineParams extends StepMachineSetters {
  session: AiSession;
  projectPath: string;
  analysis: ProjectAnalysis;
  isRegenSpec: boolean;
  includeArchSpec: boolean;
  generatedFiles: GeneratedFile[];
}

/**
 * The step machine's refs, plus the two effects that maintain them: the
 * `allGeneratedFilesRef` mirror of `generatedFiles`, and the unmount cleanup
 * of the spec retry timer.
 */
function useStepMachineRefs(generatedFiles: GeneratedFile[]): StepMachineRefs {
  // Track what we're waiting for in the AI flow
  const pendingStepRef = useRef<IntegrationStep | null>(null);
  const specRetryTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Per-page generation queue
  const pageQueueRef = useRef<PageComponent[]>([]);
  const pagesInSessionRef = useRef(0);

  const explainerQueueRef = useRef<ExplainerQueueItem[]>([]);
  const explainerContextRef = useRef<ExplainerContext | null>(null);
  const explainerCurrentRef = useRef<ExplainerQueueItem | null>(null);
  // Explainer calls share the SESSION_PAGE_BUDGET but count independently.
  const explainerCallsInSessionRef = useRef(0);

  // Ref mirror of `generatedFiles` so closures inside handleSessionTransition
  // (which captures state at a moment in time) can read the latest list —
  // the explainer phase starts once per-page generation finishes and needs
  // the full set of just-generated specs + arch diagrams.
  const allGeneratedFilesRef = useRef<GeneratedFile[]>([]);

  // Bridged from handleSessionTransition so handleGeneratePages can invoke
  // chainToDemoScript for the first page in a demo/tour-only run (where no
  // AI prompt anchors the flow).
  const chainToDemoScriptRef = useRef<((route: string, signal: AbortSignal) => void) | null>(null);
  const currentControllerRef = useRef<AbortController | null>(null);

  // Pages as originally passed to handleGeneratePages — pageQueueRef gets
  // drained during the run, so the Project Explainer falls back to this
  // when it needs to load specs from disk (for demo/tour + explainer runs
  // where no fresh specs were generated).
  const originalPagesRef = useRef<PageComponent[]>([]);

  const effectiveAnalysisRef = useRef<ProjectAnalysis | null>(null);

  const pageOptionsRef = useRef<PageGenerationOptions>({
    generateRegistrations: true,
    generateDataPageIds: true,
    generateSpecs: true,
    generateTutorials: false,
    generateArchitectureDiagrams: false,
    generateDemoVideos: false,
    generateProductTours: false,
    generateProjectExplainer: false,
  });
  const currentPageRef = useRef<CurrentPage | null>(null);
  // Collected demo video scripts for batch recording after all pages are done
  const demoVideoScriptsRef = useRef<DemoScript[]>([]);
  // Collected product tours for batch saving after all pages are done
  const productToursRef = useRef<ProductTour[]>([]);
  // Last generated spec JSON per page (used by demo script planner and tour generator)
  const lastSpecJsonRef = useRef<string>("");

  // Track how many AI messages we've already processed to avoid duplicate extraction
  const processedMessageCountRef = useRef(0);

  useEffect(() => {
    allGeneratedFilesRef.current = generatedFiles;
  }, [generatedFiles]);

  // Clean up retry timer on unmount. Reading `.current` at cleanup time is
  // intended: the timer is a value ref (assigned by the architecture-spec
  // step handler), not a DOM node, and the LATEST timer is the one to clear.
  // The rule cannot see that assignment from here, so it is silenced.
  useEffect(() => {
    return () => {
      // eslint-disable-next-line react-hooks/exhaustive-deps
      if (specRetryTimerRef.current) clearTimeout(specRetryTimerRef.current);
    };
  }, []);

  return useMemo(
    () => ({
      pendingStepRef,
      specRetryTimerRef,
      pageQueueRef,
      pagesInSessionRef,
      explainerQueueRef,
      explainerContextRef,
      explainerCurrentRef,
      explainerCallsInSessionRef,
      allGeneratedFilesRef,
      chainToDemoScriptRef,
      currentControllerRef,
      originalPagesRef,
      effectiveAnalysisRef,
      pageOptionsRef,
      currentPageRef,
      demoVideoScriptsRef,
      productToursRef,
      lastSpecJsonRef,
      processedMessageCountRef,
    }),
    [],
  );
}

export function useStepMachine(params: UseStepMachineParams) {
  const {
    session,
    projectPath,
    analysis,
    isRegenSpec,
    includeArchSpec,
    generatedFiles,
    setPhase,
    setGeneratedFiles,
    setStepStatuses,
    setExpandedFiles,
    setError,
    setCurrentPageName,
  } = params;
  const refs = useStepMachineRefs(generatedFiles);
  const { chainToDemoScriptRef } = refs;

  // When AI transitions to ready, handle the current step completion.
  // Core logic extracted into useCallback to keep fetch() out of useEffect body.
  const handleSessionTransition = useCallback(
    (transition: SessionTransitionParams) => {
      const {
        controller,
        setRetryTimer,
        prevState,
        sessionState,
        messages,
        streamingContent,
        taskRunId,
        sendMessage,
      } = transition;

      // Only process when transitioning from "processing" to "ready" —
      // ignore the initial "ready" from createSession (before sendMessage)
      const shouldProcess = sessionState === "ready" && prevState === "processing";
      const currentStep = shouldProcess ? refs.pendingStepRef.current : null;

      if (!shouldProcess || !currentStep) {
        return;
      }

      // Gather AI content — for per-page steps, only look at NEW messages
      // to prevent re-extracting files from earlier steps
      const aiMessages = messages.filter((m) => m.role === "ai");
      const relevantMessages = isPerPageStep(currentStep)
        ? aiMessages.slice(refs.processedMessageCountRef.current)
        : aiMessages;
      const allContent = relevantMessages.map((m) => m.content).join("\n\n");
      const fullContent = streamingContent ? allContent + "\n\n" + streamingContent : allContent;

      const ctx = createStepContext({
        ...refs,
        setPhase,
        setGeneratedFiles,
        setStepStatuses,
        setExpandedFiles,
        setError,
        setCurrentPageName,
        controller,
        setRetryTimer,
        messages,
        streamingContent,
        taskRunId,
        sendMessage,
        session,
        projectPath,
        analysis,
        isRegenSpec,
        includeArchSpec,
        generatedFiles,
      });

      STEP_HANDLERS[currentStep](ctx, { aiMessages, fullContent });

      // Exposed via ref so handleGeneratePages can invoke chainToDemoScript
      // for demo/tour-only re-runs that have no AI prompt to anchor the
      // first-page flow on.
      chainToDemoScriptRef.current = ctx.flow.chainToDemoScript;
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [projectPath, analysis.framework, isRegenSpec, includeArchSpec],
  );

  return { ...refs, handleSessionTransition };
}
