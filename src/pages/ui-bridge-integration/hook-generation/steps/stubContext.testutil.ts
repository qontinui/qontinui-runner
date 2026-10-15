/**
 * Test-only builders for driving step handlers with a stub `StepContext`:
 * refs are plain `{ current }` objects, setters and flow functions are
 * `vi.fn()`, and the AI message shape matches what `useAiSession` records
 * (`{ role, content, timestamp }`).
 */

import { vi, type Mock } from "vitest";
import type { PageComponent, PageGenerationOptions, ReadPageSourceResult } from "../../types";
import type { GeneratedFile } from "../parse";
import type {
  AiMessage,
  CurrentPage,
  StepContent,
  StepContext,
  StepFlow,
  StepStatus,
} from "./types";

type MockedFlow = { [K in keyof StepFlow]: Mock<StepFlow[K]> };

export type StubStepContext = StepContext & {
  flow: MockedFlow;
  setPhase: Mock;
  setGeneratedFiles: Mock;
  setStepStatuses: Mock;
  setExpandedFiles: Mock;
  setError: Mock;
  setCurrentPageName: Mock;
  setRetryTimer: Mock;
  sendMessage: Mock;
};

export function aiMessage(content: string): AiMessage {
  return { role: "ai", content, timestamp: "2026-10-05T00:00:00.000Z" } as AiMessage;
}

export function userMessage(content: string): AiMessage {
  return { role: "user", content, timestamp: "2026-10-05T00:00:00.000Z" } as AiMessage;
}

/** What `useStepMachine` hands a handler: every AI message plus the joined content. */
export function contentOf(messages: AiMessage[], streamingContent = ""): StepContent {
  const aiMessages = messages.filter((m) => m.role === "ai");
  const allContent = aiMessages.map((m) => m.content).join("\n\n");
  return {
    aiMessages,
    fullContent: streamingContent ? allContent + "\n\n" + streamingContent : allContent,
  };
}

export const NO_PAGE_OPTIONS: PageGenerationOptions = {
  generateRegistrations: false,
  generateDataPageIds: false,
  generateSpecs: false,
  generateTutorials: false,
  generateArchitectureDiagrams: false,
  generateDemoVideos: false,
  generateProductTours: false,
  generateProjectExplainer: false,
};

export function page(overrides: Partial<PageComponent> = {}): PageComponent {
  return {
    route: "/dashboard/settings",
    component_path: "src/pages/SettingsPage.tsx",
    component_name: "SettingsPage",
    has_registrations: false,
    has_data_page_id: false,
    has_spec: false,
    has_tutorial: false,
    ...overrides,
  };
}

export function pageSource(main = "export function SettingsPage() {}"): ReadPageSourceResult {
  return { main_source: main, imported_sources: [], total_lines: 1 };
}

export function currentPage(overrides: Partial<CurrentPage> = {}): CurrentPage {
  return { page: page(), source: pageSource(), registrationOutput: "", ...overrides };
}

/** Apply every `setStepStatuses` updater the handler issued, in order. */
export function appliedStatuses(ctx: StubStepContext, initial: StepStatus[]): StepStatus[] {
  return ctx.setStepStatuses.mock.calls.reduce<StepStatus[]>(
    (acc, [arg]) => (typeof arg === "function" ? arg(acc) : arg),
    initial,
  );
}

/** Apply every `setGeneratedFiles` updater the handler issued, in order. */
export function appliedFiles(ctx: StubStepContext, initial: GeneratedFile[]): GeneratedFile[] {
  return ctx.setGeneratedFiles.mock.calls.reduce<GeneratedFile[]>(
    (acc, [arg]) => (typeof arg === "function" ? arg(acc) : arg),
    initial,
  );
}

export function stubContext(overrides: Partial<StepContext> = {}): StubStepContext {
  const flow: MockedFlow = {
    chainToArchitectureDiagram: vi.fn(),
    chainToDemoScript: vi.fn(),
    advanceToNextPage: vi.fn(),
    startPageGeneration: vi.fn(async () => {}),
    maybeStartExplainerPhase: vi.fn(async () => false),
    advanceExplainerQueue: vi.fn(),
  };
  const ctx = {
    // ---- refs ----
    pendingStepRef: { current: null },
    specRetryTimerRef: { current: null },
    pageQueueRef: { current: [] },
    pagesInSessionRef: { current: 0 },
    explainerQueueRef: { current: [] },
    explainerContextRef: { current: null },
    explainerCurrentRef: { current: null },
    explainerCallsInSessionRef: { current: 0 },
    allGeneratedFilesRef: { current: [] },
    chainToDemoScriptRef: { current: null },
    currentControllerRef: { current: null },
    originalPagesRef: { current: [] },
    effectiveAnalysisRef: { current: null },
    pageOptionsRef: { current: { ...NO_PAGE_OPTIONS } },
    currentPageRef: { current: null },
    demoVideoScriptsRef: { current: [] },
    productToursRef: { current: [] },
    lastSpecJsonRef: { current: "" },
    processedMessageCountRef: { current: 0 },
    // ---- setters ----
    setPhase: vi.fn(),
    setGeneratedFiles: vi.fn(),
    setStepStatuses: vi.fn(),
    setExpandedFiles: vi.fn(),
    setError: vi.fn(),
    setCurrentPageName: vi.fn(),
    // ---- per-call values ----
    controller: new AbortController(),
    setRetryTimer: vi.fn(),
    messages: [],
    streamingContent: "",
    taskRunId: undefined,
    sendMessage: vi.fn(async () => undefined),
    // ---- closure values ----
    session: {} as StepContext["session"],
    projectPath: "/proj",
    analysis: { framework: "react", project_path: "/proj" } as StepContext["analysis"],
    isRegenSpec: false,
    includeArchSpec: false,
    generatedFiles: [],
    flow,
    ...overrides,
  };
  return ctx as unknown as StubStepContext;
}

/** Let fire-and-forget async IIFEs inside a handler run to completion. */
export async function flushAsync(): Promise<void> {
  for (let i = 0; i < 10; i++) await Promise.resolve();
}
