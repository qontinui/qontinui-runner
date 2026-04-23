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
  buildArchitectureDiagramPrompt,
  buildExplainerIndexPrompt,
  buildExplainerClusterPrompt,
  buildExplainerPagePrompt,
  type ExplainerSpecSummary,
  type ExplainerCluster,
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

/**
 * Extract Mermaid diagram file from AI output. Mermaid uses `%%` for comments,
 * so the prompt instructs the AI to emit `%% FILE: <path>` inside a
 * ```mermaid block instead of the `// FILE:` convention used for TS files.
 */
function extractMermaidFile(content: string): GeneratedFile | null {
  const regex = /```mermaid\s*\r?\n\s*%%\s*FILE:\s*(.+?)\r?\n([\s\S]*?)```/i;
  const match = regex.exec(content);
  if (!match) return null;
  const filePath = match[1].trim();
  const body = match[2].trimEnd();
  return { filePath, content: `%% FILE: ${filePath}\n${body}\n` };
}

/**
 * Collect explainer inputs from the set of files just generated during a
 * per-page run: reads each .spec.uibridge.json + its paired .arch.mmd.
 * Returns the parsed spec summaries and a map of specId → mermaid body.
 */
function gatherExplainerInputs(files: GeneratedFile[]): {
  specs: ExplainerSpecSummary[];
  arch: Map<string, string>;
} {
  const specs: ExplainerSpecSummary[] = [];
  const arch = new Map<string, string>();
  for (const f of files) {
    if (f.filePath.endsWith(".spec.uibridge.json")) {
      try {
        const json = JSON.parse(f.content) as {
          description?: string;
          groups?: Array<{ id?: string; name?: string; description?: string }>;
        };
        const specId = f.filePath.replace(/^.*\//, "").replace(/\.spec\.uibridge\.json$/, "");
        specs.push({
          specId,
          description: (json.description || "").trim(),
          groups: (json.groups || [])
            .filter((g) => g && g.name)
            .map((g) => ({
              id: g.id || "",
              name: g.name || "",
              description: (g.description || "").trim(),
            })),
        });
      } catch {
        /* skip malformed spec */
      }
    } else if (f.filePath.endsWith(".arch.mmd")) {
      const specId = f.filePath.replace(/^.*\//, "").replace(/\.arch\.mmd$/, "");
      // Strip the leading `%% FILE:` comment line from the content.
      const body = f.content.replace(/^%%\s*FILE:.*\r?\n/, "").trim();
      arch.set(specId, body);
    }
  }
  return { specs, arch };
}

/**
 * Heuristic clustering: group specs by shared first token (split by `-`).
 * Singletons are merged into a catch-all "other" cluster so we don't emit
 * clusters of one. Caps total clusters at 8.
 */
function clusterSpecsByPrefix(specs: ExplainerSpecSummary[]): ExplainerCluster[] {
  const buckets = new Map<string, ExplainerSpecSummary[]>();
  for (const s of specs) {
    const token = s.specId.split(/[-/]/)[0] || s.specId;
    if (!buckets.has(token)) buckets.set(token, []);
    buckets.get(token)!.push(s);
  }
  // Promote buckets with >=2 specs; fold singletons into "other".
  const multi: Array<{ id: string; specs: ExplainerSpecSummary[] }> = [];
  const singletons: ExplainerSpecSummary[] = [];
  for (const [id, items] of buckets) {
    if (items.length >= 2) multi.push({ id, specs: items });
    else singletons.push(...items);
  }
  multi.sort((a, b) => b.specs.length - a.specs.length);
  const top = multi.slice(0, 7);
  const overflow = multi.slice(7).flatMap((m) => m.specs);
  const otherSpecs = [...overflow, ...singletons];
  const result: ExplainerCluster[] = top.map((m) => ({
    id: m.id,
    name: m.id.replace(/\b\w/g, (c) => c.toUpperCase()),
    description: `${m.specs.length} related pages under the "${m.id}" umbrella.`,
    specIds: m.specs.map((s) => s.specId),
  }));
  if (otherSpecs.length > 0) {
    result.push({
      id: "other",
      name: "Other",
      description: `Pages that don't fit cleanly into the other clusters.`,
      specIds: otherSpecs.map((s) => s.specId),
    });
  }
  return result;
}

/**
 * Extract multiple markdown files from AI output. Markdown has no native
 * comment syntax, so the explainer prompts instruct the AI to use an HTML
 * comment on the first line of each file: `<!-- FILE: <path> -->`. Files are
 * delimited by the next FILE comment or end of content.
 */
function extractMarkdownFiles(content: string): GeneratedFile[] {
  const regex = /<!--\s*FILE:\s*(.+?)\s*-->\s*\r?\n/g;
  const results: GeneratedFile[] = [];
  const matches: Array<{ path: string; startOfBody: number }> = [];
  let match;
  while ((match = regex.exec(content)) !== null) {
    matches.push({ path: match[1].trim(), startOfBody: regex.lastIndex });
  }
  for (let i = 0; i < matches.length; i++) {
    const { path, startOfBody } = matches[i];
    const end = i + 1 < matches.length ? matches[i + 1].startOfBody : content.length;
    // Walk back from `end` to before the next FILE marker's opening `<!--`.
    let bodyEnd = end;
    if (i + 1 < matches.length) {
      // `matches[i+1].startOfBody` points past the `\n` after `-->`. Back up to
      // before the `<!--` that opens the next marker so we don't include it.
      const nextOpen = content.lastIndexOf("<!--", matches[i + 1].startOfBody);
      if (nextOpen > 0) bodyEnd = nextOpen;
    }
    const body = content.slice(startOfBody, bodyEnd).trimEnd();
    results.push({
      filePath: path,
      content: `<!-- FILE: ${path} -->\n${body}\n`,
    });
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
  | "page-architecture-diagram"
  | "page-demo-script"
  | "explainer-index"
  | "explainer-cluster"
  | "explainer-page";

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
  | "generating-page-architecture-diagram"
  | "generating-project-explainer"
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
  useEffect(() => {
    allGeneratedFilesRef.current = generatedFiles;
  }, [generatedFiles]);
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
  // Budget: how many pages we process before rolling to a fresh AI session.
  // Keeps context from growing unbounded on large projects — each page ships
  // ~4-6 prompts (registrations, spec, tutorial, …) so 5 pages ≈ 20-30 turns,
  // well inside a single session's window.
  const SESSION_PAGE_BUDGET = 5;
  const pagesInSessionRef = useRef(0);

  // Project Explainer phase: kicks off after per-page generation when the
  // `generateProjectExplainer` flag is set. One AI call per item below; each
  // response is a single markdown file saved to src/specs/explainer/.
  interface ExplainerQueueItem {
    kind: "index" | "cluster" | "page";
    /** For cluster/page items. */
    clusterId?: string;
    /** For page items. */
    specId?: string;
  }
  const explainerQueueRef = useRef<ExplainerQueueItem[]>([]);
  const explainerContextRef = useRef<{
    specs: ExplainerSpecSummary[];
    arch: Map<string, string>;
    clusters: ExplainerCluster[];
    projectName: string;
  } | null>(null);
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
  // AI prompt anchors the flow). The function is nested inside the session-
  // transition useEffect's closure; flattening the whole graph would be
  // disruptive, so we pass it out via a ref instead.
  const chainToDemoScriptRef = useRef<((route: string, signal: AbortSignal) => void) | null>(null);
  const currentControllerRef = useRef<AbortController | null>(null);

  // Pages as originally passed to handleGeneratePages — pageQueueRef gets
  // drained during the run, so the Project Explainer falls back to this
  // when it needs to load specs from disk (for demo/tour + explainer runs
  // where no fresh specs were generated).
  const originalPagesRef = useRef<PageComponent[]>([]);

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

  // Page route being processed — mirrored from currentPageRef so the UI can
  // re-render on page transitions without reading the ref during render.
  const [currentPageName, setCurrentPageName] = useState<string>("");

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
        currentStep === "page-tutorial" ||
        currentStep === "page-architecture-diagram" ||
        currentStep === "explainer-index" ||
        currentStep === "explainer-cluster" ||
        currentStep === "explainer-page";
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
        } else if (opts.generateArchitectureDiagrams && cur?.source) {
          chainToArchitectureDiagram(cur);
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
        } else if (opts.generateArchitectureDiagrams && cur?.source) {
          chainToArchitectureDiagram(cur);
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
        const cur = currentPageRef.current;
        if (opts.generateArchitectureDiagrams && cur?.source) {
          chainToArchitectureDiagram(cur);
        } else if (opts.generateDemoVideos || opts.generateProductTours) {
          chainToDemoScript(cur?.page.route ?? "", controller.signal);
        } else {
          advanceToNextPage(controller.signal);
        }
      } else if (currentStep === "page-architecture-diagram") {
        processedMessageCountRef.current = aiMessages.length;
        const diag = extractMermaidFile(fullContent);
        if (diag) {
          setGeneratedFiles((prev) => [...prev, diag]);
          setStepStatuses((prev) =>
            prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
          );
        } else {
          // No Mermaid block came back — mark the step errored but keep going.
          setStepStatuses((prev) =>
            prev.map((s) => (s.state === "active" ? { ...s, state: "error" } : s)),
          );
        }
        const opts = pageOptionsRef.current;
        const cur = currentPageRef.current;
        if (opts.generateDemoVideos || opts.generateProductTours) {
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
      } else if (
        currentStep === "explainer-index" ||
        currentStep === "explainer-cluster" ||
        currentStep === "explainer-page"
      ) {
        processedMessageCountRef.current = aiMessages.length;
        const files = extractMarkdownFiles(fullContent);
        if (files.length > 0) {
          setGeneratedFiles((prev) => [...prev, ...files]);
          setStepStatuses((prev) =>
            prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
          );
        } else {
          setStepStatuses((prev) =>
            prev.map((s) => (s.state === "active" ? { ...s, state: "error" } : s)),
          );
        }
        advanceExplainerQueue();
      }

      // Helper: chain to the architecture-diagram step. Uses the same source
      // + registrations we already fetched for this page.
      function chainToArchitectureDiagram(cur: {
        page: PageComponent;
        source: ReadPageSourceResult | null;
        registrationOutput: string;
      }) {
        if (!cur.source) {
          advanceToNextPage(controller.signal);
          return;
        }
        pendingStepRef.current = "page-architecture-diagram";
        setPhase("generating-page-architecture-diagram");
        setStepStatuses((prev) => [
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
        sendMessage(`Now generate the architecture diagram for this page.\n\n` + archPrompt);
      }

      // Helper: chain to demo script + product tour planning (non-AI — calls planner APIs directly)
      // Exposed via ref so handleGeneratePages can invoke it for demo/tour-only
      // re-runs that have no AI prompt to anchor the first-page flow on.
      chainToDemoScriptRef.current = chainToDemoScript;
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
          // If this run didn't generate specs (demo/tour-only re-run), the
          // in-memory lastSpecJsonRef is either empty or leftover from a
          // previous page — neither is right for the current page. Always
          // re-read the current page's spec from disk in that case so each
          // page's demo/tour uses its own spec.
          if (!opts.generateSpecs) {
            const slug = route.replace(/^\//, "").replace(/\//g, "-") || "root";
            const existing = await readProjectFile(`src/specs/${slug}.spec.uibridge.json`, signal);
            if (signal.aborted) return;
            lastSpecJsonRef.current = existing || "";
          }
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
          // Count the page we just finished (the one before this advance).
          pagesInSessionRef.current += 1;
          // Rotate to a fresh AI session at the batch boundary so context
          // doesn't grow unbounded on large projects. Each batch is
          // independent — the per-page prompts already include the page
          // source, so a fresh session doesn't lose information.
          if (pagesInSessionRef.current >= SESSION_PAGE_BUDGET) {
            pagesInSessionRef.current = 0;
            setStepStatuses((prev) => [
              ...prev,
              { state: "active", label: `Rotating to fresh AI session...` },
            ]);
            (async () => {
              if (signal.aborted) return;
              await session.close();
              if (signal.aborted) return;
              session.resetSession();
              const id = await session.createSession("Page Preparation: AI Generation (batch)");
              if (signal.aborted) return;
              if (!id) {
                setError("Failed to rotate AI session");
                return;
              }
              setStepStatuses((prev) =>
                prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
              );
              startPageGeneration(nextPage, signal);
            })();
            return;
          }
          startPageGeneration(nextPage, signal);
        } else {
          // All pages done
          pendingStepRef.current = null;
          currentPageRef.current = null;
          setCurrentPageName("");

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
              if (!(await maybeStartExplainerPhase())) {
                setPhase("preview");
              }
            })();
          } else {
            (async () => {
              if (!(await maybeStartExplainerPhase())) {
                setPhase("preview");
              }
            })();
          }
        }
      }

      /** If generateProjectExplainer is on and we haven't started yet, kick off
       * the explainer phase (index → clusters → pages). Returns true when the
       * phase was started so the caller knows to NOT transition to preview.
       *
       * Async because when no specs were generated in this run (e.g. a
       * demo/tour + explainer re-run), we fall back to reading existing specs
       * + architecture diagrams from disk for every page in the run, so the
       * explainer can still compose a document. */
      async function maybeStartExplainerPhase(): Promise<boolean> {
        if (!pageOptionsRef.current.generateProjectExplainer) return false;
        if (explainerContextRef.current) return false; // already running

        let { specs, arch } = gatherExplainerInputs(allGeneratedFilesRef.current);

        if (specs.length === 0) {
          // No fresh specs in memory — fall back to reading existing specs +
          // architecture diagrams from disk for each page we were asked about.
          const diskFiles: GeneratedFile[] = [];
          for (const p of originalPagesRef.current) {
            if (controller.signal.aborted) return false;
            const slug = p.route.replace(/^\//, "").replace(/\//g, "-") || "root";
            const specBody = await readProjectFile(
              `src/specs/${slug}.spec.uibridge.json`,
              controller.signal,
            );
            if (specBody) {
              diskFiles.push({
                filePath: `${slug}.spec.uibridge.json`,
                content: specBody,
              });
            }
            const archBody = await readProjectFile(
              `src/specs/architecture/${slug}.arch.mmd`,
              controller.signal,
            );
            if (archBody) {
              diskFiles.push({
                filePath: `src/specs/architecture/${slug}.arch.mmd`,
                content: archBody,
              });
            }
          }
          if (controller.signal.aborted) return false;
          const fromDisk = gatherExplainerInputs(diskFiles);
          specs = fromDisk.specs;
          arch = fromDisk.arch;
        }

        if (specs.length === 0) return false; // still nothing — give up
        const clusters = clusterSpecsByPrefix(specs);
        const projectName = projectPath.split(/[\\/]/).filter(Boolean).slice(-1)[0] || "Project";
        explainerContextRef.current = { specs, arch, clusters, projectName };
        // Build the queue: one index, N clusters, then all pages grouped by cluster.
        const queue: ExplainerQueueItem[] = [{ kind: "index" }];
        for (const c of clusters) queue.push({ kind: "cluster", clusterId: c.id });
        for (const c of clusters) {
          for (const specId of c.specIds) {
            queue.push({ kind: "page", clusterId: c.id, specId });
          }
        }
        explainerQueueRef.current = queue;
        explainerCallsInSessionRef.current = 0;
        setStepStatuses((prev) => [
          ...prev,
          {
            state: "active",
            label: `Project Explainer (${queue.length} files: index + ${clusters.length} clusters + ${specs.length} pages)`,
          },
        ]);
        advanceExplainerQueue();
        return true;
      }

      /** Pick the next explainer item and fire its prompt. Session-rotates at
       * the same SESSION_PAGE_BUDGET boundary used by per-page generation. */
      function advanceExplainerQueue() {
        const queue = explainerQueueRef.current;
        const ctx = explainerContextRef.current;
        if (!ctx || queue.length === 0) {
          explainerContextRef.current = null;
          explainerCurrentRef.current = null;
          setStepStatuses((prev) =>
            prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
          );
          setPhase("preview");
          return;
        }
        // Session rotation
        if (explainerCallsInSessionRef.current >= SESSION_PAGE_BUDGET) {
          explainerCallsInSessionRef.current = 0;
          setStepStatuses((prev) => [
            ...prev,
            { state: "active", label: `Rotating to fresh AI session (explainer)...` },
          ]);
          (async () => {
            if (controller.signal.aborted) return;
            await session.close();
            if (controller.signal.aborted) return;
            session.resetSession();
            const id = await session.createSession("Project Explainer (batch)");
            if (controller.signal.aborted) return;
            if (!id) {
              setError("Failed to rotate AI session during explainer phase");
              return;
            }
            setStepStatuses((prev) =>
              prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
            );
            advanceExplainerQueue();
          })();
          return;
        }
        explainerCallsInSessionRef.current++;
        const next = queue.shift()!;
        explainerCurrentRef.current = next;
        setPhase("generating-project-explainer");
        if (next.kind === "index") {
          pendingStepRef.current = "explainer-index";
          setStepStatuses((prev) => [...prev, { state: "active", label: "Explainer: index.md" }]);
          sendMessage(buildExplainerIndexPrompt(ctx.projectName, ctx.specs, ctx.clusters));
        } else if (next.kind === "cluster") {
          const cluster = ctx.clusters.find((c) => c.id === next.clusterId);
          if (!cluster) return advanceExplainerQueue();
          const specsInCluster = ctx.specs.filter((s) => cluster.specIds.includes(s.specId));
          const otherClusters = ctx.clusters
            .filter((c) => c.id !== cluster.id)
            .map((c) => ({ id: c.id, name: c.name, description: c.description }));
          pendingStepRef.current = "explainer-cluster";
          setStepStatuses((prev) => [
            ...prev,
            { state: "active", label: `Explainer: ${cluster.id}.md (cluster)` },
          ]);
          sendMessage(
            buildExplainerClusterPrompt(ctx.projectName, cluster, specsInCluster, otherClusters),
          );
        } else {
          // kind === "page"
          const cluster = ctx.clusters.find((c) => c.id === next.clusterId);
          const spec = ctx.specs.find((s) => s.specId === next.specId);
          if (!cluster || !spec) return advanceExplainerQueue();
          const slug = spec.specId.replace(/[^a-z0-9-]/gi, "-");
          const archDiagram = ctx.arch.get(spec.specId) || null;
          const siblings = ctx.specs
            .filter((s) => cluster.specIds.includes(s.specId) && s.specId !== spec.specId)
            .map((s) => ({
              slug: s.specId.replace(/[^a-z0-9-]/gi, "-"),
              title: s.specId,
              tagline: (s.description || "").replace(/\s+/g, " ").slice(0, 80),
            }));
          pendingStepRef.current = "explainer-page";
          setStepStatuses((prev) => [
            ...prev,
            { state: "active", label: `Explainer: ${cluster.id}/${slug}.md` },
          ]);
          sendMessage(
            buildExplainerPagePrompt(
              ctx.projectName,
              cluster.id,
              slug,
              spec,
              archDiagram,
              siblings,
            ),
          );
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
        setCurrentPageName(page.route);
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
        } else if (opts.generateArchitectureDiagrams) {
          pendingStepRef.current = "page-architecture-diagram";
          setPhase("generating-page-architecture-diagram");
          setStepStatuses((prev) => [
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
          await sendMessage(`Generate an architecture diagram for ${page.route}.\n\n` + archPrompt);
        } else if (opts.generateDemoVideos || opts.generateProductTours) {
          // Only demo videos / product tours selected — chain directly
          chainToDemoScript(page.route, signal);
        }
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [projectPath, analysis.framework, isRegenSpec, includeArchSpec],
  );

  useEffect(() => {
    const controller = new AbortController();
    currentControllerRef.current = controller;
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
      if (currentControllerRef.current === controller) {
        currentControllerRef.current = null;
      }
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

      // Guard: the Project Explainer phase runs AFTER per-page generation
      // and composes the just-generated specs + architecture diagrams. If
      // no per-page work is selected there's nothing to compose from, and
      // the initial-prompt dispatch below has no branch to fall into, so
      // the run would stall silently. Surface this clearly instead.
      const hasPerPageWork =
        options.generateRegistrations ||
        options.generateDataPageIds ||
        options.generateSpecs ||
        options.generateTutorials ||
        options.generateArchitectureDiagrams ||
        options.generateDemoVideos ||
        options.generateProductTours;
      if (!hasPerPageWork) {
        setError(
          options.generateProjectExplainer
            ? "Project Explainer composes generated specs into a document — please also enable 'Page Specs' (and ideally 'Architecture Diagrams') so there is content to compose."
            : "Please select at least one generation option.",
        );
        return;
      }

      setError(null);
      setGeneratedFiles([]);
      setWriteResult(null);
      pageQueueRef.current = [...pages];
      originalPagesRef.current = [...pages];
      pageOptionsRef.current = options;
      processedMessageCountRef.current = 0;

      // Build initial step statuses
      const steps: StepStatus[] = pages.map((p) => ({
        state: "pending" as const,
        label: `${p.route} (${[
          options.generateRegistrations && "regs",
          options.generateSpecs && "spec",
          options.generateTutorials && "tutorial",
          options.generateArchitectureDiagrams && "arch",
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
      pagesInSessionRef.current = 0;

      const id = await session.createSession("Page Preparation: AI Generation");
      if (!id) {
        setError("Failed to create AI session");
        return;
      }

      // Start first page
      const firstPage = pageQueueRef.current.shift()!;
      currentPageRef.current = { page: firstPage, source: null, registrationOutput: "" };
      setCurrentPageName(firstPage.route);
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
      } else if (options.generateArchitectureDiagrams) {
        pendingStepRef.current = "page-architecture-diagram";
        setPhase("generating-page-architecture-diagram");
        const archPrompt = buildArchitectureDiagramPrompt(
          source.main_source,
          source.imported_sources,
          firstPage.component_name,
          firstPage.route,
          "",
        );
        await session.sendMessage(
          `Generate an architecture diagram for ${firstPage.route}.\n\n` + archPrompt,
        );
      } else if (options.generateDemoVideos || options.generateProductTours) {
        // Demo/tour-only is non-AI planning; there's no prompt to send to
        // anchor the first-page flow on. Invoke chainToDemoScript directly
        // via the ref bridged out of handleSessionTransition's nested scope.
        // chainToDemoScript handles per-page spec loading (from disk when
        // specs weren't generated this run), planning, and advancement to
        // subsequent pages / the all-done branch.
        const chain = chainToDemoScriptRef.current;
        const ctrl = currentControllerRef.current;
        if (chain && ctrl) {
          chain(firstPage.route, ctrl.signal);
        } else {
          setError(
            "Demo/tour bootstrap couldn't dispatch: session helpers not ready. Try again in a moment.",
          );
          setPhase("idle");
        }
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
          {}
          {isPerPagePhase && currentPageName && (
            <div className="flex items-center gap-1.5 text-xs text-cyan-400 font-medium mb-2">
              <FolderOpen className="w-3.5 h-3.5" />
              Processing: {currentPageName}
            </div>
          )}
          {}
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
