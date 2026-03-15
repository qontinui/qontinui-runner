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
import type { ProjectAnalysis, ApiResponse, WriteHooksResult } from "./types";
import { getApiBase } from "@/lib/runner-api";

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

type IntegrationStep = "hooks" | "architecture-spec";

interface StepStatus {
  state: "pending" | "active" | "done" | "skipped" | "error";
  label: string;
}

type PanelPhase =
  | "idle"
  | "generating-hooks"
  | "generating-spec"
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
        <div key={i} className="flex items-center gap-2 text-xs">
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

  // When AI transitions to ready, handle the current step completion
  useEffect(() => {
    const controller = new AbortController();

    const prevState = prevSessionStateRef.current;
    prevSessionStateRef.current = session.sessionState;

    // Only process when transitioning from "processing" to "ready" —
    // ignore the initial "ready" from createSession (before sendMessage)
    if (session.sessionState !== "ready" || prevState !== "processing") return;
    const currentStep = pendingStepRef.current;
    if (!currentStep) return;

    // Gather all AI content
    const allContent = session.messages
      .filter((m) => m.role === "ai")
      .map((m) => m.content)
      .join("\n\n");
    const fullContent = session.streamingContent
      ? allContent + "\n\n" + session.streamingContent
      : allContent;

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
          async function fetchAndSendSpecPrompt() {
            let specPrompt: string;
            if (isRegenSpec) {
              let existingSpec = "";
              try {
                // Try to read existing spec — the AI session already has project context
                const resp = await fetch(`${getApiBase()}/ui-bridge/integration/read-file`, {
                  method: "POST",
                  headers: { "Content-Type": "application/json" },
                  body: JSON.stringify({
                    project_path: projectPath,
                    file_path: "project.architecture.uibridge.json",
                  }),
                  signal: controller.signal,
                });
                const data = await resp.json();
                if (data.success && data.data) existingSpec = data.data;
              } catch (_e) {
                if (controller.signal.aborted) return;
                // Fall through to fresh generation
              }
              specPrompt = existingSpec
                ? buildArchitectureSpecRegenPrompt(analysisForPrompt, existingSpec)
                : buildArchitectureSpecPrompt(analysisForPrompt);
            } else {
              specPrompt = buildArchitectureSpecPrompt(analysisForPrompt);
            }
            if (controller.signal.aborted) return;
            await session.sendMessage(
              "Now generate an architecture spec for this project. You already have context from the hook generation step — use what you learned.\n\n" +
                specPrompt,
            );
          }
          fetchAndSendSpecPrompt();
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
      const specContent = session.messages
        .filter((m) => m.role === "ai")
        .map((m) => m.content)
        .join("\n\n");
      const fullSpecContent = session.streamingContent
        ? specContent + "\n\n" + session.streamingContent
        : specContent;

      if (!tryExtractSpec(fullSpecContent)) {
        // Race condition: state changed to "ready" before all text events arrived.
        // Retry after a delay by fetching the full output from the runner API.
        const taskRunId = session.taskRunId;
        if (specRetryTimerRef.current) clearTimeout(specRetryTimerRef.current);
        specRetryTimerRef.current = setTimeout(async () => {
          specRetryTimerRef.current = null;
          if (!taskRunId) {
            handleSpecError();
            return;
          }

          try {
            const resp = await fetch(
              `${getApiBase()}/task-runs/${taskRunId}/output?tail_chars=200000`,
              { signal: controller.signal },
            );
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
      }
    }

    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session.sessionState]);

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
  const isGenerating = phase === "generating-hooks" || phase === "generating-spec";

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
        AI analyzes your project&apos;s source code and generates UI Bridge hooks and an
        architecture spec — fully setting up the project for AI-driven workflows.
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
      {isGenerating && stepStatuses.length > 0 && <StepIndicator steps={stepStatuses} />}

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
                key={i}
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
            Generated {generatedFiles.length} file{generatedFiles.length !== 1 ? "s" : ""}:
          </p>

          <div className="flex flex-col gap-1.5 mb-3">
            {generatedFiles.map((file) => {
              const isSpec = file.filePath.endsWith(".json");
              return (
                <div key={file.filePath} className="rounded border border-border bg-white/[0.02]">
                  <button
                    onClick={() => toggleFile(file.filePath)}
                    className="w-full flex items-center gap-1.5 px-2 py-1.5 text-xs text-left hover:bg-white/5 transition-colors"
                  >
                    {expandedFiles.has(file.filePath) ? (
                      <ChevronDown className="w-3 h-3 text-muted-foreground shrink-0" />
                    ) : (
                      <ChevronRight className="w-3 h-3 text-muted-foreground shrink-0" />
                    )}
                    {isSpec ? (
                      <BookOpen className="w-3 h-3 text-cyan-400 shrink-0" />
                    ) : (
                      <FileCode className="w-3 h-3 text-purple-400 shrink-0" />
                    )}
                    <span className="font-medium text-foreground truncate">{file.filePath}</span>
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
                <p key={i} className="text-[10px] text-muted-foreground">
                  + {f}
                </p>
              ))}
            </div>
          )}

          {writeResult.warnings.length > 0 && (
            <div>
              {writeResult.warnings.map((w, i) => (
                <p key={i} className="text-[10px] text-yellow-400/80">
                  {w}
                </p>
              ))}
            </div>
          )}

          <p className="text-[10px] text-muted-foreground mt-2">
            Restart your dev server to activate the hooks. The architecture spec is ready for
            workflow generation in the Specs page.
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
