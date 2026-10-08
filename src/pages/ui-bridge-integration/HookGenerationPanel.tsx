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
  BookOpen,
  FolderOpen,
} from "lucide-react";
import { useAiSession } from "@/hooks/useAiSession";
import { MarkdownViewer } from "@/components/MarkdownViewer";
import { StreamingMessageView } from "@/components/shared/StreamingMessageView";
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
  WriteHooksResult,
  PageComponent,
  PageGenerationOptions,
} from "./types";
import {
  buildRegistrationPrompt,
  extractInlineRegistrations,
  buildPageSpecPrompt,
  buildTutorialPrompt,
  buildArchitectureDiagramPrompt,
} from "@/lib/page-analysis-prompt-builder";
import { describeThrown } from "@/lib/utils";
import { type GeneratedFile, groupFilesByPage } from "./hook-generation/parse";
import type { PanelPhase, StepStatus } from "./hook-generation/steps/types";
import { StepIndicator } from "./hook-generation/StepIndicator";
import { useStepMachine } from "./hook-generation/useStepMachine";
import { recordPreviewPrompt } from "./hook-generation/previewPrompts";
import { useCoordinatorEvents } from "./hook-generation/useCoordinatorEvents";
import {
  readFile,
  readPageSource,
  writeHooks,
  cacheArchitectureSpec,
  type WriteHooksFile,
} from "./integrationApi";

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
  // Page route being processed — mirrored from currentPageRef so the UI can
  // re-render on page transitions without reading the ref during render.
  const [currentPageName, setCurrentPageName] = useState<string>("");
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const prevSessionStateRef = useRef<string>(session.sessionState);

  const isRegenHooks = analysis.has_generated_hooks;
  const isRegenSpec = analysis.has_architecture_spec;

  // The AI step machine: its refs + handleSessionTransition.
  const {
    pendingStepRef,
    pageQueueRef,
    pagesInSessionRef,
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
    handleSessionTransition,
  } = useStepMachine({
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
  });

  // Auto-scroll on streaming content
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [session.streamingContent, session.messages]);

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

  // When entering preview, ensure all files are expanded. Detect the
  // transition during render so the setState is inline (allowed pattern)
  // instead of via a post-render effect (set-state-in-effect).
  const [prevPreviewKey, setPrevPreviewKey] = useState<string | null>(null);
  const previewKey = phase === "preview" ? generatedFiles.map((f) => f.filePath).join("\n") : null;
  if (prevPreviewKey !== previewKey) {
    setPrevPreviewKey(previewKey);
    if (phase === "preview") {
      setExpandedFiles(new Set(generatedFiles.map((f) => f.filePath)));
    }
  }

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
          const data = await readFile(projectPath, "src/lib/ui-bridge/UIBridgeHooks.tsx");
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
          const data = await readFile(projectPath, "project.architecture.uibridge.json");
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
    // Stable refs owned by useStepMachine (listed for exhaustive-deps).
    pendingStepRef,
  ]);

  // Start per-page AI generation (called from PageSelectionPanel via window event)
  const handleGeneratePages = useCallback(
    async (
      pages: PageComponent[],
      options: PageGenerationOptions,
      previewOnly: boolean = false,
      analysisOverride: ProjectAnalysis | null = null,
    ) => {
      if (pages.length === 0) return;
      const effectiveAnalysis: ProjectAnalysis = analysisOverride ?? analysis;
      effectiveAnalysisRef.current = effectiveAnalysis;

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

      if (previewOnly && options.generateRegistrations) {
        const doneLabels: string[] = [];
        const failedLabels: string[] = [];
        for (let i = 0; i < pages.length; i++) {
          const p = pages[i];
          setStepStatuses((prev) =>
            prev.map((s, idx) => (idx === i ? { ...s, state: "active" } : s)),
          );
          try {
            const pData = await readPageSource({
              projectPath,
              componentPath: p.component_path,
              maxDepth: 2,
            });
            if (!pData.success || !pData.data) {
              failedLabels.push(`${p.route} (preview: source unavailable)`);
              setStepStatuses((prev) =>
                prev.map((s, idx) => (idx === i ? { ...s, state: "error" } : s)),
              );
              continue;
            }
            const pSource = pData.data;
            const pPrompt = buildRegistrationPrompt(
              pSource.main_source,
              pSource.imported_sources,
              p.component_name,
              p.route,
              effectiveAnalysis.framework,
              undefined,
              extractInlineRegistrations(pSource.main_source),
            );
            recordPreviewPrompt(p.route, p.component_name, pPrompt);
            doneLabels.push(`${p.route} (preview)`);
            setStepStatuses((prev) =>
              prev.map((s, idx) => (idx === i ? { ...s, state: "done" } : s)),
            );
          } catch {
            failedLabels.push(`${p.route} (preview: fetch failed)`);
            setStepStatuses((prev) =>
              prev.map((s, idx) => (idx === i ? { ...s, state: "error" } : s)),
            );
          }
        }
        pageQueueRef.current = [];
        window.dispatchEvent(
          new CustomEvent("ui-bridge-generate-pages-complete", {
            detail: {
              filesGenerated: 0,
              doneSteps: doneLabels,
              failedSteps: failedLabels,
            },
          }),
        );
        setPhase("idle");
        return;
      }

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
        const data = await readPageSource({
          projectPath,
          componentPath: firstPage.component_path,
          maxDepth: 2,
        });
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
          effectiveAnalysis.framework,
          undefined,
          // Inline `useUIElement` / `useUIComponent` calls the page already
          // contains. Prevents duplicate registrations in the side-file.
          extractInlineRegistrations(source.main_source),
        );
        if (typeof window !== "undefined")
          recordPreviewPrompt(firstPage.route, firstPage.component_name, prompt);
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
    [
      session,
      analysis,
      projectPath,
      // Stable refs owned by useStepMachine (listed for exhaustive-deps).
      chainToDemoScriptRef,
      currentControllerRef,
      currentPageRef,
      demoVideoScriptsRef,
      effectiveAnalysisRef,
      lastSpecJsonRef,
      originalPagesRef,
      pageOptionsRef,
      pageQueueRef,
      pagesInSessionRef,
      pendingStepRef,
      processedMessageCountRef,
      productToursRef,
    ],
  );

  // Listen for page generation trigger from PageSelectionPanel via CustomEvent
  useEffect(() => {
    const handler = (e: Event) => {
      const detail = (e as CustomEvent).detail as
        | {
            pages: PageComponent[];
            options: PageGenerationOptions;
            previewOnly?: boolean;
            analysis?: ProjectAnalysis | null;
          }
        | undefined;
      if (detail) {
        handleGeneratePages(
          detail.pages,
          detail.options,
          detail.previewOnly ?? false,
          detail.analysis ?? null,
        );
      }
    };
    window.addEventListener("ui-bridge-generate-pages", handler);
    return () => window.removeEventListener("ui-bridge-generate-pages", handler);
  }, [handleGeneratePages]);

  // Broadcast phase transitions and generation errors to the coordinator.
  useCoordinatorEvents({ phase, stepStatuses, writeResult, error, allGeneratedFilesRef });

  // Apply generated files to project
  const handleApply = useCallback(async () => {
    if (generatedFiles.length === 0) return;

    setPhase("applying");
    setError(null);

    const files: WriteHooksFile[] = generatedFiles.map((f) => ({
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
      const data = await writeHooks(projectPath, files);
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
            await cacheArchitectureSpec(projectPath, specFile.content);
          } catch {
            // Non-critical — spec written to disk, just not cached
          }
        }
      } else {
        setError(data.error || "Failed to write files");
        setPhase("preview");
      }
    } catch (err) {
      setError(describeThrown(err, "Failed to write files"));
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
  const groupedFiles = groupFilesByPage(generatedFiles);

  return (
    <div className="p-4 rounded-lg border border-border bg-card/50" data-task-phase={phase}>
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

          {/* Streaming content — bounded plain tail while in flight; the
              completed message renders full markdown above once the turn ends.
              (plan 2026-07-28 §A5a/A5b: a full-buffer markdown re-parse per
              streamed line is O(n²).) */}
          {session.streamingContent && (
            <div className="text-xs text-muted-foreground bg-white/[0.02] rounded p-2 mb-2 max-h-[200px] overflow-y-auto">
              <StreamingMessageView
                content={session.streamingContent}
                droppedChars={session.streamingDroppedChars}
                caretClassName="bg-primary"
              />
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
