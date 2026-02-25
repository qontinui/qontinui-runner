/**
 * AiGeneratePanel.tsx
 *
 * Primary AI workflow generation panel for the runner.
 * Displayed as the default view when no workflow is loaded in the builder.
 * Features: description textarea, templates, spec integration, context attachment,
 * advanced options, and Generate/Generate & Run actions.
 */

import React, { useState, useEffect, useCallback, useMemo, useRef } from "react";
import {
  Sparkles,
  Loader2,
  ChevronDown,
  ChevronRight,
  FileText,
  FolderOpen,
  Plus,
  Play,
  Info,
  Layers,
  GitCompare,
  Globe,
  TestTube2,
  Activity,
  Monitor,
  Rocket,
  ListOrdered,
  X,
  Save,
  AlertCircle,
  CheckCircle2,
  type LucideIcon,
} from "lucide-react";
import { getAccentColors } from "@/design-system";
import { SpecSourceSection, type SpecSourceState } from "./SpecSourceSection";
import { buildSemanticSpecPrompt } from "@/lib/spec-prompt-builder";
import {
  GENERATION_TEMPLATES,
  type WorkflowGenerationTemplate,
} from "@/lib/workflow-generation-templates";

const API_BASE = "http://localhost:9876";

// Icon lookup map for template icons
const TEMPLATE_ICONS: Record<string, LucideIcon> = {
  GitCompare,
  Globe,
  TestTube2,
  Activity,
  Monitor,
  Rocket,
  ListOrdered,
};

// Provider and model constants
const PROVIDERS = [
  { value: "claude_cli", label: "Claude CLI" },
  { value: "claude_api", label: "Claude API" },
  { value: "gemini_cli", label: "Gemini CLI" },
  { value: "gemini_api", label: "Gemini API" },
] as const;

const CLAUDE_MODELS = [
  { value: "claude-sonnet-4", label: "Claude Sonnet 4" },
  { value: "claude-opus-4", label: "Claude Opus 4" },
  { value: "claude-3-5-sonnet", label: "Claude 3.5 Sonnet" },
  { value: "claude-3-opus", label: "Claude 3 Opus" },
];

const GEMINI_MODELS = [
  { value: "gemini-3-flash", label: "Gemini 3 Flash" },
  { value: "gemini-3-pro", label: "Gemini 3 Pro" },
  { value: "gemini-2.5-flash", label: "Gemini 2.5 Flash" },
  { value: "gemini-2.5-pro", label: "Gemini 2.5 Pro" },
  { value: "gemini-2.0-flash", label: "Gemini 2.0 Flash" },
];

// =============================================================================
// Auto-run localStorage signal
// =============================================================================

export const AUTO_RUN_AFTER_GENERATE_KEY = "auto-run-after-generate";
export interface AutoRunAfterGenerate {
  taskRunId: string;
  timestamp: number;
}

// =============================================================================
// Types
// =============================================================================

interface SavedContext {
  id: string;
  name: string;
  scope: string;
  category?: string;
  content?: string;
}

interface SavedPrompt {
  id: string;
  name: string;
  content: string;
  category?: string;
  tags?: string[];
}

export interface AiGeneratePanelProps {
  onCreateManually: () => void;
  onNavigateToActiveRuns: () => void;
}

// =============================================================================
// Auto-save generation prompts to prompt library
// =============================================================================

async function autoSaveGenerationPrompt(promptText: string): Promise<void> {
  try {
    const resp = await fetch(`${API_BASE}/prompts`);
    const json = await resp.json();
    const existing: SavedPrompt[] = json.data ?? [];
    const trimmed = promptText.trim();
    const isDuplicate = existing.some(
      (p) => p.category === "Generation" && p.content.trim() === trimmed,
    );
    if (isDuplicate) return;

    const name = trimmed.length > 60 ? trimmed.substring(0, 57) + "..." : trimmed;

    await fetch(`${API_BASE}/prompts`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        name,
        content: trimmed,
        category: "Generation",
        description: "",
        tags: ["auto-saved"],
      }),
    });
  } catch {
    // Best-effort, don't block user flow
  }
}

// =============================================================================
// AiGeneratePanel Component
// =============================================================================

export function AiGeneratePanel({
  onCreateManually,
  onNavigateToActiveRuns,
}: AiGeneratePanelProps) {
  const accentColors = getAccentColors("blue");

  // Form state — description is persisted to localStorage
  const [description, setDescription] = useState("");
  const [selectedContextIds, setSelectedContextIds] = useState<string[]>([]);
  const [inlineContext, setInlineContext] = useState("");
  const [filePath, setFilePath] = useState("");
  const [isImportingFile, setIsImportingFile] = useState(false);
  const [submittingAction, setSubmittingAction] = useState<"generate" | "generate-and-run" | null>(
    null,
  );
  const [error, setError] = useState<string | null>(null);
  const [importFeedback, setImportFeedback] = useState<{
    type: "success" | "error";
    message: string;
  } | null>(null);

  // Hydrate description from localStorage after mount
  useEffect(() => {
    const saved = localStorage.getItem("generate-workflow-prompt");
    if (saved) setDescription(saved);
  }, []);

  // Persist prompt to localStorage on change
  useEffect(() => {
    localStorage.setItem("generate-workflow-prompt", description);
  }, [description]);

  // Advanced options
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [category, setCategory] = useState("");
  const [tagsInput, setTagsInput] = useState("");
  const [maxIterations, setMaxIterations] = useState("");
  const [provider, setProvider] = useState("");
  const [model, setModel] = useState("");
  const [maxFixIterations, setMaxFixIterations] = useState("");
  const [autoIncludeContexts, setAutoIncludeContexts] = useState(true);
  const [discoveryMode, setDiscoveryMode] = useState<"auto" | "enabled" | "disabled">("auto");

  // Context section
  const [showContext, setShowContext] = useState(false);
  const [contextTab, setContextTab] = useState<"saved" | "custom" | "file">("saved");

  // Page Specs section
  const [specState, setSpecState] = useState<SpecSourceState>({
    discoveredSpecs: [],
    selectedGroupIds: new Set(),
    discoveredPages: [],
    selectedPageUrls: new Set(),
  });
  const hasSpecs = specState.discoveredSpecs.length > 0 && specState.selectedGroupIds.size > 0;

  // Template picker
  const [showTemplates, setShowTemplates] = useState(false);
  const [isSavingTemplate, setIsSavingTemplate] = useState(false);
  const templatePopoverRef = useRef<HTMLDivElement>(null);

  // Click-away listener for template popover
  useEffect(() => {
    if (!showTemplates) return;
    const handleClickOutside = (e: MouseEvent) => {
      if (templatePopoverRef.current && !templatePopoverRef.current.contains(e.target as Node)) {
        setShowTemplates(false);
      }
    };
    document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
  }, [showTemplates]);

  // Saved prompts (generation category)
  const [savedPrompts, setSavedPrompts] = useState<SavedPrompt[]>([]);
  const generationPrompts = useMemo(
    () => savedPrompts.filter((p) => p.category === "Generation"),
    [savedPrompts],
  );

  // Saved contexts
  const [savedContexts, setSavedContexts] = useState<SavedContext[]>([]);

  // Fetch prompts and contexts on mount
  useEffect(() => {
    const controller = new AbortController();
    let cancelled = false;

    (async () => {
      try {
        const resp = await fetch(`${API_BASE}/prompts`, { signal: controller.signal });
        const json = await resp.json();
        if (!cancelled) setSavedPrompts(json.data ?? []);
      } catch {
        // Runner may not be running or request was aborted
      }
    })();
    (async () => {
      try {
        const resp = await fetch(`${API_BASE}/contexts`, { signal: controller.signal });
        const json = await resp.json();
        if (!cancelled) setSavedContexts(json.data ?? []);
      } catch {
        // Runner may not be running or request was aborted
      }
    })();

    return () => {
      cancelled = true;
      controller.abort();
    };
  }, []);

  // AI settings (for provider/model defaults)
  const [aiSettings, setAiSettings] = useState<Record<string, unknown> | null>(null);
  useEffect(() => {
    const controller = new AbortController();
    let cancelled = false;

    (async () => {
      try {
        const resp = await fetch(`${API_BASE}/settings/ai`, { signal: controller.signal });
        const json = await resp.json();
        if (!cancelled) setAiSettings(json.data ?? null);
      } catch {
        // Runner may not be running or request was aborted
      }
    })();

    return () => {
      cancelled = true;
      controller.abort();
    };
  }, []);

  // Initialize provider/model from settings when loaded
  useEffect(() => {
    if (!aiSettings) return;
    const settings = aiSettings as Record<string, unknown>;
    if (!provider && typeof settings.provider === "string") {
      setProvider(settings.provider);
    }
    if (!model) {
      const p = provider || (settings.provider as string) || "";
      const sub = settings[p] as Record<string, unknown> | undefined;
      if (sub && typeof sub.model === "string") {
        setModel(sub.model);
      }
    }
  }, [aiSettings, provider, model]);

  // Models list changes based on selected provider
  const modelsForProvider = useMemo(() => {
    if (provider.startsWith("gemini")) return GEMINI_MODELS;
    return CLAUDE_MODELS;
  }, [provider]);

  const handleContextToggle = useCallback((contextId: string) => {
    setSelectedContextIds((prev) =>
      prev.includes(contextId) ? prev.filter((id) => id !== contextId) : [...prev, contextId],
    );
  }, []);

  const handleApplyTemplate = useCallback((template: WorkflowGenerationTemplate) => {
    setDescription(template.content);
    if (template.advancedDefaults) {
      const d = template.advancedDefaults;
      if (d.discoveryMode) setDiscoveryMode(d.discoveryMode);
      if (d.category) setCategory(d.category);
      if (d.tags) setTagsInput(d.tags);
      setShowAdvanced(true);
    }
    setShowTemplates(false);
  }, []);

  const handleSaveAsTemplate = useCallback(async () => {
    const trimmed = description.trim();
    if (!trimmed) return;
    setIsSavingTemplate(true);
    try {
      const name = trimmed.length > 60 ? trimmed.substring(0, 57) + "..." : trimmed;
      await fetch(`${API_BASE}/prompts`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name,
          content: trimmed,
          category: "Generation",
          description: "",
          tags: ["user-template"],
        }),
      });
      // Refresh prompts
      const resp = await fetch(`${API_BASE}/prompts`);
      const json = await resp.json();
      setSavedPrompts(json.data ?? []);
    } catch {
      // Failed to save
    } finally {
      setIsSavingTemplate(false);
    }
  }, [description]);

  const handleDeleteSavedTemplate = useCallback(async (id: string, e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await fetch(`${API_BASE}/prompts/${id}`, { method: "DELETE" });
      setSavedPrompts((prev) => prev.filter((p) => p.id !== id));
    } catch {
      // Failed to delete
    }
  }, []);

  const handleImportFile = async () => {
    if (!filePath.trim()) return;
    setIsImportingFile(true);
    setImportFeedback(null);
    try {
      const importResp = await fetch(`${API_BASE}/contexts/user/from-file`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ file_path: filePath.trim() }),
      });
      const importJson = await importResp.json();
      if (!importResp.ok || !importJson.success) {
        throw new Error(importJson.error || "Import failed");
      }
      setFilePath("");
      setImportFeedback({ type: "success", message: "Context imported successfully" });
      // Refresh contexts
      const resp = await fetch(`${API_BASE}/contexts`);
      const json = await resp.json();
      setSavedContexts(json.data ?? []);
      // Auto-dismiss success after 3s
      setTimeout(() => setImportFeedback(null), 3000);
    } catch (err) {
      const msg = err instanceof Error ? err.message : "Failed to import file";
      setImportFeedback({ type: "error", message: msg });
    } finally {
      setIsImportingFile(false);
    }
  };

  // Batch mode: multiple pages selected
  const isBatchMode = specState.selectedPageUrls.size > 1;
  const batchPageCount = specState.selectedPageUrls.size;

  /** Build the base request (everything except description). */
  const buildBaseRequest = useCallback(() => {
    const tags = tagsInput
      .split(",")
      .map((t) => t.trim())
      .filter(Boolean);

    if (hasSpecs && !tags.includes("spec-generated")) {
      tags.push("spec-generated");
    }

    const request: Record<string, unknown> = {};
    if (category.trim()) request.category = category.trim();
    if (tags.length > 0) request.tags = tags;
    if (selectedContextIds.length > 0) request.context_ids = selectedContextIds;
    if (inlineContext.trim()) request.inline_context = inlineContext.trim();
    if (maxIterations) request.max_iterations = parseInt(maxIterations, 10);
    if (provider.trim()) request.provider = provider.trim();
    if (model.trim()) request.model = model.trim();
    if (maxFixIterations) request.max_fix_iterations = parseInt(maxFixIterations, 10);
    request.auto_include_contexts = autoIncludeContexts;
    if (discoveryMode !== "auto") request.discovery_mode = discoveryMode;

    return request;
  }, [
    tagsInput,
    category,
    selectedContextIds,
    inlineContext,
    maxIterations,
    provider,
    model,
    maxFixIterations,
    autoIncludeContexts,
    discoveryMode,
    hasSpecs,
  ]);

  /** Build a single request (non-batch or fallback). */
  const buildGenerateRequest = useCallback(() => {
    const base = buildBaseRequest();

    let fullDescription = "";
    if (specState.discoveredSpecs.length > 0 && specState.selectedGroupIds.size > 0) {
      const specResult = buildSemanticSpecPrompt({
        discoveredSpecs: specState.discoveredSpecs,
        selectedGroupIds: specState.selectedGroupIds,
      });
      fullDescription = specResult.prompt;
      if (description.trim()) {
        fullDescription += `\n\n## Additional Instructions\n${description.trim()}`;
      }
    } else {
      fullDescription = description.trim();
    }

    return { ...base, description: fullDescription };
  }, [buildBaseRequest, description, specState]);

  /** Build one request per selected page (batch mode). */
  const buildBatchRequests = useCallback(() => {
    const base = buildBaseRequest();
    const requests: Record<string, unknown>[] = [];

    for (const pageUrl of specState.selectedPageUrls) {
      // Filter specs belonging to this page
      const pageSpecs = specState.discoveredSpecs.filter(
        (s) => (s.config.metadata?.pageUrl || s.specId) === pageUrl,
      );
      if (pageSpecs.length === 0) continue;

      // Filter selectedGroupIds to only groups in this page's specs
      const pageGroupIds = new Set<string>();
      for (const spec of pageSpecs) {
        for (const group of spec.config.groups) {
          if (specState.selectedGroupIds.has(group.id)) {
            pageGroupIds.add(group.id);
          }
        }
      }
      if (pageGroupIds.size === 0) continue;

      const specResult = buildSemanticSpecPrompt({
        discoveredSpecs: pageSpecs,
        selectedGroupIds: pageGroupIds,
      });

      let fullDescription = specResult.prompt;
      if (description.trim()) {
        fullDescription += `\n\n## Additional Instructions\n${description.trim()}`;
      }

      requests.push({ ...base, description: fullDescription });
    }

    return requests;
  }, [buildBaseRequest, description, specState]);

  const canGenerate = description.trim() || hasSpecs;

  /** Fire a single generate-async request and return the task_run_id. */
  const fireGenerateRequest = async (request: Record<string, unknown>): Promise<string> => {
    const resp = await fetch(`${API_BASE}/unified-workflows/generate-async`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(request),
    });
    const json = await resp.json();
    if (!resp.ok) {
      throw new Error(json.error || `HTTP ${resp.status}`);
    }
    const data = json.data ?? json;
    return data.task_run_id as string;
  };

  const handleGenerate = async () => {
    if (!canGenerate) return;
    setSubmittingAction("generate");
    setError(null);
    try {
      if (isBatchMode) {
        const requests = buildBatchRequests();
        const results = await Promise.all(requests.map(fireGenerateRequest));
        console.log(`[AiGeneratePanel] Batch generation started: ${results.length} pages`);
      } else {
        const taskRunId = await fireGenerateRequest(buildGenerateRequest());
        console.log("[AiGeneratePanel] Generation started:", taskRunId);
      }
      if (description.trim()) {
        autoSaveGenerationPrompt(description); // fire-and-forget
      }
      onNavigateToActiveRuns();
    } catch (err) {
      const msg = err instanceof Error ? err.message : "Failed to start workflow generation";
      setError(msg);
      console.error("[AiGeneratePanel] Generation failed:", err);
    } finally {
      setSubmittingAction(null);
    }
  };

  const handleGenerateAndRun = async () => {
    if (!canGenerate) return;
    setSubmittingAction("generate-and-run");
    setError(null);
    try {
      if (isBatchMode) {
        const requests = buildBatchRequests();
        const results = await Promise.all(requests.map(fireGenerateRequest));
        // Signal auto-run for the first workflow; others are generate-only
        if (results.length > 0) {
          localStorage.setItem(
            AUTO_RUN_AFTER_GENERATE_KEY,
            JSON.stringify({
              taskRunId: results[0],
              timestamp: Date.now(),
            } satisfies AutoRunAfterGenerate),
          );
        }
        console.log(`[AiGeneratePanel] Batch Generate & Run started: ${results.length} pages`);
      } else {
        const taskRunId = await fireGenerateRequest(buildGenerateRequest());
        localStorage.setItem(
          AUTO_RUN_AFTER_GENERATE_KEY,
          JSON.stringify({
            taskRunId,
            timestamp: Date.now(),
          } satisfies AutoRunAfterGenerate),
        );
        console.log("[AiGeneratePanel] Generate & Run started:", taskRunId);
      }
      if (description.trim()) {
        autoSaveGenerationPrompt(description); // fire-and-forget
      }
      onNavigateToActiveRuns();
    } catch (err) {
      const msg = err instanceof Error ? err.message : "Failed to start workflow generation";
      setError(msg);
      console.error("[AiGeneratePanel] Generate & Run failed:", err);
    } finally {
      setSubmittingAction(null);
    }
  };

  // Group contexts by scope
  const contextsByScope = savedContexts.reduce(
    (acc, ctx) => {
      const scope = ctx.scope || "user";
      if (!acc[scope]) acc[scope] = [];
      acc[scope].push(ctx);
      return acc;
    },
    {} as Record<string, SavedContext[]>,
  );

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-2.5 border-b border-zinc-800 bg-zinc-900/50 shrink-0">
        <div className="flex items-center gap-2">
          <Sparkles className="w-4 h-4 text-amber-400" />
          <h2 className="text-sm font-semibold text-zinc-200">Generate Workflow with AI</h2>
        </div>
        <button
          onClick={onCreateManually}
          disabled={submittingAction !== null}
          className="flex items-center gap-1 h-7 px-2 text-xs text-zinc-400 hover:text-zinc-200 rounded hover:bg-zinc-800 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
        >
          <Plus className="w-3 h-3" />
          Create Manually
        </button>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto">
        <div className="max-w-3xl mx-auto px-6 py-6 space-y-6">
          {/* Description */}
          <div className="space-y-2">
            <div className="flex items-center justify-between">
              <label className="text-sm text-zinc-300">What should the workflow do?</label>
              {/* Template Picker */}
              <div className="relative" ref={templatePopoverRef}>
                <button
                  onClick={() => setShowTemplates(!showTemplates)}
                  className="flex items-center gap-1 h-6 px-2 text-xs text-zinc-400 hover:text-zinc-200 rounded hover:bg-zinc-800 transition-colors"
                >
                  <Layers className="w-3 h-3" />
                  Templates
                </button>
                {showTemplates && (
                  <div className="absolute right-0 top-full mt-1 z-50 w-80 bg-zinc-800 border border-zinc-700 rounded-lg shadow-xl overflow-hidden">
                    <div className="max-h-[400px] overflow-y-auto">
                      {/* Built-in templates */}
                      <div className="px-3 py-1.5 text-[10px] font-semibold uppercase tracking-wider text-zinc-500 bg-zinc-900/50 border-b border-zinc-700">
                        Built-in
                      </div>
                      {GENERATION_TEMPLATES.map((template) => {
                        const IconComponent = TEMPLATE_ICONS[template.icon] || Layers;
                        return (
                          <button
                            key={template.id}
                            className="w-full text-left px-3 py-2 text-sm hover:bg-zinc-700/50 border-b border-zinc-700/50 last:border-0"
                            onClick={() => handleApplyTemplate(template)}
                          >
                            <div className="flex items-center gap-1.5 font-medium text-xs text-zinc-200">
                              <IconComponent className="w-3 h-3 text-zinc-400 shrink-0" />
                              {template.name}
                            </div>
                            <div className="text-xs text-zinc-500 mt-0.5 line-clamp-2">
                              {template.description}
                            </div>
                          </button>
                        );
                      })}

                      {/* Saved templates */}
                      {generationPrompts.length > 0 && (
                        <>
                          <div className="px-3 py-1.5 text-[10px] font-semibold uppercase tracking-wider text-zinc-500 bg-zinc-900/50 border-b border-zinc-700">
                            My Templates ({generationPrompts.length})
                          </div>
                          {generationPrompts.map((prompt) => (
                            <button
                              key={prompt.id}
                              className="w-full text-left px-3 py-2 text-sm hover:bg-zinc-700/50 border-b border-zinc-700/50 last:border-0 group"
                              onClick={() => {
                                setDescription(prompt.content);
                                setShowTemplates(false);
                              }}
                            >
                              <div className="flex items-center justify-between gap-2">
                                <div className="font-medium text-xs text-zinc-200 truncate min-w-0">
                                  {prompt.name}
                                </div>
                                <button
                                  className="shrink-0 p-0.5 rounded hover:bg-red-500/20 text-zinc-500 hover:text-red-400 opacity-0 group-hover:opacity-100 transition-opacity"
                                  onClick={(e) => handleDeleteSavedTemplate(prompt.id, e)}
                                  title="Delete template"
                                >
                                  <X className="w-3 h-3" />
                                </button>
                              </div>
                              <div className="text-xs text-zinc-500 mt-0.5 line-clamp-2">
                                {prompt.content.substring(0, 120)}
                                {prompt.content.length > 120 && "..."}
                              </div>
                            </button>
                          ))}
                        </>
                      )}

                      {/* Save current as template */}
                      <div className="border-t border-zinc-700">
                        <button
                          className="w-full text-left px-3 py-2 text-xs hover:bg-zinc-700/50 disabled:opacity-40 disabled:cursor-not-allowed flex items-center gap-1.5 text-zinc-400 hover:text-zinc-200"
                          disabled={!description.trim() || isSavingTemplate}
                          onClick={handleSaveAsTemplate}
                        >
                          {isSavingTemplate ? (
                            <Loader2 className="w-3 h-3 animate-spin shrink-0" />
                          ) : (
                            <Save className="w-3 h-3 shrink-0" />
                          )}
                          Save Current as Template
                        </button>
                      </div>
                    </div>
                  </div>
                )}
              </div>
            </div>
            <textarea
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 text-sm min-h-[120px] resize-none focus:outline-none focus:ring-2 focus:ring-blue-500/50"
              placeholder={
                hasSpecs
                  ? "Optional: add additional instructions for the AI..."
                  : "e.g., Run TypeScript type checking on the web frontend and fix any errors\ne.g., Check the runner API health, then verify UI Bridge elements are registered\ne.g., Run pytest with coverage and fix failing tests"
              }
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              autoFocus
            />
          </div>

          {/* Page Specs Section */}
          <SpecSourceSection onSpecsChanged={setSpecState} />

          {/* Context Section */}
          <div className="space-y-1">
            <button
              type="button"
              onClick={() => setShowContext(!showContext)}
              className="flex items-center gap-2 text-sm text-zinc-400 hover:text-zinc-200 transition-colors"
            >
              {showContext ? (
                <ChevronDown className="w-4 h-4" />
              ) : (
                <ChevronRight className="w-4 h-4" />
              )}
              <FileText className="w-4 h-4" />
              Attach Context
              {selectedContextIds.length > 0 && (
                <span className="text-xs ml-1 px-1.5 py-0.5 rounded bg-zinc-700 text-zinc-300">
                  {selectedContextIds.length}
                </span>
              )}
            </button>
            {showContext && (
              <div className="mt-3 space-y-3">
                {/* Tab buttons */}
                <div className="flex gap-1 p-0.5 bg-zinc-800 rounded-md border border-zinc-700 w-fit">
                  {(["saved", "custom", "file"] as const).map((tab) => (
                    <button
                      key={tab}
                      onClick={() => setContextTab(tab)}
                      className={`px-3 py-1 text-xs rounded transition-colors ${
                        contextTab === tab
                          ? "bg-zinc-700 text-zinc-200"
                          : "text-zinc-400 hover:text-zinc-200"
                      }`}
                    >
                      {tab === "saved"
                        ? "Saved Contexts"
                        : tab === "custom"
                          ? "Custom Text"
                          : "Import File"}
                    </button>
                  ))}
                </div>

                {contextTab === "saved" && (
                  <>
                    {savedContexts.length === 0 ? (
                      <p className="text-xs text-zinc-500 py-2">
                        No saved contexts. Create one in the Contexts tab or import a file.
                      </p>
                    ) : (
                      <div className="max-h-[240px] overflow-y-auto space-y-1 pr-1">
                        {Object.entries(contextsByScope).map(([scope, contexts]) => (
                          <div key={scope}>
                            <p className="text-xs text-zinc-500 uppercase tracking-wider mb-1">
                              {scope}
                            </p>
                            {contexts.map((ctx) => (
                              <label
                                key={ctx.id}
                                className="flex items-start gap-2 p-1.5 rounded hover:bg-zinc-800/50 cursor-pointer"
                              >
                                <input
                                  type="checkbox"
                                  checked={selectedContextIds.includes(ctx.id)}
                                  onChange={() => handleContextToggle(ctx.id)}
                                  className="mt-0.5 w-4 h-4 rounded border-zinc-600 bg-zinc-800"
                                />
                                <div className="min-w-0">
                                  <span className="text-sm text-zinc-300 block truncate">
                                    {ctx.name}
                                  </span>
                                  {ctx.category && (
                                    <span className="text-[10px] px-1.5 py-0.5 rounded border border-zinc-600 text-zinc-400 mt-0.5 inline-block">
                                      {ctx.category}
                                    </span>
                                  )}
                                </div>
                              </label>
                            ))}
                          </div>
                        ))}
                      </div>
                    )}
                  </>
                )}

                {contextTab === "custom" && (
                  <textarea
                    className="w-full min-h-[100px] px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 text-xs font-mono resize-none focus:outline-none focus:ring-2 focus:ring-blue-500/50"
                    placeholder="Paste additional context here (e.g., CLAUDE.md content, project notes, API docs)..."
                    value={inlineContext}
                    onChange={(e) => setInlineContext(e.target.value)}
                  />
                )}

                {contextTab === "file" && (
                  <div className="space-y-2">
                    <p className="text-xs text-zinc-500">
                      Import a file (e.g., CLAUDE.md, GEMINI.md) as a saved context for reuse.
                    </p>
                    <div className="flex gap-2">
                      <input
                        className="flex-1 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500/50"
                        placeholder="C:\path\to\CLAUDE.md"
                        value={filePath}
                        onChange={(e) => setFilePath(e.target.value)}
                        disabled={isImportingFile}
                      />
                      <button
                        onClick={handleImportFile}
                        disabled={!filePath.trim() || isImportingFile}
                        className="flex items-center gap-1 px-3 py-2 text-sm rounded border border-zinc-600 bg-zinc-700/50 hover:bg-zinc-700 text-zinc-300 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                      >
                        {isImportingFile ? (
                          <Loader2 className="w-4 h-4 animate-spin" />
                        ) : (
                          <FolderOpen className="w-4 h-4" />
                        )}
                        Import
                      </button>
                    </div>
                    {importFeedback && (
                      <div
                        className={`flex items-center gap-2 p-2 rounded text-xs ${
                          importFeedback.type === "success"
                            ? "bg-green-500/10 border border-green-500/30 text-green-400"
                            : "bg-red-500/10 border border-red-500/30 text-red-400"
                        }`}
                      >
                        {importFeedback.type === "success" ? (
                          <CheckCircle2 className="w-3.5 h-3.5 shrink-0" />
                        ) : (
                          <AlertCircle className="w-3.5 h-3.5 shrink-0" />
                        )}
                        {importFeedback.message}
                      </div>
                    )}
                  </div>
                )}
              </div>
            )}
          </div>

          {/* Advanced Options */}
          <div className="space-y-1">
            <button
              type="button"
              onClick={() => setShowAdvanced(!showAdvanced)}
              className="flex items-center gap-2 text-sm text-zinc-400 hover:text-zinc-200 transition-colors"
            >
              {showAdvanced ? (
                <ChevronDown className="w-4 h-4" />
              ) : (
                <ChevronRight className="w-4 h-4" />
              )}
              Advanced Options
            </button>
            {showAdvanced && (
              <div className="mt-3 space-y-3 pl-1">
                <div className="grid grid-cols-2 gap-3">
                  <div className="space-y-1">
                    <label className="text-xs text-zinc-400">Category</label>
                    <input
                      className="w-full px-3 py-1.5 bg-zinc-800 border border-zinc-700 text-zinc-200 text-sm h-8 rounded-md focus:outline-none focus:ring-2 focus:ring-blue-500/50"
                      placeholder="e.g., testing, deployment"
                      value={category}
                      onChange={(e) => setCategory(e.target.value)}
                    />
                  </div>
                  <div className="space-y-1">
                    <label className="text-xs text-zinc-400">Tags (comma-separated)</label>
                    <input
                      className="w-full px-3 py-1.5 bg-zinc-800 border border-zinc-700 text-zinc-200 text-sm h-8 rounded-md focus:outline-none focus:ring-2 focus:ring-blue-500/50"
                      placeholder="e.g., python, lint"
                      value={tagsInput}
                      onChange={(e) => setTagsInput(e.target.value)}
                    />
                  </div>
                  <div className="space-y-1">
                    <label className="text-xs text-zinc-400">Max Iterations</label>
                    <input
                      type="number"
                      className="w-full px-3 py-1.5 bg-zinc-800 border border-zinc-700 text-zinc-200 text-sm h-8 rounded-md focus:outline-none focus:ring-2 focus:ring-blue-500/50"
                      placeholder="10"
                      value={maxIterations}
                      onChange={(e) => setMaxIterations(e.target.value)}
                    />
                  </div>
                  <div className="space-y-1">
                    <label className="text-xs text-zinc-400">AI Provider</label>
                    <select
                      value={provider}
                      onChange={(e) => {
                        setProvider(e.target.value);
                        setModel(""); // Reset model when provider changes
                      }}
                      className="w-full px-3 py-1.5 bg-zinc-800 border border-zinc-700 text-zinc-200 text-sm h-8 rounded-md focus:outline-none focus:ring-2 focus:ring-blue-500/50"
                    >
                      <option value="">Default (from Settings)</option>
                      {PROVIDERS.map((p) => (
                        <option key={p.value} value={p.value}>
                          {p.label}
                        </option>
                      ))}
                    </select>
                  </div>
                  <div className="space-y-1">
                    <label className="text-xs text-zinc-400">Model</label>
                    <select
                      value={model}
                      onChange={(e) => setModel(e.target.value)}
                      className="w-full px-3 py-1.5 bg-zinc-800 border border-zinc-700 text-zinc-200 text-sm h-8 rounded-md focus:outline-none focus:ring-2 focus:ring-blue-500/50"
                    >
                      <option value="">Default (from Settings)</option>
                      {modelsForProvider.map((m) => (
                        <option key={m.value} value={m.value}>
                          {m.label}
                        </option>
                      ))}
                    </select>
                  </div>
                  <div className="space-y-1">
                    <label className="text-xs text-zinc-400 flex items-center gap-1">
                      Verification Rounds
                      <span className="relative group">
                        <Info className="w-3 h-3 text-zinc-500 cursor-help" />
                        <span className="absolute bottom-full left-1/2 -translate-x-1/2 mb-1 hidden group-hover:block w-48 p-2 bg-zinc-700 border border-zinc-600 rounded text-[10px] text-zinc-300 z-50 shadow-lg">
                          After generating, the AI reviews the workflow for errors and fixes them.
                          Each round is one review-and-fix pass. Set to 0 to skip verification.
                        </span>
                      </span>
                    </label>
                    <input
                      type="number"
                      className="w-full px-3 py-1.5 bg-zinc-800 border border-zinc-700 text-zinc-200 text-sm h-8 rounded-md focus:outline-none focus:ring-2 focus:ring-blue-500/50"
                      placeholder="3"
                      value={maxFixIterations}
                      onChange={(e) => setMaxFixIterations(e.target.value)}
                    />
                  </div>
                  <div className="space-y-1">
                    <label className="text-xs text-zinc-400 flex items-center gap-1">
                      Discovery
                      <span className="relative group">
                        <Info className="w-3 h-3 text-zinc-500 cursor-help" />
                        <span className="absolute bottom-full left-1/2 -translate-x-1/2 mb-1 hidden group-hover:block w-52 p-2 bg-zinc-700 border border-zinc-600 rounded text-[10px] text-zinc-300 z-50 shadow-lg">
                          Pre-generation system scan. Auto = only matching keywords. Enabled = all
                          tools. Disabled = skip.
                        </span>
                      </span>
                    </label>
                    <select
                      value={discoveryMode}
                      onChange={(e) =>
                        setDiscoveryMode(e.target.value as "auto" | "enabled" | "disabled")
                      }
                      className="w-full px-3 py-1.5 bg-zinc-800 border border-zinc-700 text-zinc-200 text-sm h-8 rounded-md focus:outline-none focus:ring-2 focus:ring-blue-500/50"
                    >
                      <option value="auto">Auto</option>
                      <option value="enabled">Enabled (all tools)</option>
                      <option value="disabled">Disabled</option>
                    </select>
                    <p className="text-[11px] text-zinc-500">
                      Scans your system for context before generating.
                    </p>
                  </div>
                  <div className="flex items-end pb-1">
                    <label className="flex items-center gap-2 text-sm text-zinc-300 cursor-pointer">
                      <input
                        type="checkbox"
                        checked={autoIncludeContexts}
                        onChange={(e) => setAutoIncludeContexts(e.target.checked)}
                        className="w-4 h-4 rounded border-zinc-600 bg-zinc-800"
                      />
                      Auto-include contexts
                      <span className="relative group">
                        <Info className="w-3 h-3 text-zinc-500 cursor-help" />
                        <span className="absolute bottom-full left-1/2 -translate-x-1/2 mb-1 hidden group-hover:block w-48 p-2 bg-zinc-700 border border-zinc-600 rounded text-[10px] text-zinc-300 z-50 shadow-lg">
                          Automatically matches and includes relevant knowledge base documents based
                          on keywords in your description.
                        </span>
                      </span>
                    </label>
                  </div>
                </div>
              </div>
            )}
          </div>
        </div>
      </div>

      {/* Error Banner */}
      {error && (
        <div className="shrink-0 mx-6 mb-2">
          <div className="flex items-start gap-2 p-3 bg-red-500/10 border border-red-500/30 rounded-md">
            <AlertCircle className="w-4 h-4 text-red-400 flex-shrink-0 mt-0.5" />
            <div className="flex-1 text-sm text-red-400">{error}</div>
            <button
              onClick={() => setError(null)}
              className="text-red-400/60 hover:text-red-400 flex-shrink-0"
            >
              <X className="w-3.5 h-3.5" />
            </button>
          </div>
        </div>
      )}

      {/* Actions - fixed footer */}
      <div className="shrink-0 border-t border-zinc-800 bg-zinc-900/50 px-6 py-3">
        <div className="max-w-3xl mx-auto flex items-center gap-3">
          <button
            onClick={handleGenerate}
            disabled={!canGenerate || submittingAction !== null}
            className={`flex items-center gap-2 px-6 py-2 rounded-md font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${accentColors.bgSolid} text-white hover:opacity-90`}
          >
            {submittingAction === "generate" ? (
              <Loader2 className="w-4 h-4 animate-spin" />
            ) : (
              <Sparkles className="w-4 h-4" />
            )}
            {submittingAction === "generate"
              ? "Starting..."
              : isBatchMode
                ? `Generate (${batchPageCount} pages)`
                : "Generate"}
          </button>
          <button
            onClick={handleGenerateAndRun}
            disabled={!canGenerate || submittingAction !== null}
            className="flex items-center gap-2 px-6 py-2 rounded-md font-medium border border-zinc-600 bg-zinc-800 text-zinc-200 hover:bg-zinc-700 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {submittingAction === "generate-and-run" ? (
              <Loader2 className="w-4 h-4 animate-spin" />
            ) : (
              <Play className="w-4 h-4" />
            )}
            {submittingAction === "generate-and-run"
              ? "Starting..."
              : isBatchMode
                ? `Generate & Run (${batchPageCount} pages)`
                : "Generate & Run"}
          </button>
        </div>
      </div>
    </div>
  );
}
