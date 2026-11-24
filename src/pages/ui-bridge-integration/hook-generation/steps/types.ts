/**
 * Shared types for HookGenerationPanel's AI step machine.
 *
 * The step machine's state lives in `MutableRefObject`s owned by
 * `useStepMachine` (D3: refs are passed, not rehoused). Every step handler and
 * every flow function receives one `StepContext`, built fresh per
 * `handleSessionTransition` call. Handlers and flow functions MUST read
 * `ctx.someRef.current` at call time — never copy `.current` out when the
 * context is built — because the flow spans several AI round-trips and async
 * continuations that outlive the call that built the context.
 */

import type { Dispatch, MutableRefObject, SetStateAction } from "react";
import type { useAiSession } from "@/hooks/useAiSession";
import type { DemoScript } from "@/lib/demo-video/types";
import type { ProductTour } from "@/types/product-tour";
import type { ExplainerCluster, ExplainerSpecSummary } from "@/lib/page-analysis-prompt-builder";
import type {
  PageComponent,
  PageGenerationOptions,
  ProjectAnalysis,
  ReadPageSourceResult,
} from "../../types";
import type { GeneratedFile } from "../parse";

// =============================================================================
// Step + panel vocabulary
// =============================================================================

export type IntegrationStep =
  | "hooks"
  | "architecture-spec"
  | "page-registrations"
  | "page-spec"
  | "page-tutorial"
  | "page-architecture-diagram"
  | "page-demo-script"
  | "explainer-index"
  | "explainer-cluster"
  | "explainer-page";

export interface StepStatus {
  state: "pending" | "active" | "done" | "skipped" | "error";
  label: string;
}

export type PanelPhase =
  | "idle"
  | "generating-hooks"
  | "generating-spec"
  | "generating-page-registrations"
  | "generating-page-spec"
  | "generating-page-tutorial"
  | "generating-page-architecture-diagram"
  | "generating-project-explainer"
  | "preview"
  | "applying"
  | "applied";

// Budget: how many pages we process before rolling to a fresh AI session.
// Keeps context from growing unbounded on large projects — each page ships
// ~4-6 prompts (registrations, spec, tutorial, …) so 5 pages ≈ 20-30 turns,
// well inside a single session's window.
export const SESSION_PAGE_BUDGET = 5;

// Project Explainer phase: kicks off after per-page generation when the
// `generateProjectExplainer` flag is set. One AI call per item below; each
// response is a single markdown file saved to src/specs/explainer/.
export interface ExplainerQueueItem {
  kind: "index" | "cluster" | "page";
  /** For cluster/page items. */
  clusterId?: string;
  /** For page items. */
  specId?: string;
}

export interface ExplainerContext {
  specs: ExplainerSpecSummary[];
  arch: Map<string, string>;
  clusters: ExplainerCluster[];
  projectName: string;
}

/** The page the per-page flow is currently working on. */
export interface CurrentPage {
  page: PageComponent;
  source: ReadPageSourceResult | null;
  registrationOutput: string;
}

export type AiSession = ReturnType<typeof useAiSession>;
export type AiMessage = AiSession["messages"][number];

// =============================================================================
// StepContext
// =============================================================================

/** The step machine's refs — owned by `useStepMachine`, stable for the panel's lifetime. */
export interface StepMachineRefs {
  pendingStepRef: MutableRefObject<IntegrationStep | null>;
  specRetryTimerRef: MutableRefObject<ReturnType<typeof setTimeout> | null>;
  pageQueueRef: MutableRefObject<PageComponent[]>;
  pagesInSessionRef: MutableRefObject<number>;
  explainerQueueRef: MutableRefObject<ExplainerQueueItem[]>;
  explainerContextRef: MutableRefObject<ExplainerContext | null>;
  explainerCurrentRef: MutableRefObject<ExplainerQueueItem | null>;
  explainerCallsInSessionRef: MutableRefObject<number>;
  allGeneratedFilesRef: MutableRefObject<GeneratedFile[]>;
  chainToDemoScriptRef: MutableRefObject<((route: string, signal: AbortSignal) => void) | null>;
  currentControllerRef: MutableRefObject<AbortController | null>;
  originalPagesRef: MutableRefObject<PageComponent[]>;
  effectiveAnalysisRef: MutableRefObject<ProjectAnalysis | null>;
  pageOptionsRef: MutableRefObject<PageGenerationOptions>;
  currentPageRef: MutableRefObject<CurrentPage | null>;
  demoVideoScriptsRef: MutableRefObject<DemoScript[]>;
  productToursRef: MutableRefObject<ProductTour[]>;
  lastSpecJsonRef: MutableRefObject<string>;
  processedMessageCountRef: MutableRefObject<number>;
}

/** The panel state setters the step machine drives. */
export interface StepMachineSetters {
  setPhase: Dispatch<SetStateAction<PanelPhase>>;
  setGeneratedFiles: Dispatch<SetStateAction<GeneratedFile[]>>;
  setStepStatuses: Dispatch<SetStateAction<StepStatus[]>>;
  setExpandedFiles: Dispatch<SetStateAction<Set<string>>>;
  setError: Dispatch<SetStateAction<string | null>>;
  setCurrentPageName: Dispatch<SetStateAction<string>>;
}

/**
 * The flow functions, each bound to the same `StepContext`. Arms and flow
 * functions reach one another only through `ctx.flow`, so the mutual
 * recursion of the original nested helpers is preserved.
 */
export interface StepFlow {
  chainToArchitectureDiagram(cur: CurrentPage): void;
  chainToDemoScript(route: string, signal: AbortSignal): void;
  advanceToNextPage(signal: AbortSignal): void;
  startPageGeneration(page: PageComponent, signal: AbortSignal): Promise<void>;
  maybeStartExplainerPhase(): Promise<boolean>;
  advanceExplainerQueue(): void;
}

export interface StepContext extends StepMachineRefs, StepMachineSetters {
  // ---- per-call values (from the session-transition effect) ----
  /** Aborted when the session state changes again or the panel unmounts. */
  controller: AbortController;
  setRetryTimer: (t: ReturnType<typeof setTimeout> | null) => void;
  messages: AiMessage[];
  streamingContent: string;
  taskRunId: string | undefined;
  sendMessage: AiSession["sendMessage"];

  // ---- values captured by the handleSessionTransition closure ----
  // These are the callback's deps (`projectPath`, `analysis.framework`,
  // `isRegenSpec`, `includeArchSpec`) plus the other closure values the arms
  // read. Like the original closure, `session`, `analysis` and
  // `generatedFiles` are the values at the time the callback was created.
  session: AiSession;
  projectPath: string;
  analysis: ProjectAnalysis;
  isRegenSpec: boolean;
  includeArchSpec: boolean;
  generatedFiles: GeneratedFile[];

  flow: StepFlow;
}

/** The AI content gathered for the step that just completed. */
export interface StepContent {
  /** Every AI message in the session (used to mark messages as processed). */
  aiMessages: AiMessage[];
  /** The step's relevant AI text plus any trailing streaming content. */
  fullContent: string;
}

export type StepHandler = (ctx: StepContext, content: StepContent) => void;
