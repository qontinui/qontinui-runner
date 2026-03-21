import { useState, useEffect, useCallback, useRef, useReducer } from "react";
import { instanceStorage } from "@/lib/instance-storage";
import { invoke } from "@tauri-apps/api/core";
import { BatchDeleteDialog } from "./ui/BatchDeleteDialog";
import type {
  PlaywrightScript,
  PlaywrightResult,
  ScriptViewMode,
  ScriptExecutionState,
} from "../types";
import { usePromptSnippetMention } from "./PromptSnippetSelector";
import { executeAiTask } from "../hooks";
import { getAccentColors } from "@/design-system";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { createLogger } from "@/lib/logger";
import {
  ScriptListPanel,
  ScriptEditorForm,
  EmptyState,
  DescriptionPreviewModal,
  formReducer,
  formStateToPayload,
  DEFAULT_FORM_STATE,
  DEFAULT_SCRIPT_CONTENT,
} from "./playwright-test-builder";
import type { FormState } from "./playwright-test-builder";

const logger = createLogger("PlaywrightTestBuilderTab");

type LogLevel = "info" | "warning" | "error" | "debug" | "success";

interface PlaywrightTestBuilderTabProps {
  onLog: (level: LogLevel, message: string) => void;
  editScriptId?: string | null;
  onNavigateToLibrary?: () => void;
}

const CREATE_FORM_STORAGE_KEY = "qontinui-script-create-form";

function loadPersistedCreateForm() {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return instanceStorage.getJSON<any>(CREATE_FORM_STORAGE_KEY, null);
}

function loadPersistedFormState(): Partial<FormState> {
  const saved = loadPersistedCreateForm();
  if (!saved) return {};
  return {
    name: saved.formName ?? "",
    description: saved.formDescription ?? "",
    aiInstructions: saved.formAiInstructions ?? "",
    targetUrl: saved.formTargetUrl ?? "",
    scriptContent: saved.formScriptContent ?? DEFAULT_SCRIPT_CONTENT,
    category: saved.formCategory ?? "",
    tags: saved.formTags ?? "",
    timeoutSeconds: saved.formTimeoutSeconds ?? 60,
    displayMode: saved.formDisplayMode ?? "headless",
    browser: saved.formBrowser ?? "chromium",
    workflowObjective: saved.formWorkflowObjective ?? "",
    successCriteria: saved.formSuccessCriteria ?? [],
    isWorkflowAutomation: saved.formIsWorkflowAutomation ?? false,
  };
}

function getInitialFormState(): FormState {
  const persisted = loadPersistedFormState();
  return { ...DEFAULT_FORM_STATE, ...persisted };
}

export function PlaywrightTestBuilderTab({
  onLog,
  editScriptId,
  onNavigateToLibrary: _onNavigateToLibrary,
}: PlaywrightTestBuilderTabProps) {
  const [scripts, setScripts] = useState<PlaywrightScript[]>([]);
  const [_loading, setLoading] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [_selectedCategory, _setSelectedCategory] = useState<string | null>(null);
  const [categories, setCategories] = useState<string[]>([]);

  const [isSelectionMode, setIsSelectionMode] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [showBatchDeleteDialog, setShowBatchDeleteDialog] = useState(false);
  const [isBatchDeleting, setIsBatchDeleting] = useState(false);

  const [editingScript, setEditingScript] = useState<PlaywrightScript | null>(null);
  const [isCreating, setIsCreating] = useState(() => {
    const saved = loadPersistedCreateForm();
    return saved?.isCreating ?? false;
  });

  const [viewMode, setViewMode] = useState<ScriptViewMode>(() => {
    const saved = loadPersistedCreateForm();
    return saved?.viewMode ?? "natural_language";
  });

  const [form, dispatch] = useReducer(formReducer, undefined, getInitialFormState);

  const [_executingScriptId, setExecutingScriptId] = useState<string | null>(null);
  const [_executionState, setExecutionState] = useState<ScriptExecutionState>("idle");
  const [executionResult, setExecutionResult] = useState<PlaywrightResult | null>(null);

  const [isGenerating, setIsGenerating] = useState(false);
  const [refinementPrompt, setRefinementPrompt] = useState("");

  const [_isAutoRefining, setIsAutoRefining] = useState(false);
  const [_autoRefineIteration, setAutoRefineIteration] = useState(0);
  const [autoRefineMaxIterations, setAutoRefineMaxIterations] = useState(() => {
    const saved = instanceStorage.getItem("qontinui-autorefine-max-iterations");
    return saved !== null ? parseInt(saved, 10) : 10;
  });
  const [_autoRefineLog, setAutoRefineLog] = useState<string[]>([]);
  const autoRefineAbortRef = useRef(false);

  const [_autoRefineCumulativeStats, setAutoRefineCumulativeStats] = useState({
    totalRuns: 0,
    totalPassed: 0,
    totalFailed: 0,
    totalSkipped: 0,
    totalDurationMs: 0,
  });

  const [autoRefineUserHint, setAutoRefineUserHint] = useState("");
  const [_hintQueued, setHintQueued] = useState(false);

  const [includeScreenshot, setIncludeScreenshot] = useState(() => {
    const saved = instanceStorage.getItem("qontinui-autorefine-include-screenshot");
    return saved !== null ? saved === "true" : true;
  });
  const [includeTraceData, setIncludeTraceData] = useState(() => {
    const saved = instanceStorage.getItem("qontinui-autorefine-include-trace");
    return saved !== null ? saved === "true" : true;
  });
  const [includeVideo, setIncludeVideo] = useState(() => {
    const saved = instanceStorage.getItem("qontinui-autorefine-include-video");
    return saved !== null ? saved === "true" : true;
  });
  const [includeVideoAfterIterations, setIncludeVideoAfterIterations] = useState(() => {
    const saved = instanceStorage.getItem("qontinui-autorefine-video-after-iterations");
    return saved !== null ? parseInt(saved, 10) : 3;
  });
  const [videoSettingsLoaded, setVideoSettingsLoaded] = useState(false);

  useEffect(() => {
    const loadAiSettingsDefault = async () => {
      const storedValue = instanceStorage.getItem("qontinui-autorefine-video-after-iterations");
      if (storedValue !== null) {
        setVideoSettingsLoaded(true);
        return;
      }
      try {
        const result = await invoke<{
          success: boolean;
          data?: { auto_refine_video_after_iterations?: number };
        }>("get_ai_settings");
        if (result?.success && result.data?.auto_refine_video_after_iterations !== undefined) {
          setIncludeVideoAfterIterations(result.data.auto_refine_video_after_iterations);
        }
      } catch (error) {
        console.error("Failed to load AI settings for video threshold:", error);
      }
      setVideoSettingsLoaded(true);
    };
    loadAiSettingsDefault();
  }, []);

  useEffect(() => {
    instanceStorage.setItem("qontinui-autorefine-include-screenshot", String(includeScreenshot));
  }, [includeScreenshot]);
  useEffect(() => {
    instanceStorage.setItem("qontinui-autorefine-include-trace", String(includeTraceData));
  }, [includeTraceData]);
  useEffect(() => {
    instanceStorage.setItem("qontinui-autorefine-include-video", String(includeVideo));
  }, [includeVideo]);
  useEffect(() => {
    instanceStorage.setItem("qontinui-autorefine-max-iterations", String(autoRefineMaxIterations));
  }, [autoRefineMaxIterations]);
  useEffect(() => {
    if (videoSettingsLoaded) {
      instanceStorage.setItem(
        "qontinui-autorefine-video-after-iterations",
        String(includeVideoAfterIterations),
      );
    }
  }, [includeVideoAfterIterations, videoSettingsLoaded]);

  useEffect(() => {
    if (isCreating) {
      const formState = {
        isCreating,
        viewMode,
        formName: form.name,
        formDescription: form.description,
        formAiInstructions: form.aiInstructions,
        formTargetUrl: form.targetUrl,
        formScriptContent: form.scriptContent,
        formCategory: form.category,
        formTags: form.tags,
        formTimeoutSeconds: form.timeoutSeconds,
        formDisplayMode: form.displayMode,
        formBrowser: form.browser,
        formWorkflowObjective: form.workflowObjective,
        formSuccessCriteria: form.successCriteria,
        formIsWorkflowAutomation: form.isWorkflowAutomation,
      };
      instanceStorage.setJSON(CREATE_FORM_STORAGE_KEY, formState);
    } else {
      instanceStorage.removeItem(CREATE_FORM_STORAGE_KEY);
    }
  }, [isCreating, viewMode, form]);

  const [_isRegeneratingDescription, setIsRegeneratingDescription] = useState(false);
  const [descriptionPreview, setDescriptionPreview] = useState<string | null>(null);
  const [showDescriptionPreview, setShowDescriptionPreview] = useState(false);

  const descriptionChangedByUser = useRef(false);
  const descriptionTextareaRef = useRef<HTMLTextAreaElement>(null);
  const nameChangedByUser = useRef(false);
  const aiInstructionsChangedByUser = useRef(false);

  const [lastGeneratedFromDescription, setLastGeneratedFromDescription] = useState<string | null>(
    null,
  );
  const [coverageWarnings, setCoverageWarnings] = useState<string[]>([]);
  const [isValidatingCoverage, setIsValidatingCoverage] = useState(false);

  const insertPromptSnippetAtCursor = useCallback(
    (content: string) => {
      const textarea = descriptionTextareaRef.current;
      if (!textarea) {
        dispatch({
          type: "SET_DESCRIPTION",
          value: form.description + (form.description ? "\n\n" : "") + content,
        });
        descriptionChangedByUser.current = true;
        return;
      }
      const start = textarea.selectionStart;
      const end = textarea.selectionEnd;
      const newValue = form.description.slice(0, start) + content + form.description.slice(end);
      dispatch({ type: "SET_DESCRIPTION", value: newValue });
      descriptionChangedByUser.current = true;
      setTimeout(() => {
        textarea.selectionStart = textarea.selectionEnd = start + content.length;
        textarea.focus();
      }, 0);
    },
    [form.description],
  );

  const promptSnippetMention = usePromptSnippetMention(
    descriptionTextareaRef,
    (content, triggerStart, triggerEnd) => {
      const newValue =
        form.description.slice(0, triggerStart) + content + form.description.slice(triggerEnd);
      dispatch({ type: "SET_DESCRIPTION", value: newValue });
      descriptionChangedByUser.current = true;
      setTimeout(() => {
        const textarea = descriptionTextareaRef.current;
        if (textarea) {
          textarea.selectionStart = textarea.selectionEnd = triggerStart + content.length;
          textarea.focus();
        }
      }, 0);
    },
  );

  useEffect(() => {
    if (!editingScript || !descriptionChangedByUser.current) return;
    const controller = new AbortController();
    const timeoutId = setTimeout(async () => {
      try {
        await tracedFetch(`${getApiBase()}/playwright/tests/${editingScript.id}`, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ description: form.description }),
          signal: controller.signal,
        });
      } catch (error) {
        if (!controller.signal.aborted) {
          console.error("Failed to auto-save description:", error);
        }
      }
    }, 500);
    return () => {
      clearTimeout(timeoutId);
      controller.abort();
    };
  }, [form.description, editingScript]);

  useEffect(() => {
    if (!editingScript || !nameChangedByUser.current) return;
    const controller = new AbortController();
    let cancelled = false;
    const timeoutId = setTimeout(async () => {
      try {
        const response = await tracedFetch(`${getApiBase()}/playwright/tests/${editingScript.id}`, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ name: form.name }),
          signal: controller.signal,
        });
        if (response.ok && !cancelled) {
          const scriptsResponse = await tracedFetch(`${getApiBase()}/playwright/tests`, {
            signal: controller.signal,
          });
          const result = await scriptsResponse.json();
          if (!cancelled && result.success) {
            setScripts(result.data || []);
          }
        }
      } catch (error) {
        if (!cancelled) {
          console.error("Failed to auto-save name:", error);
        }
      }
    }, 500);
    return () => {
      cancelled = true;
      clearTimeout(timeoutId);
      controller.abort();
    };
  }, [form.name, editingScript]);

  useEffect(() => {
    if (!editingScript || !aiInstructionsChangedByUser.current) return;
    const controller = new AbortController();
    const timeoutId = setTimeout(async () => {
      try {
        await tracedFetch(`${getApiBase()}/playwright/tests/${editingScript.id}`, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ ai_instructions: form.aiInstructions || null }),
          signal: controller.signal,
        });
      } catch (error) {
        if (!controller.signal.aborted) {
          console.error("Failed to auto-save AI instructions:", error);
        }
      }
    }, 500);
    return () => {
      clearTimeout(timeoutId);
      controller.abort();
    };
  }, [form.aiInstructions, editingScript]);

  const [_expandedSpecs, _setExpandedSpecs] = useState<Set<number>>(new Set());
  const [_showScreenshot, _setShowScreenshot] = useState(true);
  const [_screenshotDataUrl, setScreenshotDataUrl] = useState<string | null>(null);
  const [_screenshotLoading, setScreenshotLoading] = useState(false);
  const [_screenshotError, setScreenshotError] = useState<string | null>(null);
  const [_expandedScriptId, setExpandedScriptId] = useState<string | null>(null);

  useEffect(() => {
    loadScripts();
    loadCategories();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!editScriptId) return;
    const controller = new AbortController();
    let cancelled = false;
    const loadScriptForEditing = async () => {
      try {
        const response = await tracedFetch(`${getApiBase()}/playwright/tests/${editScriptId}`, {
          signal: controller.signal,
        });
        const result = await response.json();
        if (!cancelled && result.success && result.data) {
          const script = result.data;
          setEditingScript(script);
          setIsCreating(true);
          dispatch({ type: "LOAD_SCRIPT", script });
          setViewMode(script.description ? "natural_language" : "code");
          onLog("info", `Loaded script for editing: ${script.name}`);
        } else if (!cancelled) {
          onLog("error", `Failed to load script: ${result.error || "Script not found"}`);
        }
      } catch (error) {
        if (!cancelled) {
          onLog("error", `Failed to load script: ${error}`);
        }
      }
    };
    loadScriptForEditing();
    return () => {
      cancelled = true;
      controller.abort();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editScriptId]);

  useEffect(() => {
    const loadScreenshot = async () => {
      if (!executionResult?.screenshots || executionResult.screenshots.length === 0) {
        setScreenshotDataUrl(null);
        setScreenshotError(null);
        return;
      }
      const lastScreenshot = executionResult.screenshots[executionResult.screenshots.length - 1];
      setScreenshotLoading(true);
      setScreenshotError(null);
      try {
        const dataUrl = await invoke<string>("read_image_as_base64", { path: lastScreenshot });
        setScreenshotDataUrl(dataUrl);
      } catch (error) {
        console.error("Failed to load screenshot:", error);
        setScreenshotError(String(error));
        setScreenshotDataUrl(null);
      } finally {
        setScreenshotLoading(false);
      }
    };
    loadScreenshot();
  }, [executionResult]);

  const loadScripts = useCallback(async () => {
    setLoading(true);
    try {
      const response = await tracedFetch(`${getApiBase()}/playwright/tests`);
      const result = await response.json();
      if (result.success) {
        setScripts(result.data || []);
      } else {
        onLog("error", `Failed to load scripts: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to load scripts: ${error}`);
    } finally {
      setLoading(false);
    }
  }, [onLog]);

  const toggleSelection = useCallback((id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }, []);

  const exitSelectionMode = useCallback(() => {
    setIsSelectionMode(false);
    setSelectedIds(new Set());
  }, []);

  const deleteSelectedScripts = useCallback(async () => {
    setIsBatchDeleting(true);
    try {
      for (const id of selectedIds) {
        await tracedFetch(`${getApiBase()}/playwright/tests/${id}`, { method: "DELETE" });
      }
      exitSelectionMode();
      setShowBatchDeleteDialog(false);
      await loadScripts();
      onLog("success", `Deleted ${selectedIds.size} script(s)`);
    } catch (error) {
      onLog("error", `Failed to delete scripts: ${error}`);
    } finally {
      setIsBatchDeleting(false);
    }
  }, [selectedIds, exitSelectionMode, loadScripts, onLog]);

  const getSelectedNames = useCallback(() => {
    return scripts.filter((s) => selectedIds.has(s.id)).map((s) => s.name);
  }, [scripts, selectedIds]);

  const loadCategories = async () => {
    try {
      const response = await tracedFetch(`${getApiBase()}/playwright/tests/categories`);
      const result = await response.json();
      if (result.success) {
        setCategories(result.data || []);
      }
    } catch (error) {
      console.error("Failed to load categories:", error);
    }
  };

  const isDefaultScriptContent = useCallback(() => {
    return form.scriptContent.trim() === DEFAULT_SCRIPT_CONTENT.trim();
  }, [form.scriptContent]);

  const createScript = async (options?: {
    runTest?: boolean;
    autoRefine?: boolean;
    generatedContent?: string;
  }) => {
    if (
      (options?.runTest || options?.autoRefine) &&
      !options?.generatedContent &&
      isDefaultScriptContent()
    ) {
      if (!form.description.trim()) {
        onLog("warning", "Please enter a description to generate the script");
        return;
      }
      onLog("info", "Generating script before saving...");
      await generateScriptThenCreate(options);
      return;
    }

    const payload = formStateToPayload(form);
    if (options?.generatedContent) {
      payload.script_content = options.generatedContent;
    }

    try {
      const response = await tracedFetch(`${getApiBase()}/playwright/tests`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
      });
      const result = await response.json();
      if (result.success) {
        onLog("success", `Created script: ${form.name}`);
        await loadScripts();
        loadCategories();

        if ((options?.runTest || options?.autoRefine) && result.data) {
          const createdScript = result.data as PlaywrightScript;
          setIsCreating(false);
          setEditingScript(createdScript);
          setExpandedScriptId(createdScript.id);
          await new Promise((resolve) => setTimeout(resolve, 100));
          if (options.autoRefine) {
            runAutoRefinementLoop();
          } else if (options.runTest) {
            saveAndRunScript(createdScript.id);
          }
        } else {
          dispatch({ type: "RESET" });
          setIsCreating(false);
        }
      } else {
        onLog("error", `Failed to create script: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to create script: ${error}`);
    }
  };

  const generateScriptThenCreate = async (options: { runTest?: boolean; autoRefine?: boolean }) => {
    if (!form.description.trim()) {
      onLog("warning", "Please enter a description first");
      return;
    }

    setIsGenerating(true);
    onLog("info", "Generating Playwright script from description...");

    const aiInstructionsSection = form.aiInstructions.trim()
      ? `\n## AI Instructions (Important - Follow These)\n${form.aiInstructions}\n`
      : "";

    const prompt = `Generate a Playwright test script based on the following requirements.

## Test Name
${form.name || "Untitled Test"}

## Target URL
${form.targetUrl || "http://localhost:3000"}

## Test Description
${form.description}
${aiInstructionsSection}
## Requirements
1. Generate a complete, working Playwright test script in TypeScript
2. Use modern Playwright best practices (locators, web-first assertions)
3. Include proper error handling and meaningful test assertions
4. Add comments explaining what each section does
5. Use page.goto() with the full target URL
6. Include appropriate waits for elements

## Output Format
Return ONLY the Playwright script code, wrapped in a code block. Do not include any other text or explanation.

\`\`\`typescript
import { test, expect } from '@playwright/test';

test('${form.name || "test"}', async ({ page }) => {
  // Your generated test code here
});
\`\`\``;

    try {
      const task = await executeAiTask({
        name: "ai-analysis",
        content: prompt,
        max_sessions: 1,
        display_prompt: `Generate Playwright: ${form.name || form.description.substring(0, 50)}`,
        timeout_seconds: 300,
      });

      if (task.output_log) {
        const codeMatch = task.output_log.match(/```(?:typescript|ts)?\s*([\s\S]*?)```/);
        if (codeMatch) {
          const generatedCode = codeMatch[1].trim();
          dispatch({ type: "SET_SCRIPT_CONTENT", value: generatedCode });
          setLastGeneratedFromDescription(form.description);
          onLog("success", "Script generated successfully");
          setIsGenerating(false);
          await createScript({ ...options, generatedContent: generatedCode });
        } else {
          onLog("error", "Failed to extract code from AI response");
          setIsGenerating(false);
        }
      } else {
        onLog("error", "Failed to generate script: No output from AI task");
        setIsGenerating(false);
      }
    } catch (error) {
      onLog("error", `Failed to generate script: ${error}`);
      setIsGenerating(false);
    }
  };

  const _deleteScript = async (id: string) => {
    if (!confirm("Are you sure you want to delete this script?")) return;
    try {
      const response = await tracedFetch(`${getApiBase()}/playwright/tests/${id}`, {
        method: "DELETE",
      });
      const result = await response.json();
      if (result.success) {
        onLog("success", "Script deleted");
        loadScripts();
      } else {
        onLog("error", `Failed to delete script: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to delete script: ${error}`);
    }
  };

  const _duplicateScript = async (id: string) => {
    try {
      const response = await tracedFetch(`${getApiBase()}/playwright/tests/${id}/duplicate`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({}),
      });
      const result = await response.json();
      if (result.success) {
        onLog("success", `Duplicated script: ${result.data.name}`);
        loadScripts();
      } else {
        onLog("error", `Failed to duplicate script: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to duplicate script: ${error}`);
    }
  };

  const runScript = async (id: string, targetUrlOverride?: string) => {
    setExecutingScriptId(id);
    setExecutionState("running");
    setExecutionResult(null);
    try {
      const response = await tracedFetch(`${getApiBase()}/playwright/tests/${id}/run`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ target_url_override: targetUrlOverride || undefined }),
      });
      const result = await response.json();
      if (result.success) {
        setExecutionResult(result.data);
        setExecutionState(result.data.passed ? "completed" : "failed");
        onLog(
          result.data.passed ? "success" : "error",
          `Test ${result.data.passed ? "passed" : "failed"}: ${result.data.tests_passed} passed, ${result.data.tests_failed} failed`,
        );
        loadScripts();
      } else {
        setExecutionState("failed");
        onLog("error", `Failed to run script: ${result.error}`);
      }
    } catch (error) {
      setExecutionState("failed");
      onLog("error", `Failed to run script: ${error}`);
    } finally {
      setExecutingScriptId(null);
    }
  };

  const importScripts = async () => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = ".json";
    input.onchange = async (e) => {
      const file = (e.target as HTMLInputElement).files?.[0];
      if (!file) return;
      const text = await file.text();
      try {
        const response = await tracedFetch(`${getApiBase()}/playwright/tests/import`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ scripts_json: text }),
        });
        const result = await response.json();
        if (result.success) {
          onLog("success", `Imported ${result.data.length} scripts`);
          loadScripts();
          loadCategories();
        } else {
          onLog("error", `Failed to import: ${result.error}`);
        }
      } catch (error) {
        onLog("error", `Failed to import: ${error}`);
      }
    };
    input.click();
  };

  const exportScripts = async () => {
    try {
      const response = await tracedFetch(`${getApiBase()}/playwright/tests/export`);
      const result = await response.json();
      if (result.success) {
        const blob = new Blob([result.data], { type: "application/json" });
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        a.download = "playwright-scripts.json";
        a.click();
        URL.revokeObjectURL(url);
        onLog("success", "Exported scripts");
      } else {
        onLog("error", `Failed to export: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to export: ${error}`);
    }
  };

  const generateScript = async () => {
    if (!form.description.trim()) {
      onLog("warning", "Please enter a description first");
      return;
    }

    setIsGenerating(true);
    onLog("info", "Generating Playwright script from description...");

    const aiInstructionsSection = form.aiInstructions.trim()
      ? `\n## AI Instructions (Important - Follow These)\n${form.aiInstructions}\n`
      : "";

    const prompt = `Generate a Playwright test script based on the following requirements.

## Test Name
${form.name || "Untitled Test"}

## Target URL
${form.targetUrl || "http://localhost:3000"}

## Test Description
${form.description}
${aiInstructionsSection}
## Requirements
1. Generate a complete, working Playwright test script in TypeScript
2. Use modern Playwright best practices (locators, web-first assertions)
3. Include proper error handling and meaningful test assertions
4. Add comments explaining what each section does
5. Use page.goto() with the full target URL
6. Include appropriate waits for elements

## Output Format
Return ONLY the Playwright script code, wrapped in a code block. Do not include any other text or explanation.

\`\`\`typescript
import { test, expect } from '@playwright/test';

test('${form.name || "test"}', async ({ page }) => {
  // Your generated test code here
});
\`\`\``;

    try {
      const task = await executeAiTask({
        name: "ai-analysis",
        content: prompt,
        max_sessions: 1,
        display_prompt: `Generate Playwright: ${form.name || form.description.substring(0, 50)}`,
        timeout_seconds: 120,
      });

      if (task.output_log) {
        const generatedCode = extractPlaywrightCode(task.output_log);
        if (generatedCode) {
          dispatch({ type: "SET_SCRIPT_CONTENT", value: generatedCode });
          setLastGeneratedFromDescription(form.description);
          setViewMode("code");
          onLog("success", "Script generated successfully!");
          setCoverageWarnings([]);
          const warnings = await validateCoverage(generatedCode, form.description);
          setCoverageWarnings(warnings);
        } else {
          onLog(
            "warning",
            "AI response didn't contain valid Playwright code. Check AI Output tab.",
          );
        }
      } else {
        onLog("error", "Failed to generate script: No output from AI task");
      }
    } catch (error) {
      onLog("error", `Failed to generate script: ${error}`);
    } finally {
      setIsGenerating(false);
    }
  };

  const extractPlaywrightCode = (output: string): string | null => {
    const typedCodeBlockMatch = output.match(/```(?:typescript|ts|javascript|js)\s*([\s\S]*?)```/);
    if (typedCodeBlockMatch && typedCodeBlockMatch[1]) {
      return typedCodeBlockMatch[1].trim();
    }

    const allCodeBlocks = [...output.matchAll(/```(?:\w*)?\s*([\s\S]*?)```/g)];
    for (const match of allCodeBlocks) {
      const content = match[1]?.trim();
      if (content && (content.includes("import { test, expect }") || content.includes("test("))) {
        return content;
      }
    }

    if (output.includes("import { test, expect }")) {
      const importIndex = output.indexOf("import { test, expect }");
      const endIndex = output.indexOf("```", importIndex);
      if (endIndex > importIndex) {
        return output.substring(importIndex, endIndex).trim();
      } else {
        let code = output.substring(importIndex).trim();
        const lastClosing = code.lastIndexOf("});");
        if (lastClosing > 0) {
          code = code.substring(0, lastClosing + 3);
        }
        return code;
      }
    }

    if (output.includes("test(")) {
      const testIndex = output.indexOf("test(");
      const candidate = output.substring(testIndex).trim();
      if (candidate.includes("test(")) return candidate;
    }

    return null;
  };

  const validateCoverage = async (code: string, description: string): Promise<string[]> => {
    if (!description.trim() || !code.trim()) return [];

    setIsValidatingCoverage(true);
    onLog("info", "Validating script covers all requirements...");

    const prompt = `Analyze this Playwright test script and verify it implements ALL functionality from the description.

## Test Description (What the script SHOULD do)
${description}

## Generated Script Code (What the script ACTUALLY does)
\`\`\`typescript
${code}
\`\`\`

## Task
Compare the description with the code and identify ANY missing functionality.

## Output Format
If the script implements ALL requirements from the description, respond with exactly:
COVERAGE: COMPLETE

If there are missing requirements, respond with:
COVERAGE: INCOMPLETE
MISSING:
- [First missing functionality]
- [Second missing functionality]
- [etc.]

Be thorough - check each action, assertion, and behavior mentioned in the description.`;

    try {
      const task = await executeAiTask({
        name: "ai-analysis",
        content: prompt,
        max_sessions: 1,
        display_prompt: "Validating script coverage",
        timeout_seconds: 60,
      });

      if (task.output_log) {
        const output = task.output_log;
        if (output.includes("COVERAGE: COMPLETE")) {
          onLog("success", "Script implements all requirements from description!");
          return [];
        }
        const missingMatch = output.match(/MISSING:\s*([\s\S]*?)(?:$|(?=\n\n))/i);
        if (missingMatch) {
          const missingItems = missingMatch[1]
            .split("\n")
            .map((line: string) => line.replace(/^[-*\u2022]\s*/, "").trim())
            .filter((line: string) => line.length > 0);
          if (missingItems.length > 0) {
            onLog(
              "warning",
              `Found ${missingItems.length} missing requirement(s) in generated code`,
            );
            return missingItems;
          }
        }
        if (output.includes("COVERAGE: INCOMPLETE")) {
          onLog("warning", "Script may be missing some requirements - check the description");
          return ["Some requirements from the description may not be implemented"];
        }
      }
    } catch (error) {
      onLog("error", `Coverage validation failed: ${error}`);
    } finally {
      setIsValidatingCoverage(false);
    }
    return [];
  };

  const _refineScript = async () => {
    if (!refinementPrompt.trim() && !executionResult?.error) {
      onLog("warning", "Please describe how to improve the script");
      return;
    }

    setIsGenerating(true);
    onLog("info", "Refining Playwright script based on feedback...");

    const testErrors =
      executionResult?.structured_output?.specs
        ?.filter((spec) => spec.status !== "expected" && spec.error)
        .map((spec) => `Test "${spec.title}" failed:\n${spec.error}`)
        .join("\n\n") ||
      executionResult?.error ||
      "";

    const pageSnapshot = executionResult?.structured_output?.page_snapshot || "";
    const pageSnapshotSection = pageSnapshot
      ? `\n## Page Snapshot (YAML showing all elements on the page)\nThis shows the actual elements available on the page. Use this to fix selectors.\n\`\`\`yaml\n${pageSnapshot}\n\`\`\`\n`
      : "";

    const aiInstructionsSection = form.aiInstructions.trim()
      ? `\n## AI Instructions (Important - Follow These)\n${form.aiInstructions}\n`
      : "";

    const prompt = `Refine this Playwright test script based on the test results and user feedback.

## Current Script
\`\`\`typescript
${form.scriptContent}
\`\`\`

## Test Results
${executionResult?.passed ? "All tests passed" : `Tests failed with the following errors:\n${testErrors}`}
${pageSnapshotSection}
## User Feedback
${refinementPrompt || "Fix the failing tests based on the error messages above."}
${aiInstructionsSection}
## Requirements
1. Fix the issues identified in the test results
2. Keep the overall test structure and intent
3. Use the Page Snapshot above to find the correct element roles and names
4. Use more robust selectors if elements weren't found (check if 'link' should be 'button', etc.)
5. Add appropriate waits if there were timing issues
6. Update assertions to match actual behavior if needed

## Output Format
Return ONLY the corrected Playwright script code, wrapped in a code block. Do not include any explanation.

\`\`\`typescript
import { test, expect } from '@playwright/test';
// Your corrected code here
\`\`\``;

    try {
      const task = await executeAiTask({
        name: "ai-analysis",
        content: prompt,
        max_sessions: 1,
        display_prompt: `Refine Playwright: ${form.name || "script"}`,
        timeout_seconds: 120,
      });

      if (task.output_log) {
        const generatedCode = extractPlaywrightCode(task.output_log);
        if (generatedCode) {
          dispatch({ type: "SET_SCRIPT_CONTENT", value: generatedCode });
          setViewMode("code");
          setRefinementPrompt("");
          onLog("success", "Script refined successfully! Run the test again to verify.");
        } else {
          onLog("warning", "AI response didn't contain valid code. Check AI Output tab.");
        }
      } else {
        onLog("error", "Failed to refine script: No output from AI task");
      }
    } catch (error) {
      onLog("error", `Failed to refine script: ${error}`);
    } finally {
      setIsGenerating(false);
    }
  };

  const hasDescriptionChangedSinceGeneration = () => {
    return (
      lastGeneratedFromDescription !== null &&
      form.description.trim() !== lastGeneratedFromDescription.trim()
    );
  };

  const verifyWorkflowObjective = async (
    testResult: PlaywrightResult,
    objective: string,
    successCriteria: string[],
  ): Promise<{ verified: boolean; notes: string }> => {
    const prompt = `You are verifying whether a workflow automation objective was achieved.

IMPORTANT: The Playwright script has PASSED. This is expected - it just means the automation steps ran.
Now you must verify whether the ACTUAL OBJECTIVE was achieved.

## Workflow Objective
${objective}

## Success Criteria to Verify
${successCriteria.length > 0 ? successCriteria.map((c, i) => `${i + 1}. ${c}`).join("\n") : "No specific criteria - verify the objective was met based on available evidence."}

## Available Evidence

### Page Snapshot (Current DOM State)
\`\`\`yaml
${testResult.structured_output?.page_snapshot || "Not available"}
\`\`\`

### Console Output
${testResult.structured_output?.console_output?.join("\n") || "None"}

### Screenshots Available
${testResult.screenshots?.length || 0} screenshot(s) captured

## Your Task
Analyze the page snapshot and any available evidence to determine:
1. Was the workflow objective achieved?
2. If there are success criteria, were they all met?

You MUST make a determination. If evidence is insufficient, lean towards false (not verified).

## Response Format
Respond with ONLY a JSON object (no markdown, no explanation):
{"verified": true, "notes": "Objective achieved because..."}
OR
{"verified": false, "notes": "Objective NOT achieved because..."}`;

    try {
      const task = await executeAiTask({
        name: "ai-analysis",
        content: prompt,
        max_sessions: 1,
        display_prompt: `Verifying: ${objective.substring(0, 50)}...`,
        timeout_seconds: 60,
        image_paths: testResult.screenshots || [],
      });

      if (task.output_log) {
        const jsonMatch = task.output_log.match(/\{[\s\S]*\}/);
        if (jsonMatch) {
          const parsed = JSON.parse(jsonMatch[0]);
          return {
            verified: parsed.verified === true,
            notes: parsed.notes || "No notes provided",
          };
        }
      }
      return { verified: false, notes: "Failed to parse AI verification response" };
    } catch (error) {
      return { verified: false, notes: `Verification error: ${error}` };
    }
  };

  const runAutoRefinementLoop = async () => {
    if (!editingScript) {
      onLog("warning", "Please save the script first before running auto-refinement");
      return;
    }

    if (hasDescriptionChangedSinceGeneration()) {
      const confirmed = confirm(
        "The description has changed since the code was generated.\n\n" +
          "The test may not match your current description.\n\n" +
          "Do you want to run anyway? (Click 'Generate Script' first to update the code)",
      );
      if (!confirmed) return;
    }

    autoRefineAbortRef.current = false;
    setIsAutoRefining(true);
    setAutoRefineIteration(0);
    setAutoRefineLog([]);
    setAutoRefineCumulativeStats({
      totalRuns: 0,
      totalPassed: 0,
      totalFailed: 0,
      totalSkipped: 0,
      totalDurationMs: 0,
    });
    const log = (msg: string) => {
      setAutoRefineLog((prev) => [...prev, `[${new Date().toLocaleTimeString()}] ${msg}`]);
      onLog("info", msg);
    };

    let currentScriptContent = form.scriptContent;
    const scriptId = editingScript.id;
    let iteration = 0;

    try {
      log("Saving current script state...");
      const payload = formStateToPayload(form);
      const preSaveResponse = await tracedFetch(`${getApiBase()}/playwright/tests/${scriptId}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
      });
      const preSaveResult = await preSaveResponse.json();
      if (!preSaveResult.success) {
        log(`Failed to save script before run: ${preSaveResult.error}`);
        onLog("error", `Failed to save script before auto-refinement: ${preSaveResult.error}`);
        setIsAutoRefining(false);
        return;
      }

      while (iteration < autoRefineMaxIterations && !autoRefineAbortRef.current) {
        iteration++;
        setAutoRefineIteration(iteration);
        log(`Iteration ${iteration}/${autoRefineMaxIterations}: Running test...`);

        setExecutionState("running");
        const runResponse = await tracedFetch(`${getApiBase()}/playwright/tests/${scriptId}/run`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({}),
        });
        const runResult = await runResponse.json();

        if (!runResult.success) {
          log(`Error running test: ${runResult.error}`);
          break;
        }

        const testResult: PlaywrightResult = runResult.data;
        setExecutionResult(testResult);
        setExecutionState(testResult.passed ? "completed" : "failed");

        setAutoRefineCumulativeStats((prev) => ({
          totalRuns: prev.totalRuns + 1,
          totalPassed: prev.totalPassed + testResult.tests_passed,
          totalFailed: prev.totalFailed + testResult.tests_failed,
          totalSkipped: prev.totalSkipped + testResult.tests_skipped,
          totalDurationMs: prev.totalDurationMs + testResult.duration_ms,
        }));

        if (autoRefineAbortRef.current) {
          log("Stopped by user");
          break;
        }

        if (testResult.passed) {
          const isWorkflowAutomation =
            editingScript?.is_workflow_automation || !!editingScript?.workflow_objective;

          if (isWorkflowAutomation && editingScript?.workflow_objective) {
            log(`Script execution passed (expected). Verifying workflow objective...`);
            const verification = await verifyWorkflowObjective(
              testResult,
              editingScript.workflow_objective,
              editingScript.success_criteria || [],
            );
            if (verification.verified) {
              log(`WORKFLOW SUCCESS! Objective verified: ${verification.notes}`);
              onLog("success", `Workflow completed successfully after ${iteration} iteration(s)!`);
              break;
            } else {
              log(`Script passed but objective NOT verified: ${verification.notes}`);
              onLog("error", `Workflow objective not achieved: ${verification.notes}`);
              break;
            }
          }

          log(`SUCCESS! Test passed on iteration ${iteration}`);
          onLog("success", `Auto-refinement succeeded after ${iteration} iteration(s)!`);
          break;
        }

        const failedSpecs = testResult.structured_output?.specs?.filter(
          (spec) => spec.status !== "expected" && spec.error,
        );

        if (failedSpecs && failedSpecs.length > 0) {
          for (const spec of failedSpecs) {
            const errorLines = (spec.error || "").split("\n");
            let problemSummary = errorLines[0] || "Unknown error";
            const locatorError = spec.error?.match(/locator\('([^']+)'\)/);
            const timeoutError = spec.error?.match(/Timeout (\d+)ms exceeded/i);
            const expectError = spec.error?.match(/expect\(([^)]+)\)/);
            if (timeoutError) {
              problemSummary = `Timeout: Element not found or action took too long`;
            } else if (locatorError) {
              problemSummary = `Element not found: ${locatorError[1]}`;
            } else if (expectError) {
              problemSummary = `Assertion failed: ${problemSummary.substring(0, 100)}`;
            } else if (problemSummary.length > 100) {
              problemSummary = problemSummary.substring(0, 100) + "...";
            }
            log(`PROBLEM: ${problemSummary}`);
          }
        } else if (testResult.error) {
          log(`PROBLEM: ${testResult.error.substring(0, 100)}`);
        }

        log(`Asking AI to fix the issue...`);

        const testErrors =
          failedSpecs?.map((spec) => `Test "${spec.title}" failed:\n${spec.error}`).join("\n\n") ||
          testResult.error ||
          "";

        const pageSnapshot = testResult.structured_output?.page_snapshot || "";
        const pageSnapshotSection = pageSnapshot
          ? `\n## Page Snapshot (YAML showing all elements on the page)\nThis shows the actual elements available on the page. Use this to fix selectors.\n\`\`\`yaml\n${pageSnapshot}\n\`\`\`\n`
          : "";

        const aiInstructionsSection = form.aiInstructions.trim()
          ? `\n## AI Instructions (Important - Follow These)\n${form.aiInstructions}\n`
          : "";

        const userHintSection = autoRefineUserHint.trim()
          ? `\n## User Hint (IMPORTANT - From the user watching this iteration)\n${autoRefineUserHint}\n`
          : "";

        const descriptionSection = form.description.trim()
          ? `\n## Original Test Description (IMPORTANT - Ensure ALL requirements are met)\n${form.description}\n`
          : "";

        const refinePrompt = `Refine this Playwright test script based on the test results.

## Current Script
\`\`\`typescript
${currentScriptContent}
\`\`\`
${descriptionSection}
## Test Results
Tests failed with the following errors:
${testErrors}
${pageSnapshotSection}${aiInstructionsSection}${userHintSection}
## Requirements
1. Fix the issues identified in the test results
2. **Verify the script implements ALL functionality from the Original Test Description above**
3. Keep the overall test structure and intent
4. Use the Page Snapshot above to find the correct element roles and names
5. Use more robust selectors if elements weren't found (check if 'link' should be 'button', etc.)
6. Add appropriate waits if there were timing issues
7. Update assertions to match actual behavior if needed
8. **If any functionality from the description is missing, ADD it to the script**

## Output Format
First, write a single line starting with "CHANGES:" that briefly describes what you changed (e.g., "CHANGES: Fixed button selector from 'link' to 'button', added waitForLoadState").
Then provide the corrected Playwright script code in a code block.

CHANGES: <your brief description here>

\`\`\`typescript
import { test, expect } from '@playwright/test';
// Your corrected code here
\`\`\``;

        const imagePaths: string[] = [];
        const videoPaths: string[] = [];
        let tracePath: string | undefined;

        if (includeScreenshot && testResult.screenshots && testResult.screenshots.length > 0) {
          const lastScreenshot = testResult.screenshots[testResult.screenshots.length - 1];
          imagePaths.push(lastScreenshot);
          log(`Including screenshot: ${lastScreenshot.split(/[\\/]/).pop()}`);
        }

        if (includeTraceData && testResult.trace_path) {
          tracePath = testResult.trace_path;
          log(`Including trace data from: ${tracePath.split(/[\\/]/).pop()}`);
        }

        const shouldIncludeVideo = includeVideo && iteration >= includeVideoAfterIterations;
        if (shouldIncludeVideo && testResult.video_paths && testResult.video_paths.length > 0) {
          videoPaths.push(...testResult.video_paths);
          log(`Iteration ${iteration}: Including video for additional context`);
        }

        setIsGenerating(true);
        let aiTask;
        try {
          aiTask = await executeAiTask({
            name: "ai-analysis",
            content: refinePrompt,
            max_sessions: 1,
            display_prompt: `Auto-refine iteration ${iteration}: ${form.name || "script"}`,
            timeout_seconds: 120,
            image_paths: imagePaths,
            video_paths: videoPaths,
            trace_path: tracePath,
            max_video_frames: 3,
            max_trace_screenshots: 5,
          });
        } catch (error) {
          setIsGenerating(false);
          log(`AI refinement failed: ${error}`);
          console.error("[AUTO-REFINE] AI failed:", error);
          break;
        }
        setIsGenerating(false);

        if (autoRefineUserHint.trim()) {
          log(`[HINT SENT TO AI] "${autoRefineUserHint.trim()}"`);
          setAutoRefineUserHint("");
          setHintQueued(false);
        }

        if (autoRefineAbortRef.current) {
          log("Stopped by user");
          break;
        }

        if (!aiTask.output_log) {
          log("AI refinement failed: No output");
          console.error("[AUTO-REFINE] AI failed: No output_log");
          break;
        }

        const output = aiTask.output_log;
        logger.debug("AUTO-REFINE AI output length:", output?.length);

        const changesMatch = output.match(/CHANGES:\s*(.+?)(?:\n|$)/i);
        if (changesMatch && changesMatch[1]) {
          log(`FIX: ${changesMatch[1].trim()}`);
        }

        const generatedCode = extractPlaywrightCode(output);
        if (!generatedCode) {
          log("AI response didn't contain valid code");
          console.error("[AUTO-REFINE] Invalid code. output:", output?.substring(0, 500));
          break;
        }
        logger.debug("AUTO-REFINE Extracted valid code, length:", generatedCode.length);

        currentScriptContent = generatedCode;
        dispatch({ type: "SET_SCRIPT_CONTENT", value: generatedCode });
        log("Saving updated script...");

        const saveResponse = await tracedFetch(`${getApiBase()}/playwright/tests/${scriptId}`, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ script_content: generatedCode }),
        });
        const saveResult = await saveResponse.json();

        if (!saveResult.success) {
          log(`Failed to save script: ${saveResult.error}`);
          console.error("[AUTO-REFINE] Save failed:", saveResult);
          break;
        }
        logger.debug("AUTO-REFINE Save successful, continuing to next iteration");

        log("Re-running test with fixes...");
        await new Promise((resolve) => setTimeout(resolve, 1000));
      }

      if (iteration >= autoRefineMaxIterations) {
        log(`Reached maximum iterations (${autoRefineMaxIterations}). Test still failing.`);
        onLog(
          "warning",
          `Auto-refinement stopped after ${autoRefineMaxIterations} iterations without success`,
        );
      }

      loadScripts();
    } catch (error) {
      onLog("error", `Auto-refinement error: ${error}`);
      setAutoRefineLog((prev) => [...prev, `[${new Date().toLocaleTimeString()}] Error: ${error}`]);
    } finally {
      setIsAutoRefining(false);
      setIsGenerating(false);
      setExecutingScriptId(null);
    }
  };

  const _stopAutoRefine = async () => {
    autoRefineAbortRef.current = true;
    setIsAutoRefining(false);
    setIsGenerating(false);
    setExecutionState("idle");
    onLog("info", "Auto-refinement stopped by user");
    try {
      await tracedFetch(`${getApiBase()}/stop-ai-analysis`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
      });
    } catch (error) {
      console.error("Failed to stop AI analysis:", error);
    }
  };

  const saveAndRunScript = async (id: string) => {
    if (hasDescriptionChangedSinceGeneration()) {
      const confirmed = confirm(
        "The description has changed since the code was generated.\n\n" +
          "The test may not match your current description.\n\n" +
          "Do you want to run anyway? (Click 'Generate Script' first to update the code)",
      );
      if (!confirmed) return;
    }

    try {
      const payload = formStateToPayload(form);
      const saveResponse = await tracedFetch(`${getApiBase()}/playwright/tests/${id}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
      });
      const saveResult = await saveResponse.json();
      if (!saveResult.success) {
        onLog("error", `Failed to save before run: ${saveResult.error}`);
        return;
      }
    } catch (error) {
      onLog("error", `Failed to save before run: ${error}`);
      return;
    }
    await runScript(id);
  };

  const _regenerateDescriptionFromCode = async () => {
    if (!form.scriptContent || !form.scriptContent.includes("test(")) {
      onLog("warning", "No valid script content to analyze");
      return;
    }

    setIsRegeneratingDescription(true);
    onLog("info", "Generating description from code...");

    const prompt = `Analyze this Playwright test script and generate a clear, concise natural language description of what it does.

## Script
\`\`\`typescript
${form.scriptContent}
\`\`\`

## Requirements
- Write a description that a non-technical person could understand
- Focus on WHAT the test does, not HOW (no code details)
- Be specific about the user actions and expected outcomes
- Keep it to 2-4 sentences maximum
- Start directly with the action, don't say "This test..."

## Output Format
Return ONLY the description text, no code blocks or extra formatting.

Example: "Navigate to the dashboard, click the Create button, then select Extract Images. Click the Capture Screen button and verify the screenshot preview appears."`;

    try {
      const task = await executeAiTask({
        name: "ai-analysis",
        content: prompt,
        max_sessions: 1,
        display_prompt: `Generate description for: ${form.name || "script"}`,
        timeout_seconds: 60,
      });

      if (task.output_log) {
        const output = task.output_log.trim();
        const cleanDescription = output
          .replace(/^```[\s\S]*?```$/gm, "")
          .replace(/^\*\*.*?\*\*:?\s*/gm, "")
          .trim();
        setDescriptionPreview(cleanDescription);
        setShowDescriptionPreview(true);
        onLog("info", "Description generated - review and accept or reject");
      } else {
        onLog("error", "Failed to generate description: No output from AI task");
      }
    } catch (error) {
      onLog("error", `Failed to generate description: ${error}`);
    } finally {
      setIsRegeneratingDescription(false);
    }
  };

  const acceptDescriptionPreview = async () => {
    if (!descriptionPreview) return;
    dispatch({ type: "SET_DESCRIPTION", value: descriptionPreview });
    descriptionChangedByUser.current = true;
    onLog("success", "Description updated from code");
    if (editingScript) {
      try {
        await tracedFetch(`${getApiBase()}/playwright/tests/${editingScript.id}`, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ description: descriptionPreview }),
        });
      } catch (error) {
        console.error("Failed to save description:", error);
      }
    }
    setShowDescriptionPreview(false);
    setDescriptionPreview(null);
  };

  const rejectDescriptionPreview = () => {
    setShowDescriptionPreview(false);
    setDescriptionPreview(null);
    onLog("info", "Description update cancelled");
  };

  const _startEditing = (script: PlaywrightScript) => {
    descriptionChangedByUser.current = false;
    nameChangedByUser.current = false;
    aiInstructionsChangedByUser.current = false;
    setLastGeneratedFromDescription(null);
    setEditingScript(script);
    dispatch({ type: "LOAD_SCRIPT", script });
    setIsCreating(false);
  };

  const _startCreating = () => {
    dispatch({ type: "RESET" });
    setEditingScript(null);
    setIsCreating(true);
  };

  const _filteredScripts = scripts.filter((s) => {
    const matchesSearch =
      !searchQuery ||
      s.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      s.description.toLowerCase().includes(searchQuery.toLowerCase()) ||
      s.target_url.toLowerCase().includes(searchQuery.toLowerCase()) ||
      s.tags.some((t) => t.toLowerCase().includes(searchQuery.toLowerCase()));
    const matchesCategory = !_selectedCategory || s.category === _selectedCategory;
    return matchesSearch && matchesCategory;
  });

  useEffect(() => {
    if (!editScriptId && !isCreating && !editingScript) {
      setIsCreating(true);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const accentColors = getAccentColors("purple");

  const filteredScripts = scripts.filter((script) => {
    if (!searchQuery) return true;
    const query = searchQuery.toLowerCase();
    return (
      script.name.toLowerCase().includes(query) ||
      script.description?.toLowerCase().includes(query) ||
      script.target_url?.toLowerCase().includes(query)
    );
  });

  const selectScript = (script: PlaywrightScript) => {
    setEditingScript(script);
    setIsCreating(false);
    dispatch({ type: "LOAD_SCRIPT", script });
  };

  const startCreate = () => {
    setEditingScript(null);
    setIsCreating(true);
    dispatch({ type: "RESET" });
  };

  const handleClose = () => {
    setIsCreating(false);
    setEditingScript(null);
    dispatch({ type: "RESET" });
  };

  return (
    <div className="h-full flex">
      <ScriptListPanel
        scripts={scripts}
        filteredScripts={filteredScripts}
        loading={_loading}
        searchQuery={searchQuery}
        onSearchChange={setSearchQuery}
        editingScriptId={editingScript?.id}
        onSelectScript={selectScript}
        onStartCreate={startCreate}
        isSelectionMode={isSelectionMode}
        onToggleSelectionMode={() => setIsSelectionMode(!isSelectionMode)}
        selectedIds={selectedIds}
        onToggleSelection={toggleSelection}
        onSelectAll={() => setSelectedIds(new Set(filteredScripts.map((s) => s.id)))}
        onClearSelection={() => setSelectedIds(new Set())}
        onDeleteSelected={() => setShowBatchDeleteDialog(true)}
        onExitSelectionMode={exitSelectionMode}
        accentColor={accentColors.bgSolid}
      />

      <div className="flex-1 flex flex-col bg-card/50 overflow-hidden">
        {!editingScript && !isCreating ? (
          <EmptyState onStartCreate={startCreate} accentColors={accentColors} />
        ) : (
          <ScriptEditorForm
            form={form}
            dispatch={dispatch}
            viewMode={viewMode}
            setViewMode={setViewMode}
            isCreating={isCreating}
            editingScript={editingScript}
            isGenerating={isGenerating}
            categories={categories}
            coverageWarnings={coverageWarnings}
            isValidatingCoverage={isValidatingCoverage}
            descriptionTextareaRef={descriptionTextareaRef}
            promptSnippetMention={promptSnippetMention}
            insertPromptSnippetAtCursor={insertPromptSnippetAtCursor}
            onGenerateScript={generateScript}
            onCreateScript={createScript}
            onImportScripts={importScripts}
            onExportScripts={exportScripts}
            onClose={handleClose}
            onLog={onLog}
            setCoverageWarnings={setCoverageWarnings}
            visualContext={{
              includeScreenshot,
              setIncludeScreenshot,
              includeTraceData,
              setIncludeTraceData,
              includeVideo,
              setIncludeVideo,
              includeVideoAfterIterations,
              setIncludeVideoAfterIterations,
              autoRefineMaxIterations,
              setAutoRefineMaxIterations,
            }}
          />
        )}

        {showDescriptionPreview && descriptionPreview && (
          <DescriptionPreviewModal
            currentDescription={form.description}
            previewDescription={descriptionPreview}
            onAccept={acceptDescriptionPreview}
            onReject={rejectDescriptionPreview}
          />
        )}
      </div>

      <BatchDeleteDialog
        open={showBatchDeleteDialog}
        title="Delete Scripts"
        itemType="script"
        itemNames={getSelectedNames()}
        isDeleting={isBatchDeleting}
        onClose={() => setShowBatchDeleteDialog(false)}
        onConfirm={deleteSelectedScripts}
      />
    </div>
  );
}
