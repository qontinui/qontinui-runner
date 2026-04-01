/**
 * HookGenerationPanel — AI-powered hook & spec generation for UI Bridge integration
 *
 * Multi-step panel that:
 * 1. Generates UI Bridge hook files (route awareness, state machine, etc.)
 * 2. Generates an architecture spec (.architecture.uibridge.json)
 *
 * Shows progress steps so the user knows what's happening during the process.
 */

import { useState, useCallback, useRef, useEffect } from "react";
import {
  Sparkles,
  RefreshCw,
  Loader2,
  CheckCircle2,
  FileCode,
  ChevronDown,
  ChevronRight,
  Play,
  AlertTriangle,
  Circle,
  BookOpen,
  FolderOpen,
} from "lucide-react";
import { useAiSession } from "@/hooks/useAiSession";
import { MarkdownViewer } from "@/components/MarkdownViewer";
import {
  buildHookGenPrompt,
  buildHookRegenPrompt,
  buildArchitectureSpecPrompt,
  buildArchitectureSpecRegenPrompt,
  ALL_HOOK_CATEGORIES,
  HOOK_CATEGORY_LABELS,
  HOOK_CATEGORY_DESCRIPTIONS,
} from "@/lib/hook-gen-prompt-builder";
import type { HookCategory } from "@/lib/hook-gen-prompt-builder";
import type {
  ProjectAnalysis,
  ApiResponse,
  WriteHooksResult,
  PageComponent,
  PageGenerationOptions,
  ReadPageSourceResult,
} from "./types";
import { getApiBase } from "@/lib/runner-api";
import { planDemoScript, fetchRegisteredElements } from "@/lib/demo-video/script-planner";
import { executeScript } from "@/lib/demo-video/script-executor";
import { generateNarration } from "@/lib/demo-video/narration-generator";
import type { DemoScript } from "@/lib/demo-video/types";
import { DEFAULT_RECORDING_CONFIG } from "@/lib/demo-video/types";
import { generateTour } from "@/lib/product-tour/tour-generator";
import type { ProductTour } from "@/types/product-tour";
import { PRODUCT_TOURS_STORAGE_KEY } from "@/types/product-tour";
import { instanceStorage } from "@/lib/instance-storage";
import type { SpecConfig } from "@/lib/spec-prompt-builder";
import {
  buildRegistrationPrompt,
  buildPageSpecPrompt,
  buildTutorialPrompt,
} from "@/lib/page-analysis-prompt-builder";

// =============================================================================
// File extraction from AI output
// =============================================================================

interface GeneratedFile {
  filePath: string;
  content: string;
}

function extractGeneratedFiles(content: string): GeneratedFile[] {
  const results: GeneratedFile[] = [];
  // Match code blocks with optional language tag, then // FILE: marker on first line
  // Case-insensitive for language tags, allows blank lines between fence and marker
  const regex =
    /```(?:tsx?|jsx?|typescript(?:react)?|javascript(?:react)?)?\s*\r?\n\s*\/\/ FILE:\s*(.+?)\r?\n([\s\S]*?)```/gi;
  let match;
  while ((match = regex.exec(content)) !== null) {
    const filePath = match[1].trim();
    const fileContent = `// FILE: ${filePath}\n${match[2]}`;
    results.push({ filePath, content: fileContent });
  }
  return results;
}

/** Maximum size (in bytes) for extracted JSON blocks to prevent caching oversized payloads. */
const MAX_JSON_BLOCK_SIZE = 1024 * 1024; // 1 MB

function extractJsonBlock(content: string): string | null {
  // First pass: match ```json blocks specifically (avoids pairing with closing ``` of other code blocks)
  const jsonRegex = /```json\s*\n([\s\S]*?)```/gi;
  let match;
  while ((match = jsonRegex.exec(content)) !== null) {
    const raw = match[1];
    if (raw.length > MAX_JSON_BLOCK_SIZE) continue; // Skip oversized blocks
    try {
      JSON.parse(raw);
      return raw.trim();
    } catch {
      // Not valid JSON, try next block
    }
  }
  // Fallback: try bare ``` blocks (no language tag)
  const bareRegex = /```\s*\n(\s*\{[\s\S]*?\})\s*\n```/g;
  while ((match = bareRegex.exec(content)) !== null) {
    const raw = match[1];
    if (raw.length > MAX_JSON_BLOCK_SIZE) continue; // Skip oversized blocks
    try {
      JSON.parse(raw);
      return raw.trim();
    } catch {
      // Not valid JSON, try next block
    }
  }
  return null;
}

// =============================================================================
// Step types
// =============================================================================

type IntegrationStep =
  | "hooks"
  | "architecture-spec"
  | "page-registrations"
  | "page-spec"
  | "page-tutorial"
  | "page-demo-script";

interface StepStatus {
  state: "pending" | "active" | "done" | "skipped" | "error";
  label: string;
}

type PanelPhase =
  | "idle"
  | "generating-hooks"
  | "generating-spec"
  | "generating-page-registrations"
  | "generating-page-spec"
  | "generating-page-tutorial"
  | "preview"
  | "applying"
  | "applied";

// =============================================================================
// Progress Step Indicator
// =============================================================================

function StepIndicator({ steps }: { steps: StepStatus[] }) {
  return (
    <div className="flex flex-col gap-1 mb-3">
      {steps.map((step, i) => (
        <div key={`${step.label}-${i}`} className="flex items-center gap-2 text-xs">
          {step.state === "done" ? (
            <CheckCircle2 className="w-3.5 h-3.5 text-green-400 shrink-0" />
          ) : step.state === "active" ? (
            <Loader2 className="w-3.5 h-3.5 text-purple-400 animate-spin shrink-0" />
          ) : step.state === "error" ? (
            <AlertTriangle className="w-3.5 h-3.5 text-red-400 shrink-0" />
          ) : step.state === "skipped" ? (
            <Circle className="w-3.5 h-3.5 text-muted-foreground/30 shrink-0" />
          ) : (
            <Circle className="w-3.5 h-3.5 text-muted-foreground/40 shrink-0" />
          )}
          <span
            className={
              step.state === "active"
                ? "text-purple-400 font-medium"
                : step.state === "done"
                  ? "text-green-400"
                  : step.state === "error"
                    ? "text-red-400"
                    : "text-muted-foreground/60"
            }
          >
            {step.label}
          </span>
        </div>
      ))}
    </div>
  );
}

// =============================================================================
// Async helpers (extracted outside component to avoid fetch-in-useEffect lint)
// =============================================================================

async function fetchAndSendSpecPrompt(params: {
  signal: AbortSignal;
  isRegenSpec: boolean;
  projectPath: string;
  analysis: { framework: string; project_path: string };
  sendMessage: (msg: string) => Promise<void>;
}): Promise<void> {
  const { signal, isRegenSpec, projectPath, analysis, sendMessage } = params;
  let specPrompt: string;
  if (isRegenSpec) {
    let existingSpec = "";
    try {
      const resp = await fetch(`${getApiBase()}/ui-bridge/integration/read-file`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          project_path: projectPath,
          file_path: "project.architecture.uibridge.json",
        }),
        signal,
      });
      if (signal.aborted) return;
      const data = await resp.json();
      if (data.success && data.data) existingSpec = data.data;
    } catch (_e) {
      if (signal.aborted) return;
      // Fall through to fresh generation
    }
    specPrompt = existingSpec
      ? buildArchitectureSpecRegenPrompt(analysis, existingSpec)
      : buildArchitectureSpecPrompt(analysis);
  } else {
    specPrompt = buildArchitectureSpecPrompt(analysis);
  }
  if (signal.aborted) return;
  await sendMessage(
    "Now generate an architecture spec for this project. You already have context from the hook generation step — use what you learned.\n\n" +
      specPrompt,
  );
}

// =============================================================================
// HookGenerationPanel
// =============================================================================

interface HookGenerationPanelProps {
  projectPath: string;
  analysis: ProjectAnalysis;
  onRefreshAnalysis?: () => void;
}

export function HookGenerationPanel({
  projectPath,
  analysis,
  onRefreshAnalysis,
}: HookGenerationPanelProps) {
  const session = useAiSession();
  const [phase, setPhase] = useState<PanelPhase>("idle");
  const [selectedCategories, setSelectedCategories] = useState<Set<HookCategory>>(
    new Set(ALL_HOOK_CATEGORIES),
  );
  const [includeArchSpec, setIncludeArchSpec] = useState(true);
  const [generatedFiles, setGeneratedFiles] = useState<GeneratedFile[]>([]);
  const [expandedFiles, setExpandedFiles] = useState<Set<string>>(new Set());
  const [writeResult, setWriteResult] = useState<WriteHooksResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [stepStatuses, setStepStatuses] = useState<StepStatus[]>([]);
  const messagesEndRef = useRef<HTMLDivElement>(null);

  // Track what we're waiting for in the AI flow
  const pendingStepRef = useRef<IntegrationStep | null>(null);
  const prevSessionStateRef = useRef<string>(session.sessionState);
  const specRetryTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Per-page generation queue
  const pageQueueRef = useRef<PageComponent[]>([]);
  const pageOptionsRef = useRef<PageGenerationOptions>({
    generateRegistrations: true,
    generateSpecs: true,
    generateTutorials: false,
    generateDemoVideos: false,
    generateProductTours: false,
  });
  const currentPageRef = useRef<{
    page: PageComponent;
    source: ReadPageSourceResult | null;
    registrationOutput: string;
  } | null>(null);
  // Collected demo video scripts for batch recording after all pages are done
  const demoVideoScriptsRef = useRef<DemoScript[]>([]);
  // Collected product tours for batch saving after all pages are done
  const productToursRef = useRef<ProductTour[]>([]);
  // Last generated spec JSON per page (used by demo script planner and tour generator)
  const lastSpecJsonRef = useRef<string>("");

  // Track how many AI messages we've already processed to avoid duplicate extraction
  const processedMessageCountRef = useRef(0);

  const isRegenHooks = analysis.has_generated_hooks;
  const isRegenSpec = analysis.has_architecture_spec;

  // Clean up retry timer on unmount
  useEffect(() => {
    return () => {
      if (specRetryTimerRef.current) clearTimeout(specRetryTimerRef.current);
    };
  }, []);

  // Auto-scroll on streaming content
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [session.streamingContent, session.messages]);

  // When AI transitions to ready, handle the current step completion.
  // Core logic extracted into useCallback to keep fetch() out of useEffect body.
  const handleSessionTransition = useCallback(
    (params: {
      controller: AbortController;
      setRetryTimer: (t: ReturnType<typeof setTimeout> | null) => void;
      prevState: string;
      sessionState: string;
      messages: typeof session.messages;
      streamingContent: string;
      taskRunId: string | undefined;
      sendMessage: typeof session.sendMessage;
    }) => {
      const {
        controller,
        setRetryTimer,
        prevState,
        sessionState,
        messages,
        streamingContent,
        taskRunId,
        sendMessage,
      } = params;

      // Only process when transitioning from "processing" to "ready" —
      // ignore the initial "ready" from createSession (before sendMessage)
      const shouldProcess = sessionState === "ready" && prevState === "processing";
      const currentStep = shouldProcess ? pendingStepRef.current : null;

      if (!shouldProcess || !currentStep) {
        return;
      }

      // Gather AI content — for per-page steps, only look at NEW messages
      // to prevent re-extracting files from earlier steps
      const aiMessages = messages.filter((m) => m.role === "ai");
      const isPerPageStep =
        currentStep === "page-registrations" ||
        currentStep === "page-spec" ||
        currentStep === "page-tutorial";
      const relevantMessages = isPerPageStep
        ? aiMessages.slice(processedMessageCountRef.current)
        : aiMessages;
      const allContent = relevantMessages.map((m) => m.content).join("\n\n");
      const fullContent = streamingContent ? allContent + "\n\n" + streamingContent : allContent;

      if (currentStep === "hooks") {
        const files = extractGeneratedFiles(fullContent);
        if (files.length > 0) {
          setGeneratedFiles(files);
          setStepStatuses((prev) =>
            prev.map((s) => (s.label.includes("Hook") ? { ...s, state: "done" } : s)),
          );

          // If architecture spec is included, proceed to that step
          if (includeArchSpec) {
            pendingStepRef.current = "architecture-spec";
            setPhase("generating-spec");
            setStepStatuses((prev) =>
              prev.map((s) => (s.label.includes("Architecture") ? { ...s, state: "active" } : s)),
            );
            // Send the architecture spec prompt in the same session
            const analysisForPrompt = { framework: analysis.framework, project_path: projectPath };
            fetchAndSendSpecPrompt({
              signal: controller.signal,
              isRegenSpec,
              projectPath,
              analysis: analysisForPrompt,
              sendMessage,
            });
          } else {
            // No spec step — go to preview
            pendingStepRef.current = null;
            setExpandedFiles(new Set(files.map((f) => f.filePath)));
            setPhase("preview");
          }
        } else {
          pendingStepRef.current = null;
          setStepStatuses((prev) =>
            prev.map((s) => (s.label.includes("Hook") ? { ...s, state: "error" } : s)),
          );
          setError("AI did not produce any files with // FILE: markers. Try regenerating.");
          setPhase("idle");
        }
      } else if (currentStep === "architecture-spec") {
        // Extract JSON — try immediately, then retry via API if text events are still in transit
        const handleSpecError = () => {
          if (controller.signal.aborted) return;
          setStepStatuses((prev) =>
            prev.map((s) => (s.label.includes("Architecture") ? { ...s, state: "error" } : s)),
          );
          setError(
            "Architecture spec generation did not produce valid JSON. You can still apply the hook files.",
          );
          pendingStepRef.current = null;
          setPhase("preview");
        };

        const tryExtractSpec = (content: string) => {
          const jsonBlock = extractJsonBlock(content);
          if (jsonBlock) {
            let specFileName = "project.architecture.uibridge.json";
            try {
              const parsed = JSON.parse(jsonBlock);
              if (typeof parsed.projectName === "string" && parsed.projectName) {
                specFileName =
                  parsed.projectName
                    .toLowerCase()
                    .replace(/[^a-z0-9]+/g, "-")
                    .replace(/^-|-$/g, "") + ".architecture.uibridge.json";
              }
            } catch {
              // Use default name
            }

            setGeneratedFiles((prev) => [...prev, { filePath: specFileName, content: jsonBlock }]);
            setStepStatuses((prev) =>
              prev.map((s) => (s.label.includes("Architecture") ? { ...s, state: "done" } : s)),
            );
            pendingStepRef.current = null;
            setExpandedFiles(new Set(generatedFiles.map((f) => f.filePath).concat(["spec"])));
            setPhase("preview");
            return true;
          }
          return false;
        };

        // Try extracting from current content
        const specContent = messages
          .filter((m) => m.role === "ai")
          .map((m) => m.content)
          .join("\n\n");
        const fullSpecContent = streamingContent
          ? specContent + "\n\n" + streamingContent
          : specContent;

        if (!tryExtractSpec(fullSpecContent)) {
          // Race condition: text events still in transit. Retry via API.
          if (specRetryTimerRef.current) clearTimeout(specRetryTimerRef.current);
          const timer = setTimeout(async () => {
            specRetryTimerRef.current = null;
            setRetryTimer(null);
            if (controller.signal.aborted || !taskRunId) {
              if (!controller.signal.aborted) handleSpecError();
              return;
            }

            try {
              const resp = await fetch(
                `${getApiBase()}/task-runs/${taskRunId}/output?tail_chars=200000`,
                { signal: controller.signal },
              );
              if (controller.signal.aborted) return;
              const data = await resp.json();
              const apiOutput: string = data?.output || "";
              if (!tryExtractSpec(apiOutput)) {
                handleSpecError();
              }
            } catch {
              if (!controller.signal.aborted) {
                handleSpecError();
              }
            }
          }, 1000);
          specRetryTimerRef.current = timer;
          setRetryTimer(timer);
        }

        // ---- Per-page step handlers ----
      } else if (currentStep === "page-registrations") {
        processedMessageCountRef.current = aiMessages.length; // Mark messages as processed
        const files = extractGeneratedFiles(fullContent);
        if (files.length > 0) {
          setGeneratedFiles((prev) => [...prev, ...files]);
          // Store registration output for spec/tutorial prompts
          if (currentPageRef.current) {
            currentPageRef.current.registrationOutput = files.map((f) => f.content).join("\n\n");
          }
          setStepStatuses((prev) =>
            prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
          );
        }

        // Chain to page-spec if enabled
        const opts = pageOptionsRef.current;
        const cur = currentPageRef.current;
        if (opts.generateSpecs && cur?.source) {
          pendingStepRef.current = "page-spec";
          setPhase("generating-page-spec");
          setStepStatuses((prev) => [
            ...prev,
            { state: "active", label: `Spec: ${cur.page.route}` },
          ]);

          // Load existing spec for merge mode if available
          (async () => {
            let existingSpec: string | undefined;
            if (cur.page.has_spec) {
              const specName = `${cur.page.route.replace(/^\//, "").replace(/\//g, "-") || "root"}.spec.uibridge.json`;
              existingSpec =
                (await readProjectFile(`src/specs/${specName}`, controller.signal)) ||
                (await readProjectFile(specName, controller.signal)) ||
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
            sendMessage(
              "Now generate a page spec (.spec.uibridge.json) for this page.\n\n" + specPrompt,
            );
          })();
        } else if (opts.generateTutorials && cur?.source) {
          // Skip to tutorial
          pendingStepRef.current = "page-tutorial";
          setPhase("generating-page-tutorial");
          setStepStatuses((prev) => [
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
          sendMessage("Now generate a tutorial for this page.\n\n" + tutPrompt);
        } else if (opts.generateDemoVideos || opts.generateProductTours) {
          // Skip to demo script / product tour planning
          chainToDemoScript(cur?.page.route ?? "", controller.signal);
        } else {
          // Advance to next page
          advanceToNextPage(controller.signal);
        }
      } else if (currentStep === "page-spec") {
        processedMessageCountRef.current = aiMessages.length;
        const jsonBlock = extractJsonBlock(fullContent);
        if (jsonBlock) {
          lastSpecJsonRef.current = jsonBlock;
          const specName = currentPageRef.current
            ? `${currentPageRef.current.page.route.replace(/^\//, "").replace(/\//g, "-") || "root"}.spec.uibridge.json`
            : "page.spec.uibridge.json";
          setGeneratedFiles((prev) => [...prev, { filePath: specName, content: jsonBlock }]);
          setStepStatuses((prev) =>
            prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
          );
        }

        // Chain to tutorial if enabled
        const opts = pageOptionsRef.current;
        const cur = currentPageRef.current;
        if (opts.generateTutorials && cur?.source) {
          pendingStepRef.current = "page-tutorial";
          setPhase("generating-page-tutorial");
          setStepStatuses((prev) => [
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
          sendMessage("Now generate a tutorial for this page.\n\n" + tutPrompt);
        } else if (opts.generateDemoVideos || opts.generateProductTours) {
          chainToDemoScript(cur?.page.route ?? "", controller.signal);
        } else {
          advanceToNextPage(controller.signal);
        }
      } else if (currentStep === "page-tutorial") {
        processedMessageCountRef.current = aiMessages.length;
        const files = extractGeneratedFiles(fullContent);
        if (files.length > 0) {
          setGeneratedFiles((prev) => [...prev, ...files]);
        }
        setStepStatuses((prev) =>
          prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
        );
        const opts = pageOptionsRef.current;
        if (opts.generateDemoVideos || opts.generateProductTours) {
          const cur = currentPageRef.current;
          chainToDemoScript(cur?.page.route ?? "", controller.signal);
        } else {
          advanceToNextPage(controller.signal);
        }
      } else if (currentStep === "page-demo-script") {
        // Demo script planning is handled inline (non-AI) — this case handles
        // the completion signal. The script was already added to demoVideoScriptsRef
        // by chainToDemoScript. Just advance.
        setStepStatuses((prev) =>
          prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
        );
        advanceToNextPage(controller.signal);
      }

      // Helper: chain to demo script + product tour planning (non-AI — calls planner APIs directly)
      function chainToDemoScript(route: string, signal: AbortSignal) {
        pendingStepRef.current = "page-demo-script";
        const opts = pageOptionsRef.current;
        const parts = [
          opts.generateDemoVideos && "Demo",
          opts.generateProductTours && "Tour",
        ].filter(Boolean);
        setStepStatuses((prev) => [
          ...prev,
          { state: "active", label: `${parts.join(" + ")}: ${route}` },
        ]);

        (async () => {
          // Parse the last generated spec JSON into a SpecConfig
          let specConfig: SpecConfig | null = null;
          if (lastSpecJsonRef.current) {
            try {
              specConfig = JSON.parse(lastSpecJsonRef.current) as SpecConfig;
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
                demoVideoScriptsRef.current.push(script);
              } catch (err) {
                console.warn("Demo script planning failed for", route, err);
              }
            }

            // Product tour
            if (opts.generateProductTours) {
              try {
                const tour = await generateTour(specConfig, elements, "new-user");
                productToursRef.current.push(tour);
              } catch (err) {
                console.warn("Product tour generation failed for", route, err);
              }
            }
          }

          setStepStatuses((prev) =>
            prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
          );
          advanceToNextPage(signal);
        })();
      }

      // Helper: advance to the next page in the queue or go to preview/recording
      function advanceToNextPage(signal: AbortSignal) {
        const queue = pageQueueRef.current;
        if (queue.length > 0) {
          const nextPage = queue.shift()!;
          startPageGeneration(nextPage, signal);
        } else {
          // All pages done
          pendingStepRef.current = null;
          currentPageRef.current = null;

          // Save product tours if any were generated
          const tours = productToursRef.current;
          if (tours.length > 0) {
            const existing = instanceStorage.getJSON<ProductTour[]>(PRODUCT_TOURS_STORAGE_KEY, []);
            const newIds = new Set(tours.map((t) => t.id));
            const merged = [...existing.filter((t) => !newIds.has(t.id)), ...tours];
            instanceStorage.setJSON(PRODUCT_TOURS_STORAGE_KEY, merged);
            productToursRef.current = [];
          }

          // If demo videos were planned, start batch recording
          const scripts = demoVideoScriptsRef.current;
          if (scripts.length > 0 && pageOptionsRef.current.generateDemoVideos) {
            setStepStatuses((prev) => [
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
                  setGeneratedFiles((prev) => [
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
              demoVideoScriptsRef.current = [];
              setStepStatuses((prev) =>
                prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
              );
              setPhase("preview");
            })();
          } else {
            setPhase("preview");
          }
        }
      }

      /** Read an existing file from the project (returns empty string on failure). */
      async function readProjectFile(filePath: string, signal: AbortSignal): Promise<string> {
        try {
          const resp = await fetch(`${getApiBase()}/ui-bridge/integration/read-file`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ project_path: projectPath, file_path: filePath }),
            signal,
          });
          if (signal.aborted) return "";
          const data = await resp.json();
          return data.success && data.data ? data.data : "";
        } catch {
          return "";
        }
      }

      async function startPageGeneration(page: PageComponent, signal: AbortSignal) {
        currentPageRef.current = { page, source: null, registrationOutput: "" };
        setStepStatuses((prev) => [
          ...prev,
          { state: "active", label: `Registrations: ${page.route}` },
        ]);

        // Fetch page source
        try {
          const resp = await fetch(`${getApiBase()}/ui-bridge/integration/read-page-source`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({
              project_path: projectPath,
              component_path: page.component_path,
              max_depth: 2,
            }),
            signal,
          });
          if (signal.aborted) return;
          const data: ApiResponse<ReadPageSourceResult> = await resp.json();
          if (data.success && data.data) {
            currentPageRef.current!.source = data.data;
          }
        } catch {
          if (signal.aborted) return;
        }

        const source = currentPageRef.current?.source;
        if (!source) {
          setStepStatuses((prev) =>
            prev.map((s) => (s.state === "active" ? { ...s, state: "error" } : s)),
          );
          advanceToNextPage(signal);
          return;
        }

        const opts = pageOptionsRef.current;
        if (opts.generateRegistrations) {
          pendingStepRef.current = "page-registrations";
          setPhase("generating-page-registrations");

          // Load existing registrations for merge mode
          let existingRegs: string | undefined;
          if (page.has_registrations) {
            const pageName = page.component_name.toLowerCase().replace(/page$/, "");
            existingRegs = await readProjectFile(
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
            analysis.framework,
            existingRegs || undefined,
          );
          await sendMessage(
            `Analyze the page at ${page.route} and generate UI Bridge registrations.\n\n` + prompt,
          );
        } else if (opts.generateSpecs) {
          pendingStepRef.current = "page-spec";
          setPhase("generating-page-spec");
          setStepStatuses((prev) => [
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
          await sendMessage(`Generate a page spec for ${page.route}.\n\n` + specPrompt);
        } else if (opts.generateTutorials) {
          pendingStepRef.current = "page-tutorial";
          setPhase("generating-page-tutorial");
          setStepStatuses((prev) => [
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
          await sendMessage(`Generate a tutorial for ${page.route}.\n\n` + tutPrompt);
        } else if (opts.generateDemoVideos || opts.generateProductTours) {
          // Only demo videos / product tours selected — chain directly
          chainToDemoScript(page.route, signal);
        }
      }
    },
    [projectPath, analysis.framework, isRegenSpec, includeArchSpec],
  );

  useEffect(() => {
    const controller = new AbortController();
    let retryTimer: ReturnType<typeof setTimeout> | null = null;

    const prevState = prevSessionStateRef.current;
    prevSessionStateRef.current = session.sessionState;

    handleSessionTransition({
      controller,
      setRetryTimer: (t) => {
        retryTimer = t;
      },
      prevState,
      sessionState: session.sessionState,
      messages: session.messages,
      streamingContent: session.streamingContent,
      taskRunId: session.taskRunId ?? undefined,
      sendMessage: session.sendMessage,
    });

    return () => {
      controller.abort();
      if (retryTimer) clearTimeout(retryTimer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session.sessionState, handleSessionTransition]);

  // When entering preview, ensure all files are expanded
  useEffect(() => {
    if (phase === "preview") {
      setExpandedFiles(new Set(generatedFiles.map((f) => f.filePath)));
    }
  }, [phase, generatedFiles]);

  // Toggle category selection
  const toggleCategory = useCallback((cat: HookCategory) => {
    setSelectedCategories((prev) => {
      const next = new Set(prev);
      if (next.has(cat)) {
        next.delete(cat);
      } else {
        next.add(cat);
      }
      return next;
    });
  }, []);

  // Start the multi-step generation
  const handleGenerate = useCallback(async () => {
    if (selectedCategories.size === 0 && !includeArchSpec) return;

    setError(null);
    setGeneratedFiles([]);
    setWriteResult(null);

    // Build step statuses
    const steps: StepStatus[] = [];
    if (selectedCategories.size > 0) {
      steps.push({
        state: "active",
        label: isRegenHooks ? "Regenerating Hook Files" : "Generating Hook Files",
      });
    }
    if (includeArchSpec) {
      steps.push({
        state: "pending",
        label: isRegenSpec ? "Updating Architecture Spec" : "Generating Architecture Spec",
      });
    }
    setStepStatuses(steps);

    // Close existing session if any
    if (session.taskRunId) {
      session.close();
      session.resetSession();
    }

    const categories = Array.from(selectedCategories) as HookCategory[];
    const analysisForPrompt = { framework: analysis.framework, project_path: projectPath };

    // Determine if we're starting with hooks or going straight to spec
    if (selectedCategories.size > 0) {
      setPhase("generating-hooks");
      pendingStepRef.current = "hooks";

      let prompt: string;
      if (isRegenHooks) {
        let existingCode = "";
        try {
          const resp = await fetch(`${getApiBase()}/ui-bridge/integration/read-file`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({
              project_path: projectPath,
              file_path: "src/lib/ui-bridge/UIBridgeHooks.tsx",
            }),
          });
          const data = await resp.json();
          if (data.success && data.data) existingCode = data.data;
        } catch {
          // Fall through to fresh generation
        }
        prompt = existingCode
          ? buildHookRegenPrompt(analysisForPrompt, categories, existingCode)
          : buildHookGenPrompt(analysisForPrompt, categories);
      } else {
        prompt = buildHookGenPrompt(analysisForPrompt, categories);
      }

      const label = isRegenHooks ? "Regenerate Integration" : "Generate Integration";
      const id = await session.createSession(`Integration: ${label}`);
      if (!id) {
        setError("Failed to create AI session");
        setPhase("idle");
        return;
      }
      await session.sendMessage(prompt);
    } else {
      // Spec only (no hooks selected)
      setPhase("generating-spec");
      pendingStepRef.current = "architecture-spec";
      setStepStatuses((prev) =>
        prev.map((s) => (s.label.includes("Architecture") ? { ...s, state: "active" } : s)),
      );

      let specPrompt: string;
      if (isRegenSpec) {
        let existingSpec = "";
        try {
          const resp = await fetch(`${getApiBase()}/ui-bridge/integration/read-file`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({
              project_path: projectPath,
              file_path: "project.architecture.uibridge.json",
            }),
          });
          const data = await resp.json();
          if (data.success && data.data) existingSpec = data.data;
        } catch {
          // Fall through
        }
        specPrompt = existingSpec
          ? buildArchitectureSpecRegenPrompt(analysisForPrompt, existingSpec)
          : buildArchitectureSpecPrompt(analysisForPrompt);
      } else {
        specPrompt = buildArchitectureSpecPrompt(analysisForPrompt);
      }

      const id = await session.createSession("Integration: Architecture Spec");
      if (!id) {
        setError("Failed to create AI session");
        setPhase("idle");
        return;
      }
      await session.sendMessage(specPrompt);
    }
  }, [
    selectedCategories,
    includeArchSpec,
    session,
    analysis,
    projectPath,
    isRegenHooks,
    isRegenSpec,
  ]);

  // Start per-page AI generation (called from PageSelectionPanel via window event)
  const handleGeneratePages = useCallback(
    async (pages: PageComponent[], options: PageGenerationOptions) => {
      if (pages.length === 0) return;

      setError(null);
      setGeneratedFiles([]);
      setWriteResult(null);
      pageQueueRef.current = [...pages];
      pageOptionsRef.current = options;
      processedMessageCountRef.current = 0;

      // Build initial step statuses
      const steps: StepStatus[] = pages.map((p) => ({
        state: "pending" as const,
        label: `${p.route} (${[
          options.generateRegistrations && "regs",
          options.generateSpecs && "spec",
          options.generateTutorials && "tutorial",
          options.generateDemoVideos && "demo",
          options.generateProductTours && "tour",
        ]
          .filter(Boolean)
          .join("+")})`,
      }));
      setStepStatuses(steps);
      demoVideoScriptsRef.current = [];
      productToursRef.current = [];
      lastSpecJsonRef.current = "";

      // Close existing session
      if (session.taskRunId) {
        session.close();
        session.resetSession();
      }

      const id = await session.createSession("Page Preparation: AI Generation");
      if (!id) {
        setError("Failed to create AI session");
        return;
      }

      // Start first page
      const firstPage = pageQueueRef.current.shift()!;
      currentPageRef.current = { page: firstPage, source: null, registrationOutput: "" };
      setStepStatuses((prev) => prev.map((s, i) => (i === 0 ? { ...s, state: "active" } : s)));

      // Fetch page source and send first prompt
      try {
        const resp = await fetch(`${getApiBase()}/ui-bridge/integration/read-page-source`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            project_path: projectPath,
            component_path: firstPage.component_path,
            max_depth: 2,
          }),
        });
        const data: ApiResponse<ReadPageSourceResult> = await resp.json();
        if (data.success && data.data) {
          currentPageRef.current!.source = data.data;
        }
      } catch {
        // Continue with empty source
      }

      const source = currentPageRef.current?.source;
      if (!source) {
        setError(`Failed to read source for ${firstPage.component_path}`);
        setPhase("idle");
        return;
      }

      if (options.generateRegistrations) {
        pendingStepRef.current = "page-registrations";
        setPhase("generating-page-registrations");
        const prompt = buildRegistrationPrompt(
          source.main_source,
          source.imported_sources,
          firstPage.component_name,
          firstPage.route,
          analysis.framework,
        );
        await session.sendMessage(
          `Analyze the page at ${firstPage.route} and generate UI Bridge registrations.\n\n` +
            prompt,
        );
      } else if (options.generateSpecs) {
        pendingStepRef.current = "page-spec";
        setPhase("generating-page-spec");
        const specPrompt = buildPageSpecPrompt(
          source.main_source,
          source.imported_sources,
          firstPage.component_name,
          firstPage.route,
          "",
        );
        await session.sendMessage(`Generate a page spec for ${firstPage.route}.\n\n` + specPrompt);
      } else if (options.generateTutorials) {
        pendingStepRef.current = "page-tutorial";
        setPhase("generating-page-tutorial");
        const tutPrompt = buildTutorialPrompt(
          source.main_source,
          firstPage.component_name,
          firstPage.route,
          "",
          "",
        );
        await session.sendMessage(`Generate a tutorial for ${firstPage.route}.\n\n` + tutPrompt);
      }
    },
    [session, analysis, projectPath],
  );

  // Listen for page generation trigger from PageSelectionPanel via CustomEvent
  useEffect(() => {
    const handler = (e: Event) => {
      const detail = (e as CustomEvent).detail as
        | {
            pages: PageComponent[];
            options: PageGenerationOptions;
          }
        | undefined;
      if (detail) {
        handleGeneratePages(detail.pages, detail.options);
      }
    };
    window.addEventListener("ui-bridge-generate-pages", handler);
    return () => window.removeEventListener("ui-bridge-generate-pages", handler);
  }, [handleGeneratePages]);

  // Apply generated files to project
  const handleApply = useCallback(async () => {
    if (generatedFiles.length === 0) return;

    setPhase("applying");
    setError(null);

    const files = generatedFiles.map((f) => ({
      file_path: f.filePath,
      modification_type: f.filePath.endsWith(".json")
        ? isRegenSpec
          ? "replace"
          : "create_new"
        : isRegenHooks
          ? "replace"
          : "create_new",
      new_content: f.content,
    }));

    try {
      const resp = await fetch(`${getApiBase()}/ui-bridge/integration/write-hooks`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ project_path: projectPath, files }),
      });
      const data: ApiResponse<WriteHooksResult> = await resp.json();
      if (data.success && data.data) {
        setWriteResult(data.data);
        setPhase("applied");
        onRefreshAnalysis?.();

        // Cache architecture spec so it appears in the Architecture page
        const specFile = generatedFiles.find((f) =>
          f.filePath.endsWith(".architecture.uibridge.json"),
        );
        if (specFile) {
          try {
            await fetch(`${getApiBase()}/ui-bridge/integration/cache-architecture-spec`, {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({
                project_path: projectPath,
                spec_json: specFile.content,
              }),
            });
          } catch {
            // Non-critical — spec written to disk, just not cached
          }
        }
      } else {
        setError(data.error || "Failed to write files");
        setPhase("preview");
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to write files");
      setPhase("preview");
    }
  }, [generatedFiles, projectPath, isRegenHooks, isRegenSpec, onRefreshAnalysis]);

  // Toggle file preview expansion
  const toggleFile = useCallback((filePath: string) => {
    setExpandedFiles((prev) => {
      const next = new Set(prev);
      if (next.has(filePath)) {
        next.delete(filePath);
      } else {
        next.add(filePath);
      }
      return next;
    });
  }, []);

  const isProcessing = session.sessionState === "processing";
  const isPerPagePhase =
    phase === "generating-page-registrations" ||
    phase === "generating-page-spec" ||
    phase === "generating-page-tutorial";
  const isGenerating =
    phase === "generating-hooks" || phase === "generating-spec" || isPerPagePhase;
  const currentPageName = currentPageRef.current?.page.route || "";

  // Group generated files by page for preview
  const groupedFiles = generatedFiles.reduce<Record<string, GeneratedFile[]>>((acc, f) => {
    // Group by: page-specific files go under their page route, others under "project"
    const isPageSpec =
      f.filePath.endsWith(".spec.uibridge.json") && !f.filePath.includes("architecture");
    const isPageReg = f.filePath.includes("/pages/") && f.filePath.includes("-registrations");
    const isPageTut = f.filePath.includes("tutorial/data/");
    let group = "Project";
    if (isPageSpec || isPageReg || isPageTut) {
      // Extract page name from file path
      const parts = f.filePath.split("/");
      const fileName = parts[parts.length - 1];
      const pageName = fileName
        .replace("-registrations.tsx", "")
        .replace(".spec.uibridge.json", "")
        .replace(".ts", "");
      group = `Page: /${pageName}`;
    }
    if (!acc[group]) acc[group] = [];
    acc[group].push(f);
    return acc;
  }, {});

  return (
    <div className="p-4 rounded-lg border border-border bg-card/50">
      <div className="flex items-center justify-between mb-3">
        <h3 className="text-sm font-medium flex items-center gap-1.5">
          <Sparkles className="w-3.5 h-3.5 text-purple-400" />
          AI Integration Setup
        </h3>
        {phase === "applied" && (
          <span className="text-[10px] px-2 py-0.5 rounded-full font-medium bg-green-500/10 text-green-400">
            Applied
          </span>
        )}
      </div>

      <p className="text-xs text-muted-foreground mb-3">
        AI analyzes your project and generates hooks, architecture specs, element registrations,
        page specs, and tutorials — fully preparing the project for AI-driven automation.
      </p>

      {/* Configuration — shown in idle/applied states */}
      {(phase === "idle" || phase === "applied") && (
        <>
          {/* Hook category checkboxes */}
          <p className="text-[10px] text-muted-foreground font-medium mb-1">Hook Categories:</p>
          <div className="mb-3 grid grid-cols-2 gap-1">
            {ALL_HOOK_CATEGORIES.map((cat) => (
              <label
                key={cat}
                className="flex items-start gap-1.5 text-xs cursor-pointer group"
                title={HOOK_CATEGORY_DESCRIPTIONS[cat]}
              >
                <input
                  type="checkbox"
                  checked={selectedCategories.has(cat)}
                  onChange={() => toggleCategory(cat)}
                  className="mt-0.5 accent-purple-500"
                />
                <span className="text-muted-foreground group-hover:text-foreground transition-colors">
                  {HOOK_CATEGORY_LABELS[cat]}
                </span>
              </label>
            ))}
          </div>

          {/* Architecture spec checkbox */}
          <label className="flex items-start gap-1.5 text-xs cursor-pointer group mb-3">
            <input
              type="checkbox"
              checked={includeArchSpec}
              onChange={() => setIncludeArchSpec((v) => !v)}
              className="mt-0.5 accent-purple-500"
            />
            <div>
              <span className="text-muted-foreground group-hover:text-foreground transition-colors font-medium flex items-center gap-1">
                <BookOpen className="w-3 h-3" />
                Architecture Spec
              </span>
              <span className="text-[10px] text-muted-foreground/60 block">
                Generates a .architecture.uibridge.json describing tech stack, features, patterns,
                and constraints — used by AI for deeper project understanding
              </span>
            </div>
          </label>

          {/* Generate button */}
          <button
            onClick={handleGenerate}
            disabled={selectedCategories.size === 0 && !includeArchSpec}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded
                       bg-purple-500/10 text-purple-400 border border-purple-500/20
                       hover:bg-purple-500/20 disabled:opacity-50 transition-colors"
          >
            {isRegenHooks || isRegenSpec ? (
              <RefreshCw className="w-3.5 h-3.5" />
            ) : (
              <Sparkles className="w-3.5 h-3.5" />
            )}
            {isRegenHooks || isRegenSpec ? "Regenerate" : "Generate"}
          </button>
        </>
      )}

      {/* Progress steps — shown during generation */}
      {isGenerating && stepStatuses.length > 0 && (
        <div className="mb-2">
          {isPerPagePhase && currentPageName && (
            <div className="flex items-center gap-1.5 text-xs text-cyan-400 font-medium mb-2">
              <FolderOpen className="w-3.5 h-3.5" />
              Processing: {currentPageName}
            </div>
          )}
          <StepIndicator steps={stepStatuses} />
        </div>
      )}

      {/* Generating — streaming view */}
      {isGenerating && (
        <div className="mt-2">
          {/* Tool activity */}
          {session.toolActivity && (
            <div className="flex items-center gap-1.5 text-[10px] text-muted-foreground/60 mb-2">
              <Loader2 className="w-3 h-3 animate-spin" />
              <span className="truncate">{session.toolActivity}</span>
            </div>
          )}

          {/* AI messages */}
          {session.messages
            .filter((m) => m.role === "ai")
            .map((msg, i) => (
              <div
                key={`${msg.role}-${i}`}
                className="text-xs text-muted-foreground bg-white/[0.02] rounded p-2 mb-2 max-h-[200px] overflow-y-auto"
              >
                <MarkdownViewer content={msg.content} />
              </div>
            ))}

          {/* Streaming content */}
          {session.streamingContent && (
            <div className="text-xs text-muted-foreground bg-white/[0.02] rounded p-2 mb-2 max-h-[200px] overflow-y-auto">
              <MarkdownViewer content={session.streamingContent} />
            </div>
          )}

          {/* Stop button */}
          {isProcessing && (
            <button
              onClick={session.interrupt}
              className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded
                         bg-red-500/10 text-red-400 border border-red-500/20
                         hover:bg-red-500/20 transition-colors mt-2"
            >
              Stop
            </button>
          )}

          <div ref={messagesEndRef} />
        </div>
      )}

      {/* Preview — show generated files */}
      {phase === "preview" && generatedFiles.length > 0 && (
        <div className="mt-3">
          {/* Show completed steps */}
          {stepStatuses.length > 0 && <StepIndicator steps={stepStatuses} />}

          <p className="text-[10px] text-muted-foreground font-medium mb-2">
            Generated {generatedFiles.length} file{generatedFiles.length !== 1 ? "s" : ""}
            {Object.keys(groupedFiles).length > 1
              ? ` across ${Object.keys(groupedFiles).length} groups`
              : ""}
            :
          </p>

          <div className="flex flex-col gap-2 mb-3">
            {Object.entries(groupedFiles).map(([group, files]) => (
              <div key={group}>
                {/* Group header — only show if multiple groups */}
                {Object.keys(groupedFiles).length > 1 && (
                  <div className="flex items-center gap-1.5 text-[10px] font-medium text-muted-foreground mb-1">
                    <FolderOpen className="w-3 h-3" />
                    {group}
                    <span className="text-muted-foreground/40">
                      ({files.length} file{files.length !== 1 ? "s" : ""})
                    </span>
                  </div>
                )}
                <div className="flex flex-col gap-1">
                  {files.map((file) => {
                    const isSpec = file.filePath.endsWith(".json");
                    const isTutorial = file.filePath.includes("tutorial");
                    const fileIcon = isSpec ? (
                      <BookOpen className="w-3 h-3 text-cyan-400 shrink-0" />
                    ) : isTutorial ? (
                      <BookOpen className="w-3 h-3 text-amber-400 shrink-0" />
                    ) : (
                      <FileCode className="w-3 h-3 text-purple-400 shrink-0" />
                    );

                    return (
                      <div
                        key={file.filePath}
                        className="rounded border border-border bg-white/[0.02]"
                      >
                        <button
                          onClick={() => toggleFile(file.filePath)}
                          className="w-full flex items-center gap-1.5 px-2 py-1.5 text-xs text-left hover:bg-white/5 transition-colors"
                        >
                          {expandedFiles.has(file.filePath) ? (
                            <ChevronDown className="w-3 h-3 text-muted-foreground shrink-0" />
                          ) : (
                            <ChevronRight className="w-3 h-3 text-muted-foreground shrink-0" />
                          )}
                          {fileIcon}
                          <span className="font-medium text-foreground truncate">
                            {file.filePath}
                          </span>
                          <span className="text-[10px] text-muted-foreground/50 ml-auto shrink-0">
                            {file.content.split("\n").length} lines
                          </span>
                        </button>

                        {expandedFiles.has(file.filePath) && (
                          <div className="border-t border-border">
                            <pre className="text-[10px] text-muted-foreground p-2 overflow-x-auto max-h-[400px] overflow-y-auto leading-relaxed">
                              {file.content}
                            </pre>
                          </div>
                        )}
                      </div>
                    );
                  })}
                </div>
              </div>
            ))}
          </div>

          <div className="flex items-center gap-2">
            <button
              onClick={handleApply}
              className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded
                         bg-green-500/10 text-green-400 border border-green-500/20
                         hover:bg-green-500/20 transition-colors"
            >
              <Play className="w-3.5 h-3.5" />
              Apply to Project
            </button>
            <button
              onClick={() => {
                setPhase("idle");
                setGeneratedFiles([]);
                setStepStatuses([]);
              }}
              className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded
                         bg-white/5 text-muted-foreground border border-border
                         hover:bg-white/10 transition-colors"
            >
              Discard
            </button>
          </div>
        </div>
      )}

      {/* Applying state */}
      {phase === "applying" && (
        <div className="mt-3 flex items-center gap-1.5 text-xs text-muted-foreground">
          <Loader2 className="w-3.5 h-3.5 animate-spin" />
          Writing files to project...
        </div>
      )}

      {/* Applied result */}
      {phase === "applied" && writeResult && (
        <div
          className={`mt-3 p-3 rounded border ${
            writeResult.success
              ? "border-green-500/30 bg-green-500/5"
              : "border-red-500/30 bg-red-500/5"
          }`}
        >
          <div className="flex items-center gap-1.5 mb-2">
            {writeResult.success ? (
              <CheckCircle2 className="w-3.5 h-3.5 text-green-400" />
            ) : (
              <AlertTriangle className="w-3.5 h-3.5 text-red-400" />
            )}
            <span className="text-xs font-medium">
              {writeResult.success ? "Integration Applied" : "Some files failed to write"}
            </span>
          </div>

          {writeResult.files_written.length > 0 && (
            <div className="mb-2">
              {writeResult.files_written.map((f, i) => (
                <p key={`${f}-${i}`} className="text-[10px] text-muted-foreground">
                  + {f}
                </p>
              ))}
            </div>
          )}

          {writeResult.warnings.length > 0 && (
            <div>
              {writeResult.warnings.map((w, i) => (
                <p key={`${w}-${i}`} className="text-[10px] text-yellow-400/80">
                  {w}
                </p>
              ))}
            </div>
          )}

          <p className="text-[10px] text-muted-foreground mt-2">
            Restart your dev server to activate the changes. Hooks, registrations, and specs are
            ready for AI-driven workflows in the Specs and Workflows pages.
          </p>
        </div>
      )}

      {/* Error */}
      {error && (
        <p className="text-xs text-red-400 mt-2">
          <AlertTriangle className="w-3 h-3 inline mr-1" />
          {error}
        </p>
      )}
    </div>
  );
}
