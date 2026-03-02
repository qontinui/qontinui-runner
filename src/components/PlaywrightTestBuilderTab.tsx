import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  TestTube,
  Plus,
  Play,
  Save,
  X,
  Loader2,
  Upload,
  Download,
  Code,
  FileText,
  Globe,
  CheckCircle,
  ChevronDown,
  Sparkles,
  RefreshCw,
  Info,
  ImageIcon,
  CheckSquare,
  Check,
  Search,
} from "lucide-react";
import { BatchDeleteDialog } from "./ui/BatchDeleteDialog";
import type {
  PlaywrightScript,
  PlaywrightResult,
  ScriptViewMode,
  ScriptExecutionState,
  DisplayMode,
} from "../types";
import { PromptSnippetSelector, usePromptSnippetMention } from "./PromptSnippetSelector";
import { executeAiTask } from "../hooks";
import { getAccentColors, getStatusColors } from "@/design-system";
import { BuilderToolbar, toolbarActions } from "./ui/BuilderToolbar";
import { getApiBase } from "@/lib/runner-api";

type LogLevel = "info" | "warning" | "error" | "debug" | "success";

interface PlaywrightTestBuilderTabProps {
  onLog: (level: LogLevel, message: string) => void;
  /** Optional script ID to load for editing (from Library navigation) */
  editScriptId?: string | null;
  /** Callback when user wants to go back to library */
  onNavigateToLibrary?: () => void;
}

const DEFAULT_SCRIPT_CONTENT = `import { test, expect } from '@playwright/test';

test('example test', async ({ page }) => {
  // Navigate to the target page
  await page.goto('/');

  // Add your test assertions here
  await expect(page).toHaveTitle(/./);
});
`;

export function PlaywrightTestBuilderTab({
  onLog,
  editScriptId,
  onNavigateToLibrary: _onNavigateToLibrary,
}: PlaywrightTestBuilderTabProps) {
  // State
  const [scripts, setScripts] = useState<PlaywrightScript[]>([]);
  const [_loading, setLoading] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [_selectedCategory, _setSelectedCategory] = useState<string | null>(null);
  const [categories, setCategories] = useState<string[]>([]);

  // Bulk deletion state
  const [isSelectionMode, setIsSelectionMode] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [showBatchDeleteDialog, setShowBatchDeleteDialog] = useState(false);
  const [isBatchDeleting, setIsBatchDeleting] = useState(false);

  // Storage key for persisting create form state
  const CREATE_FORM_STORAGE_KEY = "qontinui-script-create-form";

  // Load persisted create form state
  const loadPersistedCreateForm = useCallback(() => {
    try {
      const saved = localStorage.getItem(CREATE_FORM_STORAGE_KEY);
      if (saved) {
        return JSON.parse(saved);
      }
    } catch {
      // Ignore parse errors
    }
    return null;
  }, []);

  // Edit modal state
  const [editingScript, setEditingScript] = useState<PlaywrightScript | null>(null);
  const [isCreating, setIsCreating] = useState(() => {
    const saved = loadPersistedCreateForm();
    return saved?.isCreating ?? false;
  });

  // View mode toggle
  const [viewMode, setViewMode] = useState<ScriptViewMode>(() => {
    const saved = loadPersistedCreateForm();
    return saved?.viewMode ?? "natural_language";
  });

  // Form state - restore from localStorage if available
  const [formName, setFormName] = useState(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formName ?? "";
  });
  const [formDescription, setFormDescription] = useState(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formDescription ?? "";
  });
  const [formAiInstructions, setFormAiInstructions] = useState(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formAiInstructions ?? "";
  });
  const [formTargetUrl, setFormTargetUrl] = useState(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formTargetUrl ?? "";
  });
  const [formScriptContent, setFormScriptContent] = useState(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formScriptContent ?? DEFAULT_SCRIPT_CONTENT;
  });
  const [formCategory, setFormCategory] = useState(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formCategory ?? "";
  });
  const [formTags, setFormTags] = useState(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formTags ?? "";
  });
  const [formTimeoutSeconds, setFormTimeoutSeconds] = useState(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formTimeoutSeconds ?? 60;
  });
  const [formDisplayMode, setFormDisplayMode] = useState<DisplayMode>(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formDisplayMode ?? "headless";
  });
  const [formBrowser, setFormBrowser] = useState<"chromium" | "firefox" | "webkit">(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formBrowser ?? "chromium";
  });

  // Workflow automation fields
  const [formWorkflowObjective, setFormWorkflowObjective] = useState(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formWorkflowObjective ?? "";
  });
  const [formSuccessCriteria, setFormSuccessCriteria] = useState<string[]>(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formSuccessCriteria ?? [];
  });
  const [formIsWorkflowAutomation, setFormIsWorkflowAutomation] = useState(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formIsWorkflowAutomation ?? false;
  });

  // Execution state
  const [_executingScriptId, setExecutingScriptId] = useState<string | null>(null);
  const [_executionState, setExecutionState] = useState<ScriptExecutionState>("idle");
  const [executionResult, setExecutionResult] = useState<PlaywrightResult | null>(null);

  // AI generation state
  const [isGenerating, setIsGenerating] = useState(false);

  // Refinement prompt for improving scripts based on test results
  const [refinementPrompt, setRefinementPrompt] = useState("");

  // Auto-refinement loop state
  const [_isAutoRefining, setIsAutoRefining] = useState(false);
  const [_autoRefineIteration, setAutoRefineIteration] = useState(0);
  const [autoRefineMaxIterations, setAutoRefineMaxIterations] = useState(() => {
    const saved = localStorage.getItem("qontinui-autorefine-max-iterations");
    return saved !== null ? parseInt(saved, 10) : 10;
  });
  const [_autoRefineLog, setAutoRefineLog] = useState<string[]>([]);
  const autoRefineAbortRef = useRef(false);

  // Cumulative stats across all auto-refine iterations
  const [_autoRefineCumulativeStats, setAutoRefineCumulativeStats] = useState({
    totalRuns: 0,
    totalPassed: 0,
    totalFailed: 0,
    totalSkipped: 0,
    totalDurationMs: 0,
  });

  // User hint for auto-refine - gets included in next AI call
  const [autoRefineUserHint, setAutoRefineUserHint] = useState("");
  const [_hintQueued, setHintQueued] = useState(false);

  // Visual context settings for auto-refine (persisted to localStorage)
  const [includeScreenshot, setIncludeScreenshot] = useState(() => {
    const saved = localStorage.getItem("qontinui-autorefine-include-screenshot");
    return saved !== null ? saved === "true" : true; // Default: true
  });
  const [includeTraceData, setIncludeTraceData] = useState(() => {
    const saved = localStorage.getItem("qontinui-autorefine-include-trace");
    return saved !== null ? saved === "true" : true; // Default: true
  });
  // Video settings - checkbox to enable + iteration threshold
  const [includeVideo, setIncludeVideo] = useState(() => {
    const saved = localStorage.getItem("qontinui-autorefine-include-video");
    return saved !== null ? saved === "true" : true; // Default: enabled
  });
  const [includeVideoAfterIterations, setIncludeVideoAfterIterations] = useState(() => {
    const saved = localStorage.getItem("qontinui-autorefine-video-after-iterations");
    return saved !== null ? parseInt(saved, 10) : 3; // Temporary default, will be updated from AI settings
  });
  const [videoSettingsLoaded, setVideoSettingsLoaded] = useState(false);

  // Load default video threshold from AI settings on mount (only if no localStorage override)
  useEffect(() => {
    const loadAiSettingsDefault = async () => {
      // Only load from AI settings if localStorage doesn't have a value
      const localStorageValue = localStorage.getItem("qontinui-autorefine-video-after-iterations");
      if (localStorageValue !== null) {
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
        // Keep the default value
      }
      setVideoSettingsLoaded(true);
    };

    loadAiSettingsDefault();
  }, []);

  // Persist visual context settings
  useEffect(() => {
    localStorage.setItem("qontinui-autorefine-include-screenshot", String(includeScreenshot));
  }, [includeScreenshot]);
  useEffect(() => {
    localStorage.setItem("qontinui-autorefine-include-trace", String(includeTraceData));
  }, [includeTraceData]);
  useEffect(() => {
    localStorage.setItem("qontinui-autorefine-include-video", String(includeVideo));
  }, [includeVideo]);
  useEffect(() => {
    localStorage.setItem("qontinui-autorefine-max-iterations", String(autoRefineMaxIterations));
  }, [autoRefineMaxIterations]);
  // Only persist video threshold after settings have been loaded (to avoid overwriting with default)
  useEffect(() => {
    if (videoSettingsLoaded) {
      localStorage.setItem(
        "qontinui-autorefine-video-after-iterations",
        String(includeVideoAfterIterations),
      );
    }
  }, [includeVideoAfterIterations, videoSettingsLoaded]);

  // Persist create form state when in creating mode
  useEffect(() => {
    if (isCreating) {
      const formState = {
        isCreating,
        viewMode,
        formName,
        formDescription,
        formAiInstructions,
        formTargetUrl,
        formScriptContent,
        formCategory,
        formTags,
        formTimeoutSeconds,
        formDisplayMode,
        formBrowser,
        formWorkflowObjective,
        formSuccessCriteria,
        formIsWorkflowAutomation,
      };
      localStorage.setItem(CREATE_FORM_STORAGE_KEY, JSON.stringify(formState));
    } else {
      // Clear persisted state when not creating
      localStorage.removeItem(CREATE_FORM_STORAGE_KEY);
    }
  }, [
    isCreating,
    viewMode,
    formName,
    formDescription,
    formAiInstructions,
    formTargetUrl,
    formScriptContent,
    formCategory,
    formTags,
    formTimeoutSeconds,
    formDisplayMode,
    formBrowser,
    formWorkflowObjective,
    formSuccessCriteria,
    formIsWorkflowAutomation,
  ]);

  // Description regeneration state
  const [_isRegeneratingDescription, setIsRegeneratingDescription] = useState(false);
  const [descriptionPreview, setDescriptionPreview] = useState<string | null>(null);
  const [showDescriptionPreview, setShowDescriptionPreview] = useState(false);

  // Track if description was manually changed (to trigger auto-save)
  const descriptionChangedByUser = useRef(false);

  // Ref for description textarea (for prompt snippet insertion)
  const descriptionTextareaRef = useRef<HTMLTextAreaElement>(null);

  // Track if name was manually changed (to trigger auto-save)
  const nameChangedByUser = useRef(false);

  // Track if AI instructions was manually changed (to trigger auto-save)
  const aiInstructionsChangedByUser = useRef(false);

  // Track the description that was used to generate the current code
  // This is used to warn users if they try to run tests when description has changed
  const [lastGeneratedFromDescription, setLastGeneratedFromDescription] = useState<string | null>(
    null,
  );

  // Coverage validation state - warnings about missing functionality
  const [coverageWarnings, setCoverageWarnings] = useState<string[]>([]);
  const [isValidatingCoverage, setIsValidatingCoverage] = useState(false);

  // Prompt snippet insertion - insert at cursor position
  const insertPromptSnippetAtCursor = useCallback(
    (content: string) => {
      const textarea = descriptionTextareaRef.current;
      if (!textarea) {
        // Fallback: append to end
        setFormDescription((prev) => prev + (prev ? "\n\n" : "") + content);
        descriptionChangedByUser.current = true;
        return;
      }

      const start = textarea.selectionStart;
      const end = textarea.selectionEnd;
      const currentValue = formDescription;

      const newValue = currentValue.slice(0, start) + content + currentValue.slice(end);
      setFormDescription(newValue);
      descriptionChangedByUser.current = true;

      // Restore cursor position after inserted text
      setTimeout(() => {
        textarea.selectionStart = textarea.selectionEnd = start + content.length;
        textarea.focus();
      }, 0);
    },
    [formDescription],
  );

  // @-mention hook for prompt snippet insertion
  const promptSnippetMention = usePromptSnippetMention(
    descriptionTextareaRef,
    (content, triggerStart, triggerEnd) => {
      // Replace @query with the prompt snippet content
      const currentValue = formDescription;
      const newValue =
        currentValue.slice(0, triggerStart) + content + currentValue.slice(triggerEnd);
      setFormDescription(newValue);
      descriptionChangedByUser.current = true;

      // Focus back on textarea
      setTimeout(() => {
        const textarea = descriptionTextareaRef.current;
        if (textarea) {
          textarea.selectionStart = textarea.selectionEnd = triggerStart + content.length;
          textarea.focus();
        }
      }, 0);
    },
  );

  // Auto-save description when editing an existing script
  useEffect(() => {
    // Only auto-save if editing an existing script and description was changed by user
    if (!editingScript || !descriptionChangedByUser.current) {
      return;
    }

    const controller = new AbortController();
    const timeoutId = setTimeout(async () => {
      try {
        await fetch(`${getApiBase()}/playwright/tests/${editingScript.id}`, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ description: formDescription }),
          signal: controller.signal,
        });
        // Don't show a log message for auto-save to avoid noise
      } catch (error) {
        if (!controller.signal.aborted) {
          console.error("Failed to auto-save description:", error);
        }
      }
    }, 500); // 500ms debounce

    return () => {
      clearTimeout(timeoutId);
      controller.abort();
    };
  }, [formDescription, editingScript]);

  // Auto-save name when editing an existing script
  useEffect(() => {
    // Only auto-save if editing an existing script and name was changed by user
    if (!editingScript || !nameChangedByUser.current) {
      return;
    }

    const controller = new AbortController();
    let cancelled = false;
    const timeoutId = setTimeout(async () => {
      try {
        const response = await fetch(`${getApiBase()}/playwright/tests/${editingScript.id}`, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ name: formName }),
          signal: controller.signal,
        });
        if (response.ok && !cancelled) {
          // Reload scripts to update the list display
          const scriptsResponse = await fetch(`${getApiBase()}/playwright/tests`, {
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
    }, 500); // 500ms debounce

    return () => {
      cancelled = true;
      clearTimeout(timeoutId);
      controller.abort();
    };
  }, [formName, editingScript]);

  // Auto-save AI instructions when editing an existing script
  useEffect(() => {
    // Only auto-save if editing an existing script and AI instructions was changed by user
    if (!editingScript || !aiInstructionsChangedByUser.current) {
      return;
    }

    const controller = new AbortController();
    const timeoutId = setTimeout(async () => {
      try {
        await fetch(`${getApiBase()}/playwright/tests/${editingScript.id}`, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ ai_instructions: formAiInstructions || null }),
          signal: controller.signal,
        });
        // Don't show a log message for auto-save to avoid noise
      } catch (error) {
        if (!controller.signal.aborted) {
          console.error("Failed to auto-save AI instructions:", error);
        }
      }
    }, 500); // 500ms debounce

    return () => {
      clearTimeout(timeoutId);
      controller.abort();
    };
  }, [formAiInstructions, editingScript]);

  // Expanded sections in execution result
  const [_expandedSpecs, _setExpandedSpecs] = useState<Set<number>>(new Set());

  // Show screenshot in execution result
  const [_showScreenshot, _setShowScreenshot] = useState(true);
  const [_screenshotDataUrl, setScreenshotDataUrl] = useState<string | null>(null);
  const [_screenshotLoading, setScreenshotLoading] = useState(false);
  const [_screenshotError, setScreenshotError] = useState<string | null>(null);

  // Expanded script for viewing details
  const [_expandedScriptId, setExpandedScriptId] = useState<string | null>(null);

  // Load scripts on mount
  useEffect(() => {
    loadScripts();
    loadCategories();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Load script for editing when editScriptId is provided
  useEffect(() => {
    if (!editScriptId) return;

    const controller = new AbortController();
    let cancelled = false;

    const loadScriptForEditing = async () => {
      try {
        const response = await fetch(`${getApiBase()}/playwright/tests/${editScriptId}`, {
          signal: controller.signal,
        });
        const result = await response.json();
        if (!cancelled && result.success && result.data) {
          const script = result.data;
          setEditingScript(script);
          setIsCreating(true);
          setFormName(script.name || "");
          setFormDescription(script.description || "");
          setFormAiInstructions(script.ai_instructions || "");
          setFormTargetUrl(script.target_url || "");
          setFormScriptContent(script.script_content || DEFAULT_SCRIPT_CONTENT);
          setFormCategory(script.category || "");
          setFormTags(script.tags?.join(", ") || "");
          setFormTimeoutSeconds(script.timeout_seconds || 60);
          setFormDisplayMode(script.display_mode || "headless");
          setFormBrowser(script.browser || "chromium");
          setFormWorkflowObjective(script.workflow_objective || "");
          setFormSuccessCriteria(script.success_criteria || []);
          setFormIsWorkflowAutomation(script.is_workflow_automation || false);
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

  // Load screenshot when execution result changes
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
      const response = await fetch(`${getApiBase()}/playwright/tests`);
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

  // Toggle selection for bulk delete
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

  // Exit selection mode
  const exitSelectionMode = useCallback(() => {
    setIsSelectionMode(false);
    setSelectedIds(new Set());
  }, []);

  // Delete selected scripts
  const deleteSelectedScripts = useCallback(async () => {
    setIsBatchDeleting(true);
    try {
      for (const id of selectedIds) {
        await fetch(`${getApiBase()}/playwright/tests/${id}`, { method: "DELETE" });
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

  // Get names of selected scripts for dialog
  const getSelectedNames = useCallback(() => {
    return scripts.filter((s) => selectedIds.has(s.id)).map((s) => s.name);
  }, [scripts, selectedIds]);

  const loadCategories = async () => {
    try {
      const response = await fetch(`${getApiBase()}/playwright/tests/categories`);
      const result = await response.json();
      if (result.success) {
        setCategories(result.data || []);
      }
    } catch (error) {
      console.error("Failed to load categories:", error);
    }
  };

  // Check if script content is still the default (needs generation)
  const isDefaultScriptContent = useCallback(() => {
    return formScriptContent.trim() === DEFAULT_SCRIPT_CONTENT.trim();
  }, [formScriptContent]);

  // CRUD operations
  const createScript = async (options?: {
    runTest?: boolean;
    autoRefine?: boolean;
    generatedContent?: string;
  }) => {
    // If auto-refine or run test is requested and script is still default, generate first
    if (
      (options?.runTest || options?.autoRefine) &&
      !options?.generatedContent &&
      isDefaultScriptContent()
    ) {
      if (!formDescription.trim()) {
        onLog("warning", "Please enter a description to generate the script");
        return;
      }
      onLog("info", "Generating script before saving...");
      // Generate script first, then this function will be called again via generateAndCreate
      await generateScriptThenCreate(options);
      return;
    }

    // Use generated content if provided, otherwise use form state
    const scriptContent = options?.generatedContent || formScriptContent;

    try {
      const response = await fetch(`${getApiBase()}/playwright/tests`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: formName,
          description: formDescription,
          ai_instructions: formAiInstructions || undefined,
          target_url: formTargetUrl,
          script_content: scriptContent,
          category: formCategory,
          tags: formTags
            .split(",")
            .map((t) => t.trim())
            .filter(Boolean),
          timeout_seconds: formTimeoutSeconds,
          display_mode: formDisplayMode,
          browser: formBrowser,
          workflow_objective: formWorkflowObjective || undefined,
          success_criteria: formSuccessCriteria.length > 0 ? formSuccessCriteria : undefined,
          is_workflow_automation: formIsWorkflowAutomation,
        }),
      });
      const result = await response.json();
      if (result.success) {
        onLog("success", `Created script: ${formName}`);
        await loadScripts();
        loadCategories();

        // If we need to run test or auto-refine, switch to edit mode with the new script
        // but keep the form visible (don't minimize)
        if ((options?.runTest || options?.autoRefine) && result.data) {
          const createdScript = result.data as PlaywrightScript;
          // Switch from create mode to edit mode - this keeps the form open
          setIsCreating(false);
          setEditingScript(createdScript);
          setExpandedScriptId(createdScript.id); // Must set this for edit form to show
          // Form state is already set, no need to reset

          // Small delay to ensure state is updated
          await new Promise((resolve) => setTimeout(resolve, 100));

          if (options.autoRefine) {
            // Start auto-refinement loop
            runAutoRefinementLoop();
          } else if (options.runTest) {
            // Just run the test once
            saveAndRunScript(createdScript.id);
          }
        } else {
          // Only reset and close if not running/refining
          resetForm();
          setIsCreating(false);
        }
      } else {
        onLog("error", `Failed to create script: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to create script: ${error}`);
    }
  };

  // Generate script and then create with options
  const generateScriptThenCreate = async (options: { runTest?: boolean; autoRefine?: boolean }) => {
    if (!formDescription.trim()) {
      onLog("warning", "Please enter a description first");
      return;
    }

    setIsGenerating(true);
    onLog("info", "Generating Playwright script from description...");

    // Build the prompt with optional AI instructions
    const aiInstructionsSection = formAiInstructions.trim()
      ? `\n## AI Instructions (Important - Follow These)\n${formAiInstructions}\n`
      : "";

    const prompt = `Generate a Playwright test script based on the following requirements.

## Test Name
${formName || "Untitled Test"}

## Target URL
${formTargetUrl || "http://localhost:3000"}

## Test Description
${formDescription}
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

test('${formName || "test"}', async ({ page }) => {
  // Your generated test code here
});
\`\`\``;

    try {
      // Use async task execution with polling
      const task = await executeAiTask({
        name: "ai-analysis",
        content: prompt,
        max_sessions: 1,
        display_prompt: `Generate Playwright: ${formName || formDescription.substring(0, 50)}`,
        timeout_seconds: 300,
      });

      if (task.output_log) {
        // Extract code from the AI response
        const codeMatch = task.output_log.match(/```(?:typescript|ts)?\s*([\s\S]*?)```/);
        if (codeMatch) {
          const generatedCode = codeMatch[1].trim();
          setFormScriptContent(generatedCode);
          setLastGeneratedFromDescription(formDescription);
          onLog("success", "Script generated successfully");

          // Now create the script with the generated content
          setIsGenerating(false);
          // Call createScript again - pass the generated content directly to avoid state timing issues
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
      const response = await fetch(`${getApiBase()}/playwright/tests/${id}`, {
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
      const response = await fetch(`${getApiBase()}/playwright/tests/${id}/duplicate`, {
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
      const response = await fetch(`${getApiBase()}/playwright/tests/${id}/run`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          target_url_override: targetUrlOverride || undefined,
        }),
      });
      const result = await response.json();
      if (result.success) {
        setExecutionResult(result.data);
        setExecutionState(result.data.passed ? "completed" : "failed");
        onLog(
          result.data.passed ? "success" : "error",
          `Test ${result.data.passed ? "passed" : "failed"}: ${result.data.tests_passed} passed, ${result.data.tests_failed} failed`,
        );
        // Reload to get updated last_result
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
        const response = await fetch(`${getApiBase()}/playwright/tests/import`, {
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
      const response = await fetch(`${getApiBase()}/playwright/tests/export`);
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

  // AI script generation
  const generateScript = async () => {
    if (!formDescription.trim()) {
      onLog("warning", "Please enter a description first");
      return;
    }

    setIsGenerating(true);
    onLog("info", "Generating Playwright script from description...");

    // Build the prompt with optional AI instructions
    const aiInstructionsSection = formAiInstructions.trim()
      ? `\n## AI Instructions (Important - Follow These)\n${formAiInstructions}\n`
      : "";

    const prompt = `Generate a Playwright test script based on the following requirements.

## Test Name
${formName || "Untitled Test"}

## Target URL
${formTargetUrl || "http://localhost:3000"}

## Test Description
${formDescription}
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

test('${formName || "test"}', async ({ page }) => {
  // Your generated test code here
});
\`\`\``;

    try {
      // Use async task execution with polling
      const task = await executeAiTask({
        name: "ai-analysis",
        content: prompt,
        max_sessions: 1,
        display_prompt: `Generate Playwright: ${formName || formDescription.substring(0, 50)}`,
        timeout_seconds: 120,
      });

      if (task.output_log) {
        // Extract code from the AI response
        const output = task.output_log;

        // Try multiple extraction patterns
        let generatedCode: string | null = null;

        // Pattern 1: Code block with explicit TypeScript/JavaScript language tag
        const typedCodeBlockMatch = output.match(
          /```(?:typescript|ts|javascript|js)\s*([\s\S]*?)```/,
        );
        if (typedCodeBlockMatch && typedCodeBlockMatch[1]) {
          generatedCode = typedCodeBlockMatch[1].trim();
        }

        // Pattern 2: Look for any code block containing Playwright test code
        if (!generatedCode) {
          const allCodeBlocks = [...output.matchAll(/```(?:\w*)?\s*([\s\S]*?)```/g)];
          for (const match of allCodeBlocks) {
            const content = match[1]?.trim();
            if (
              content &&
              (content.includes("import { test, expect }") || content.includes("test("))
            ) {
              generatedCode = content;
              break;
            }
          }
        }

        // Pattern 3: If no code block found, look for import statement and extract from there
        if (!generatedCode && output.includes("import { test, expect }")) {
          const importIndex = output.indexOf("import { test, expect }");
          // Find the end - either the next ``` or end of content
          const endIndex = output.indexOf("```", importIndex);
          if (endIndex > importIndex) {
            generatedCode = output.substring(importIndex, endIndex).trim();
          } else {
            // Take from import to the end, trimming any trailing explanation
            let code = output.substring(importIndex).trim();
            // Remove any trailing text after the last }); which ends the test
            const lastClosing = code.lastIndexOf("});");
            if (lastClosing > 0) {
              code = code.substring(0, lastClosing + 3);
            }
            generatedCode = code;
          }
        }

        // Pattern 4: Look for test() function directly
        if (!generatedCode && output.includes("test(")) {
          const testIndex = output.indexOf("test(");
          generatedCode = output.substring(testIndex).trim();
        }

        if (generatedCode && generatedCode.includes("test(")) {
          setFormScriptContent(generatedCode);
          setLastGeneratedFromDescription(formDescription); // Track which description generated this code
          setViewMode("code"); // Switch to code view to show the result
          onLog("success", "Script generated successfully!");

          // Validate that the generated code covers all requirements
          setCoverageWarnings([]); // Clear previous warnings
          const warnings = await validateCoverage(generatedCode, formDescription);
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

  // Validate that generated code covers all requirements from the description
  const validateCoverage = async (code: string, description: string): Promise<string[]> => {
    if (!description.trim() || !code.trim()) {
      return [];
    }

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
      // Use async task execution with polling
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

        // Extract missing items
        const missingMatch = output.match(/MISSING:\s*([\s\S]*?)(?:$|(?=\n\n))/i);
        if (missingMatch) {
          const missingText = missingMatch[1];
          const missingItems = missingText
            .split("\n")
            .map((line: string) => line.replace(/^[-*•]\s*/, "").trim())
            .filter((line: string) => line.length > 0);

          if (missingItems.length > 0) {
            onLog(
              "warning",
              `Found ${missingItems.length} missing requirement(s) in generated code`,
            );
            return missingItems;
          }
        }

        // Fallback: if we can't parse but it says incomplete
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

  // Refine script based on test results
  const _refineScript = async () => {
    if (!refinementPrompt.trim() && !executionResult?.error) {
      onLog("warning", "Please describe how to improve the script");
      return;
    }

    setIsGenerating(true);
    onLog("info", "Refining Playwright script based on feedback...");

    // Build context from test results
    const testErrors =
      executionResult?.structured_output?.specs
        ?.filter((spec) => spec.status !== "expected" && spec.error)
        .map((spec) => `Test "${spec.title}" failed:\n${spec.error}`)
        .join("\n\n") ||
      executionResult?.error ||
      "";

    // Include page snapshot if available (shows actual elements on the page)
    const pageSnapshot = executionResult?.structured_output?.page_snapshot || "";
    const pageSnapshotSection = pageSnapshot
      ? `\n## Page Snapshot (YAML showing all elements on the page)\nThis shows the actual elements available on the page. Use this to fix selectors.\n\`\`\`yaml\n${pageSnapshot}\n\`\`\`\n`
      : "";

    // Include AI instructions if present
    const aiInstructionsSection = formAiInstructions.trim()
      ? `\n## AI Instructions (Important - Follow These)\n${formAiInstructions}\n`
      : "";

    const prompt = `Refine this Playwright test script based on the test results and user feedback.

## Current Script
\`\`\`typescript
${formScriptContent}
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
      // Use async task execution with polling
      const task = await executeAiTask({
        name: "ai-analysis",
        content: prompt,
        max_sessions: 1,
        display_prompt: `Refine Playwright: ${formName || "script"}`,
        timeout_seconds: 120,
      });

      if (task.output_log) {
        const output = task.output_log;
        let generatedCode: string | null = null;

        // Extract code from response - first try explicit TypeScript/JavaScript blocks
        const typedCodeBlockMatch = output.match(
          /```(?:typescript|ts|javascript|js)\s*([\s\S]*?)```/,
        );
        if (typedCodeBlockMatch && typedCodeBlockMatch[1]) {
          generatedCode = typedCodeBlockMatch[1].trim();
        }

        // Fallback: look for any code block containing Playwright test code
        if (!generatedCode) {
          const allCodeBlocks = [...output.matchAll(/```(?:\w*)?\s*([\s\S]*?)```/g)];
          for (const match of allCodeBlocks) {
            const content = match[1]?.trim();
            if (
              content &&
              (content.includes("import { test, expect }") || content.includes("test("))
            ) {
              generatedCode = content;
              break;
            }
          }
        }

        // Pattern 3: If no code block found, look for import statement directly
        if (!generatedCode && output.includes("import { test, expect }")) {
          const importIndex = output.indexOf("import { test, expect }");
          const endIndex = output.indexOf("```", importIndex);
          if (endIndex > importIndex) {
            generatedCode = output.substring(importIndex, endIndex).trim();
          } else {
            let code = output.substring(importIndex).trim();
            const lastClosing = code.lastIndexOf("});");
            if (lastClosing > 0) {
              code = code.substring(0, lastClosing + 3);
            }
            generatedCode = code;
          }
        }

        if (generatedCode && generatedCode.includes("test(")) {
          setFormScriptContent(generatedCode);
          setViewMode("code");
          setRefinementPrompt(""); // Clear the refinement prompt
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

  // Check if description has changed since code was generated
  const hasDescriptionChangedSinceGeneration = () => {
    // Only warn if we have a record of what description was used to generate the code
    // AND the current description is different
    return (
      lastGeneratedFromDescription !== null &&
      formDescription.trim() !== lastGeneratedFromDescription.trim()
    );
  };

  // Verify workflow objective using AI analysis
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
      // Use async task execution with polling
      const task = await executeAiTask({
        name: "ai-analysis",
        content: prompt,
        max_sessions: 1,
        display_prompt: `Verifying: ${objective.substring(0, 50)}...`,
        timeout_seconds: 60,
        image_paths: testResult.screenshots || [],
      });

      if (task.output_log) {
        // Parse JSON from AI response
        const jsonMatch = task.output_log.match(/\{[\s\S]*\}/);
        if (jsonMatch) {
          const parsed = JSON.parse(jsonMatch[0]);
          return {
            verified: parsed.verified === true, // Explicit true check
            notes: parsed.notes || "No notes provided",
          };
        }
      }
      return { verified: false, notes: "Failed to parse AI verification response" };
    } catch (error) {
      return { verified: false, notes: `Verification error: ${error}` };
    }
  };

  // Auto-refinement loop: runs test, refines on failure, repeats until success
  const runAutoRefinementLoop = async () => {
    if (!editingScript) {
      onLog("warning", "Please save the script first before running auto-refinement");
      return;
    }

    // Check if description has changed since code was generated
    if (hasDescriptionChangedSinceGeneration()) {
      const confirmed = confirm(
        "The description has changed since the code was generated.\n\n" +
          "The test may not match your current description.\n\n" +
          "Do you want to run anyway? (Click 'Generate Script' first to update the code)",
      );
      if (!confirmed) {
        return;
      }
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

    let currentScriptContent = formScriptContent;
    let scriptId = editingScript.id;
    let iteration = 0;

    // Save current form state to backend before first iteration
    // This ensures the backend has the latest code (especially important after copying a script)
    try {
      log("Saving current script state...");
      const preSaveResponse = await fetch(`${getApiBase()}/playwright/tests/${scriptId}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: formName,
          description: formDescription,
          ai_instructions: formAiInstructions || undefined,
          target_url: formTargetUrl,
          script_content: formScriptContent,
          category: formCategory,
          tags: formTags
            .split(",")
            .map((t) => t.trim())
            .filter(Boolean),
          timeout_seconds: formTimeoutSeconds,
          display_mode: formDisplayMode,
          browser: formBrowser,
          workflow_objective: formWorkflowObjective || undefined,
          success_criteria: formSuccessCriteria.length > 0 ? formSuccessCriteria : undefined,
          is_workflow_automation: formIsWorkflowAutomation,
        }),
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

        // Run the test
        setExecutionState("running");
        const runResponse = await fetch(`${getApiBase()}/playwright/tests/${scriptId}/run`, {
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

        // Update cumulative stats across all iterations
        setAutoRefineCumulativeStats((prev) => ({
          totalRuns: prev.totalRuns + 1,
          totalPassed: prev.totalPassed + testResult.tests_passed,
          totalFailed: prev.totalFailed + testResult.tests_failed,
          totalSkipped: prev.totalSkipped + testResult.tests_skipped,
          totalDurationMs: prev.totalDurationMs + testResult.duration_ms,
        }));

        // Check if stopped by user
        if (autoRefineAbortRef.current) {
          log("Stopped by user");
          break;
        }

        if (testResult.passed) {
          // Check if this is workflow automation requiring objective verification
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

          // Traditional test mode - script pass = success
          log(`SUCCESS! Test passed on iteration ${iteration}`);
          onLog("success", `Auto-refinement succeeded after ${iteration} iteration(s)!`);
          break;
        }

        // Extract and log the specific problem
        const failedSpecs = testResult.structured_output?.specs?.filter(
          (spec) => spec.status !== "expected" && spec.error,
        );

        if (failedSpecs && failedSpecs.length > 0) {
          for (const spec of failedSpecs) {
            // Extract a concise error summary (first line or key info)
            const errorLines = (spec.error || "").split("\n");
            let problemSummary = errorLines[0] || "Unknown error";

            // Look for common Playwright error patterns
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

        // Build context from test results for AI
        const testErrors =
          failedSpecs?.map((spec) => `Test "${spec.title}" failed:\n${spec.error}`).join("\n\n") ||
          testResult.error ||
          "";

        const pageSnapshot = testResult.structured_output?.page_snapshot || "";
        const pageSnapshotSection = pageSnapshot
          ? `\n## Page Snapshot (YAML showing all elements on the page)\nThis shows the actual elements available on the page. Use this to fix selectors.\n\`\`\`yaml\n${pageSnapshot}\n\`\`\`\n`
          : "";

        // Include AI instructions if present
        const aiInstructionsSection = formAiInstructions.trim()
          ? `\n## AI Instructions (Important - Follow These)\n${formAiInstructions}\n`
          : "";

        // Include user hint if provided (for real-time guidance during auto-refine)
        const userHintSection = autoRefineUserHint.trim()
          ? `\n## User Hint (IMPORTANT - From the user watching this iteration)\n${autoRefineUserHint}\n`
          : "";

        // Include the original description so AI knows the full goal
        const descriptionSection = formDescription.trim()
          ? `\n## Original Test Description (IMPORTANT - Ensure ALL requirements are met)\n${formDescription}\n`
          : "";

        const prompt = `Refine this Playwright test script based on the test results.

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

        // Collect visual context (screenshots, trace, video)
        const imagePaths: string[] = [];
        const videoPaths: string[] = [];
        let tracePath: string | undefined;

        // Include final screenshot if enabled and available
        if (includeScreenshot && testResult.screenshots && testResult.screenshots.length > 0) {
          const lastScreenshot = testResult.screenshots[testResult.screenshots.length - 1];
          imagePaths.push(lastScreenshot);
          log(`Including screenshot: ${lastScreenshot.split(/[\\/]/).pop()}`);
        }

        // Include trace if enabled and available
        if (includeTraceData && testResult.trace_path) {
          tracePath = testResult.trace_path;
          log(`Including trace data from: ${tracePath.split(/[\\/]/).pop()}`);
        }

        // Include video after N iterations (if enabled and available)
        const shouldIncludeVideo = includeVideo && iteration >= includeVideoAfterIterations;
        if (shouldIncludeVideo && testResult.video_paths && testResult.video_paths.length > 0) {
          videoPaths.push(...testResult.video_paths);
          log(`Iteration ${iteration}: Including video for additional context`);
        }

        // Call AI to refine using async task execution with polling
        setIsGenerating(true);
        let aiTask;
        try {
          aiTask = await executeAiTask({
            name: "ai-analysis",
            content: prompt,
            max_sessions: 1,
            display_prompt: `Auto-refine iteration ${iteration}: ${formName || "script"}`,
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

        // Clear user hint after it's been sent to AI
        if (autoRefineUserHint.trim()) {
          log(`[HINT SENT TO AI] "${autoRefineUserHint.trim()}"`);
          setAutoRefineUserHint("");
          setHintQueued(false);
        }

        // Check if stopped by user
        if (autoRefineAbortRef.current) {
          log("Stopped by user");
          break;
        }

        if (!aiTask.output_log) {
          log("AI refinement failed: No output");
          console.error("[AUTO-REFINE] AI failed: No output_log");
          break;
        }

        // Extract code and changes summary from AI response
        const output = aiTask.output_log;
        console.log("[AUTO-REFINE] AI output length:", output?.length);
        let generatedCode: string | null = null;

        // Extract CHANGES summary
        const changesMatch = output.match(/CHANGES:\s*(.+?)(?:\n|$)/i);
        if (changesMatch && changesMatch[1]) {
          const changesSummary = changesMatch[1].trim();
          log(`FIX: ${changesSummary}`);
        }

        // First try to match explicitly typed TypeScript/JavaScript code blocks
        const typedCodeBlockMatch = output.match(
          /```(?:typescript|ts|javascript|js)\s*([\s\S]*?)```/,
        );
        if (typedCodeBlockMatch && typedCodeBlockMatch[1]) {
          generatedCode = typedCodeBlockMatch[1].trim();
        }

        // Fallback: look for any code block containing Playwright test code
        if (!generatedCode) {
          const allCodeBlocks = [...output.matchAll(/```(?:\w*)?\s*([\s\S]*?)```/g)];
          for (const match of allCodeBlocks) {
            const content = match[1]?.trim();
            if (
              content &&
              (content.includes("import { test, expect }") || content.includes("test("))
            ) {
              generatedCode = content;
              break;
            }
          }
        }

        // Pattern 3: If no code block found, look for import statement directly
        if (!generatedCode && output.includes("import { test, expect }")) {
          const importIndex = output.indexOf("import { test, expect }");
          const endIndex = output.indexOf("```", importIndex);
          if (endIndex > importIndex) {
            generatedCode = output.substring(importIndex, endIndex).trim();
          } else {
            let code = output.substring(importIndex).trim();
            const lastClosing = code.lastIndexOf("});");
            if (lastClosing > 0) {
              code = code.substring(0, lastClosing + 3);
            }
            generatedCode = code;
          }
        }

        if (!generatedCode || !generatedCode.includes("test(")) {
          log("AI response didn't contain valid code");
          console.error(
            "[AUTO-REFINE] Invalid code. generatedCode:",
            generatedCode?.substring(0, 200),
          );
          console.error("[AUTO-REFINE] Full output:", output?.substring(0, 500));
          break;
        }
        console.log("[AUTO-REFINE] Extracted valid code, length:", generatedCode.length);

        currentScriptContent = generatedCode;
        setFormScriptContent(generatedCode);
        log("Saving updated script...");

        // Save the updated script
        const saveResponse = await fetch(`${getApiBase()}/playwright/tests/${scriptId}`, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            script_content: generatedCode,
          }),
        });
        const saveResult = await saveResponse.json();

        if (!saveResult.success) {
          log(`Failed to save script: ${saveResult.error}`);
          console.error("[AUTO-REFINE] Save failed:", saveResult);
          break;
        }
        console.log("[AUTO-REFINE] Save successful, continuing to next iteration");

        log("Re-running test with fixes...");
        // Small delay before next iteration
        await new Promise((resolve) => setTimeout(resolve, 1000));
      }

      if (iteration >= autoRefineMaxIterations) {
        log(`Reached maximum iterations (${autoRefineMaxIterations}). Test still failing.`);
        onLog(
          "warning",
          `Auto-refinement stopped after ${autoRefineMaxIterations} iterations without success`,
        );
      }

      // Reload scripts to get latest state
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

  // Stop auto-refinement loop
  const _stopAutoRefine = async () => {
    autoRefineAbortRef.current = true;
    setIsAutoRefining(false);
    setIsGenerating(false);
    setExecutionState("idle");
    onLog("info", "Auto-refinement stopped by user");

    // Stop the AI analysis if it's running
    try {
      await fetch(`${getApiBase()}/stop-ai-analysis`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
      });
    } catch (error) {
      console.error("Failed to stop AI analysis:", error);
    }
  };

  // Save and run: saves current form state, then runs the test
  const saveAndRunScript = async (id: string) => {
    // Check if description has changed since code was generated
    if (hasDescriptionChangedSinceGeneration()) {
      const confirmed = confirm(
        "The description has changed since the code was generated.\n\n" +
          "The test may not match your current description.\n\n" +
          "Do you want to run anyway? (Click 'Generate Script' first to update the code)",
      );
      if (!confirmed) {
        return;
      }
    }

    // First save the current form state
    try {
      const saveResponse = await fetch(`${getApiBase()}/playwright/tests/${id}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: formName,
          description: formDescription,
          ai_instructions: formAiInstructions || undefined,
          target_url: formTargetUrl,
          script_content: formScriptContent,
          category: formCategory,
          tags: formTags
            .split(",")
            .map((t) => t.trim())
            .filter(Boolean),
          timeout_seconds: formTimeoutSeconds,
          display_mode: formDisplayMode,
          browser: formBrowser,
          workflow_objective: formWorkflowObjective || undefined,
          success_criteria: formSuccessCriteria.length > 0 ? formSuccessCriteria : undefined,
          is_workflow_automation: formIsWorkflowAutomation,
        }),
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

    // Now run the test
    await runScript(id);
  };

  // Regenerate description from code using AI
  const _regenerateDescriptionFromCode = async () => {
    if (!formScriptContent || !formScriptContent.includes("test(")) {
      onLog("warning", "No valid script content to analyze");
      return;
    }

    setIsRegeneratingDescription(true);
    onLog("info", "Generating description from code...");

    const prompt = `Analyze this Playwright test script and generate a clear, concise natural language description of what it does.

## Script
\`\`\`typescript
${formScriptContent}
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
      // Use async task execution with polling
      const task = await executeAiTask({
        name: "ai-analysis",
        content: prompt,
        max_sessions: 1,
        display_prompt: `Generate description for: ${formName || "script"}`,
        timeout_seconds: 60,
      });

      if (task.output_log) {
        const output = task.output_log.trim();
        // Remove any markdown formatting if present
        const cleanDescription = output
          .replace(/^```[\s\S]*?```$/gm, "")
          .replace(/^\*\*.*?\*\*:?\s*/gm, "")
          .trim();

        // Show preview instead of immediately applying
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

  // Accept the generated description preview
  const acceptDescriptionPreview = async () => {
    if (!descriptionPreview) return;

    setFormDescription(descriptionPreview);
    descriptionChangedByUser.current = true; // Trigger auto-save
    onLog("success", "Description updated from code");

    // Auto-save the updated description if editing
    if (editingScript) {
      try {
        await fetch(`${getApiBase()}/playwright/tests/${editingScript.id}`, {
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

  // Reject the generated description preview
  const rejectDescriptionPreview = () => {
    setShowDescriptionPreview(false);
    setDescriptionPreview(null);
    onLog("info", "Description update cancelled");
  };

  // Form helpers
  const resetForm = () => {
    setFormName("");
    setFormDescription("");
    setFormAiInstructions("");
    setFormTargetUrl("");
    setFormScriptContent(DEFAULT_SCRIPT_CONTENT);
    setFormCategory("");
    setFormTags("");
    setFormTimeoutSeconds(60);
    setFormDisplayMode("headless");
    setFormBrowser("chromium");
    setFormWorkflowObjective("");
    setFormSuccessCriteria([]);
    setFormIsWorkflowAutomation(false);
    setViewMode("natural_language");
  };

  const _startEditing = (script: PlaywrightScript) => {
    descriptionChangedByUser.current = false; // Reset flag when loading a script
    nameChangedByUser.current = false; // Reset flag when loading a script
    aiInstructionsChangedByUser.current = false; // Reset flag when loading a script
    setLastGeneratedFromDescription(null); // We don't know if existing code matches description
    setEditingScript(script);
    setFormName(script.name);
    setFormDescription(script.description);
    setFormAiInstructions(script.ai_instructions || "");
    setFormTargetUrl(script.target_url);
    setFormScriptContent(script.script_content);
    setFormCategory(script.category);
    setFormTags(script.tags.join(", "));
    setFormTimeoutSeconds(script.timeout_seconds);
    setFormDisplayMode(script.display_mode);
    setFormBrowser(script.browser);
    setFormWorkflowObjective(script.workflow_objective || "");
    setFormSuccessCriteria(script.success_criteria || []);
    setFormIsWorkflowAutomation(script.is_workflow_automation || false);
    setIsCreating(false);
  };

  const _startCreating = () => {
    resetForm();
    setEditingScript(null);
    setIsCreating(true);
  };

  // Filter scripts
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

  // Always show the form (either creating new or editing)
  // If not explicitly creating/editing, start with a new script form
  useEffect(() => {
    if (!editScriptId && !isCreating && !editingScript) {
      setIsCreating(true);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const accentColors = getAccentColors("purple");

  // Filter scripts based on search
  const filteredScripts = scripts.filter((script) => {
    if (!searchQuery) return true;
    const query = searchQuery.toLowerCase();
    return (
      script.name.toLowerCase().includes(query) ||
      script.description?.toLowerCase().includes(query) ||
      script.target_url?.toLowerCase().includes(query)
    );
  });

  // Select a script for editing
  const selectScript = (script: PlaywrightScript) => {
    setEditingScript(script);
    setIsCreating(false);
    setFormName(script.name);
    setFormDescription(script.description || "");
    setFormTargetUrl(script.target_url || "");
    setFormScriptContent(script.script_content || DEFAULT_SCRIPT_CONTENT);
    setFormCategory(script.category || "");
    setFormTags(script.tags?.join(", ") || "");
    setFormTimeoutSeconds(script.timeout_seconds || 60);
    setFormDisplayMode((script.display_mode as DisplayMode) || "headless");
    setFormBrowser(script.browser || "chromium");
    setFormAiInstructions(script.ai_instructions || "");
    setFormWorkflowObjective(script.workflow_objective || "");
    setFormSuccessCriteria(script.success_criteria || []);
    setFormIsWorkflowAutomation(script.is_workflow_automation || false);
  };

  // Start creating new script
  const startCreate = () => {
    setEditingScript(null);
    setIsCreating(true);
    resetForm();
  };

  return (
    <div className="h-full flex">
      {/* Left Panel - Script List */}
      <div className="w-80 border-r border-border flex flex-col bg-card">
        {/* Header */}
        <div className="p-4 border-b border-border">
          <div className="flex items-center justify-between mb-2">
            <h2 className="text-lg font-semibold flex items-center gap-2">
              <TestTube className="w-5 h-5" style={{ color: accentColors.bgSolid }} />
              Scripts
            </h2>
          </div>

          {/* Action buttons */}
          <BuilderToolbar
            className="mb-3"
            actions={[
              toolbarActions.aiPlaceholder(),
              toolbarActions.editInWeb("/build/playwright-tests"),
              toolbarActions.delete(() => setIsSelectionMode(!isSelectionMode), isSelectionMode),
              toolbarActions.new(startCreate, "New"),
            ]}
          />

          {/* Search */}
          <div className="relative">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
            <input
              type="text"
              placeholder="Search scripts..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full pl-9 pr-3 py-2 bg-muted border border-border rounded-lg text-sm focus:outline-none focus:border-primary"
            />
          </div>
        </div>

        {/* Selection Mode Header */}
        {isSelectionMode && (
          <div className="flex items-center justify-between px-4 py-2 bg-red-500/10 border-b border-red-500/30">
            <span className="text-sm text-red-400">{selectedIds.size} selected</span>
            <div className="flex items-center gap-2">
              <button
                onClick={() => setSelectedIds(new Set(filteredScripts.map((s) => s.id)))}
                className="px-2 py-1 text-xs bg-muted/80 hover:bg-muted text-foreground rounded"
              >
                Select All
              </button>
              <button
                onClick={() => setSelectedIds(new Set())}
                disabled={selectedIds.size === 0}
                className="px-2 py-1 text-xs bg-muted/80 hover:bg-muted text-foreground rounded disabled:opacity-50"
              >
                Clear
              </button>
              <button
                onClick={() => setShowBatchDeleteDialog(true)}
                disabled={selectedIds.size === 0}
                className="px-2 py-1 text-xs bg-red-600 hover:bg-red-500 text-white rounded disabled:opacity-50 disabled:cursor-not-allowed"
              >
                Delete Selected
              </button>
              <button
                onClick={exitSelectionMode}
                className="p-1 text-muted-foreground hover:text-foreground"
                title="Cancel selection"
              >
                <X className="w-4 h-4" />
              </button>
            </div>
          </div>
        )}

        {/* Script List */}
        <div className="flex-1 overflow-y-auto p-2">
          {_loading ? (
            <div className="flex items-center justify-center py-8">
              <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
            </div>
          ) : filteredScripts.length === 0 ? (
            <div className="text-center py-8 text-muted-foreground">
              <TestTube className="w-8 h-8 mx-auto mb-2 opacity-50" />
              <p className="text-sm">No scripts found</p>
            </div>
          ) : (
            <div className="space-y-1">
              {filteredScripts.map((script) => (
                <button
                  key={script.id}
                  onClick={() =>
                    isSelectionMode ? toggleSelection(script.id) : selectScript(script)
                  }
                  className={`w-full text-left p-3 rounded-lg transition-colors ${
                    editingScript?.id === script.id ? "bg-muted/80" : "hover:bg-muted"
                  } ${isSelectionMode && selectedIds.has(script.id) ? "bg-red-500/10" : ""}`}
                >
                  <div className="flex items-center gap-2">
                    {isSelectionMode && (
                      <div
                        className={`w-4 h-4 rounded border flex-shrink-0 flex items-center justify-center ${
                          selectedIds.has(script.id)
                            ? "bg-red-500 border-red-500"
                            : "border-muted-foreground hover:border-red-400"
                        }`}
                      >
                        {selectedIds.has(script.id) && <Check className="w-3 h-3 text-white" />}
                      </div>
                    )}
                    <div className="flex-1 min-w-0">
                      <div className="font-medium text-sm truncate">{script.name}</div>
                      {script.target_url && (
                        <div className="text-xs text-muted-foreground truncate mt-0.5">
                          {script.target_url}
                        </div>
                      )}
                      {script.category && (
                        <span className="text-xs px-1.5 py-0.5 rounded bg-muted text-muted-foreground mt-1.5 inline-block">
                          {script.category}
                        </span>
                      )}
                    </div>
                  </div>
                </button>
              ))}
            </div>
          )}
        </div>
      </div>

      {/* Right Panel - Editor */}
      <div className="flex-1 flex flex-col bg-card/50 overflow-hidden">
        {!editingScript && !isCreating ? (
          <div className="flex-1 flex items-center justify-center">
            <div className="text-center text-muted-foreground">
              <TestTube className="w-12 h-12 mx-auto mb-3 opacity-30" />
              <p className="text-lg mb-2">No script selected</p>
              <p className="text-sm mb-4">Select a script to edit or create a new one</p>
              <button
                onClick={startCreate}
                className="inline-flex items-center gap-2 px-4 py-2 rounded-lg transition-colors"
                style={{ backgroundColor: accentColors.bgSolid, color: "#000" }}
              >
                <Plus className="w-4 h-4" />
                Create New Script
              </button>
            </div>
          </div>
        ) : (
          <>
            {/* Toolbar */}
            <div className="flex items-center justify-between p-4 border-b border-border">
              <h3 className="font-medium">
                {isCreating ? "New Script" : `Editing: ${editingScript?.name}`}
              </h3>
              <div className="flex items-center gap-2">
                <button
                  onClick={importScripts}
                  className="p-2 rounded-lg hover:bg-muted transition-colors"
                  title="Import"
                >
                  <Upload className="w-4 h-4" />
                </button>
                <button
                  onClick={exportScripts}
                  className="p-2 rounded-lg hover:bg-muted transition-colors"
                  title="Export"
                >
                  <Download className="w-4 h-4" />
                </button>
              </div>
            </div>

            {/* Scrollable Content */}
            <div className="flex-1 overflow-y-auto p-4 scrollbar-dark">
              {/* Playwright Test Builder Form - Always visible */}
              {(isCreating || editingScript) && (
                <div
                  className={`card p-4 space-y-4 border-2 ${getStatusColors("success").border} min-h-min`}
                >
                  <div className="flex items-center justify-between">
                    <h3 className="text-lg font-semibold flex items-center gap-2">
                      <TestTube className={`w-5 h-5 ${getStatusColors("success").text}`} />
                      Create New Script
                    </h3>
                    <div className="flex items-center gap-2">
                      {/* View Mode Toggle */}
                      <div className="flex items-center bg-card border border-border rounded-lg p-1">
                        <button
                          onClick={() => setViewMode("natural_language")}
                          className={`px-3 py-1 text-sm rounded flex items-center gap-1 ${
                            viewMode === "natural_language"
                              ? `${getStatusColors("success").bg} ${getStatusColors("success").text}`
                              : "text-muted-foreground hover:text-foreground"
                          }`}
                        >
                          <FileText className="w-4 h-4" />
                          Description
                        </button>
                        <button
                          onClick={() => setViewMode("code")}
                          className={`px-3 py-1 text-sm rounded flex items-center gap-1 ${
                            viewMode === "code"
                              ? `${getStatusColors("success").bg} ${getStatusColors("success").text}`
                              : "text-muted-foreground hover:text-foreground"
                          }`}
                        >
                          <Code className="w-4 h-4" />
                          Code
                        </button>
                      </div>
                      <button
                        onClick={() => {
                          setIsCreating(false);
                          setEditingScript(null);
                          resetForm();
                        }}
                        className="p-1 hover:bg-card rounded"
                      >
                        <X className="w-5 h-5" />
                      </button>
                    </div>
                  </div>

                  <div className="grid grid-cols-2 gap-4">
                    <div>
                      <label className="block text-sm font-medium mb-1">Name *</label>
                      <input
                        type="text"
                        value={formName}
                        onChange={(e) => setFormName(e.target.value)}
                        placeholder="Login Flow Test"
                        className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-green-500/50"
                      />
                    </div>
                    <div>
                      <label className="block text-sm font-medium mb-1">Target URL</label>
                      <div className="relative">
                        <Globe className="absolute left-3 top-1/2 transform -translate-y-1/2 w-4 h-4 text-muted-foreground" />
                        <input
                          type="text"
                          value={formTargetUrl}
                          onChange={(e) => setFormTargetUrl(e.target.value)}
                          placeholder="http://localhost:3000"
                          className="w-full pl-10 pr-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-green-500/50"
                        />
                      </div>
                    </div>
                  </div>

                  {viewMode === "natural_language" ? (
                    <div>
                      <div className="flex items-center justify-between mb-1">
                        <label className="block text-sm font-medium">
                          Description (Natural Language)
                        </label>
                        <div className="flex items-center gap-2">
                          <PromptSnippetSelector
                            mode="dropdown"
                            onSelect={insertPromptSnippetAtCursor}
                          />
                          <button
                            onClick={generateScript}
                            disabled={isGenerating || !formDescription.trim()}
                            className="flex items-center gap-1.5 px-3 py-1.5 text-sm bg-gradient-to-r from-purple-500 to-pink-500 text-white rounded-lg hover:from-purple-600 hover:to-pink-600 disabled:opacity-50 disabled:cursor-not-allowed transition-all"
                          >
                            {isGenerating ? (
                              <>
                                <Loader2 className="w-4 h-4 animate-spin" />
                                Generating...
                              </>
                            ) : (
                              <>
                                <Sparkles className="w-4 h-4" />
                                Generate Script
                              </>
                            )}
                          </button>
                        </div>
                      </div>
                      <div className="relative">
                        <textarea
                          ref={descriptionTextareaRef}
                          value={formDescription}
                          onChange={(e) => setFormDescription(e.target.value)}
                          onKeyDown={promptSnippetMention.handleKeyDown}
                          onInput={promptSnippetMention.handleInput}
                          placeholder="Describe what this test should do in plain English... (Type @ to insert a prompt snippet)&#10;&#10;Example: There is a Capture Screen button on the Image Extraction page. Click it and select Capture Screen in the dialog box."
                          rows={6}
                          className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-green-500/50"
                        />
                        {/* @-mention popup for prompt snippet insertion */}
                        {promptSnippetMention.isActive && (
                          <PromptSnippetSelector
                            mode="popup"
                            onSelect={promptSnippetMention.handleSelect}
                            onClose={promptSnippetMention.handleClose}
                            searchQuery={promptSnippetMention.searchQuery}
                            position={promptSnippetMention.position}
                          />
                        )}
                      </div>
                      <p className="text-xs text-muted-foreground mt-1">
                        Describe what you want to test, then click "Generate Script" to create the
                        Playwright code.
                      </p>

                      {/* AI Instructions (optional) */}
                      <div className="mt-4">
                        <label className="block text-sm font-medium mb-1 flex items-center gap-2">
                          <Sparkles className={`w-4 h-4 ${getAccentColors("purple").text}`} />
                          AI Instructions (Optional)
                        </label>
                        <textarea
                          value={formAiInstructions}
                          onChange={(e) => setFormAiInstructions(e.target.value)}
                          placeholder="Additional instructions for the AI that modify how the description is interpreted...&#10;&#10;Example: The feature is currently broken. Stop after capturing the screen and take a screenshot. Expect failure but capture the state for debugging."
                          rows={3}
                          className={`w-full px-3 py-2 ${getAccentColors("purple").bg} border ${getAccentColors("purple").border} rounded-lg focus:outline-none focus:ring-2 focus:ring-purple-500 text-sm`}
                        />
                        <p className="text-xs text-muted-foreground mt-1">
                          Use this to give the AI additional context without changing the test
                          description.
                        </p>
                      </div>

                      {/* Workflow Automation (optional - collapsible) */}
                      <details
                        className={`mt-4 border ${getAccentColors("blue").border} rounded-lg p-3 ${getAccentColors("blue").bg}`}
                      >
                        <summary
                          className={`cursor-pointer text-sm font-medium flex items-center gap-2 ${getAccentColors("blue").text}`}
                        >
                          <CheckSquare className="w-4 h-4" />
                          Workflow Automation Settings (Optional)
                        </summary>
                        <div className="mt-3 space-y-3">
                          <div className="flex items-center gap-2">
                            <input
                              type="checkbox"
                              id="is-workflow-automation"
                              checked={formIsWorkflowAutomation}
                              onChange={(e) => setFormIsWorkflowAutomation(e.target.checked)}
                              className={`w-4 h-4 rounded ${getAccentColors("blue").border} ${getAccentColors("blue").text} focus:ring-blue-500`}
                            />
                            <label htmlFor="is-workflow-automation" className="text-sm">
                              This is workflow automation (script success is expected, verify
                              objective instead)
                            </label>
                          </div>

                          <div>
                            <label className="block text-sm font-medium mb-1">
                              Workflow Objective
                            </label>
                            <textarea
                              value={formWorkflowObjective}
                              onChange={(e) => setFormWorkflowObjective(e.target.value)}
                              placeholder="What should this workflow achieve?&#10;&#10;Example: Create a new state called 'test-state' and verify it appears in the state list"
                              rows={2}
                              className={`w-full px-3 py-2 bg-background border ${getAccentColors("blue").border} rounded-lg focus:outline-none focus:ring-2 focus:ring-blue-500 text-sm`}
                            />
                          </div>

                          <div>
                            <label className="block text-sm font-medium mb-1">
                              Success Criteria (one per line)
                            </label>
                            <textarea
                              value={formSuccessCriteria.join("\n")}
                              onChange={(e) =>
                                setFormSuccessCriteria(
                                  e.target.value
                                    .split("\n")
                                    .map((s) => s.trim())
                                    .filter(Boolean),
                                )
                              }
                              placeholder="Specific things to verify after script passes:&#10;- 'test-state' appears in state list&#10;- State is visible on canvas&#10;- No error messages shown"
                              rows={3}
                              className={`w-full px-3 py-2 bg-background border ${getAccentColors("blue").border} rounded-lg focus:outline-none focus:ring-2 focus:ring-blue-500 text-sm`}
                            />
                          </div>

                          <p className="text-xs text-muted-foreground">
                            When enabled, script execution success is expected. After the script
                            passes, AI will verify whether the workflow objective was actually
                            achieved.
                          </p>
                        </div>
                      </details>
                    </div>
                  ) : (
                    <div>
                      <div className="flex items-center justify-between mb-1">
                        <label className="block text-sm font-medium">
                          Script Content (.spec.ts) *
                        </label>
                        {formDescription.trim() && (
                          <button
                            onClick={generateScript}
                            disabled={isGenerating}
                            className="flex items-center gap-1.5 px-3 py-1.5 text-sm bg-card border border-border rounded-lg hover:bg-muted disabled:opacity-50 disabled:cursor-not-allowed transition-all"
                            title="Regenerate script from description"
                          >
                            {isGenerating ? (
                              <>
                                <Loader2 className="w-4 h-4 animate-spin" />
                                Regenerating...
                              </>
                            ) : (
                              <>
                                <RefreshCw className="w-4 h-4" />
                                Regenerate
                              </>
                            )}
                          </button>
                        )}
                      </div>
                      <textarea
                        value={formScriptContent}
                        onChange={(e) => setFormScriptContent(e.target.value)}
                        rows={12}
                        className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-green-500/50 font-mono text-sm"
                        spellCheck={false}
                      />

                      {/* Coverage validation status */}
                      {isValidatingCoverage && (
                        <div className="mt-2 flex items-center gap-2 text-sm text-muted-foreground">
                          <Loader2 className="w-4 h-4 animate-spin" />
                          Validating script covers all requirements...
                        </div>
                      )}

                      {/* Coverage warnings */}
                      {coverageWarnings.length > 0 && (
                        <div
                          className={`mt-3 p-3 ${getAccentColors("amber").bg} border ${getAccentColors("amber").border} rounded-lg`}
                        >
                          <div className="flex items-start gap-2">
                            <Info
                              className={`w-4 h-4 ${getAccentColors("amber").text} mt-0.5 flex-shrink-0`}
                            />
                            <div className="flex-1">
                              <div
                                className={`text-sm font-medium ${getAccentColors("amber").text} mb-1`}
                              >
                                Missing Requirements Detected
                              </div>
                              <div className={`text-sm ${getAccentColors("amber").text} mb-2`}>
                                The generated code may not implement all functionality from the
                                description:
                              </div>
                              <ul
                                className={`text-sm ${getAccentColors("amber").text} list-disc list-inside space-y-1`}
                              >
                                {coverageWarnings.map((warning, i) => (
                                  <li key={i}>{warning}</li>
                                ))}
                              </ul>
                              <button
                                onClick={() => {
                                  // Re-generate with emphasis on missing requirements
                                  const missingItems = coverageWarnings.join(", ");
                                  setFormAiInstructions(
                                    (prev) =>
                                      prev +
                                      (prev ? "\n\n" : "") +
                                      `IMPORTANT: Make sure to implement these missing requirements: ${missingItems}`,
                                  );
                                  setCoverageWarnings([]);
                                  generateScript();
                                }}
                                disabled={isGenerating}
                                className={`mt-2 px-3 py-1.5 text-sm ${getAccentColors("amber").bg} hover:opacity-80 ${getAccentColors("amber").text} rounded-lg transition-colors disabled:opacity-50`}
                              >
                                <RefreshCw className="w-3 h-3 inline mr-1.5" />
                                Regenerate with missing requirements
                              </button>
                            </div>
                          </div>
                        </div>
                      )}
                    </div>
                  )}

                  <div className="grid grid-cols-2 gap-4">
                    <div>
                      <label className="block text-sm font-medium mb-1">Category</label>
                      <input
                        type="text"
                        value={formCategory}
                        onChange={(e) => setFormCategory(e.target.value)}
                        placeholder="E2E, Smoke, Regression"
                        className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-green-500/50"
                        list="category-suggestions"
                      />
                      <datalist id="category-suggestions">
                        {categories.map((cat) => (
                          <option key={cat} value={cat} />
                        ))}
                      </datalist>
                    </div>
                    <div>
                      <label className="block text-sm font-medium mb-1">
                        Tags (comma-separated)
                      </label>
                      <input
                        type="text"
                        value={formTags}
                        onChange={(e) => setFormTags(e.target.value)}
                        placeholder="login, auth, smoke"
                        className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-green-500/50"
                      />
                    </div>
                  </div>

                  <div className="grid grid-cols-3 gap-4">
                    <div>
                      <label className="block text-sm font-medium mb-1">Timeout (seconds)</label>
                      <input
                        type="number"
                        value={formTimeoutSeconds}
                        onChange={(e) => setFormTimeoutSeconds(parseInt(e.target.value) || 60)}
                        min={10}
                        max={600}
                        className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-green-500/50"
                      />
                    </div>
                    <div>
                      <label className="block text-sm font-medium mb-1">Browser</label>
                      <select
                        value={formBrowser}
                        onChange={(e) => setFormBrowser(e.target.value as typeof formBrowser)}
                        className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-green-500/50"
                      >
                        <option value="chromium">Chromium</option>
                        <option value="firefox">Firefox</option>
                        <option value="webkit">WebKit</option>
                      </select>
                    </div>
                    <div>
                      <label className="block text-sm font-medium mb-1">Display Mode</label>
                      <select
                        value={formDisplayMode}
                        onChange={(e) => setFormDisplayMode(e.target.value as DisplayMode)}
                        className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-green-500/50"
                      >
                        <option value="headless">Headless (No UI)</option>
                        <option value="headed">Headed (New Window)</option>
                        <option value="connect_existing">Open Browser (CDP)</option>
                      </select>
                      {formDisplayMode === "connect_existing" && (
                        <div className="mt-1 space-y-1">
                          <div className="flex items-center gap-1">
                            <p className={`text-xs ${getAccentColors("amber").text}`}>
                              Requires Chrome started with: --remote-debugging-port=9222
                            </p>
                            <div className="relative group">
                              <Info
                                className={`w-3.5 h-3.5 ${getAccentColors("amber").text} cursor-help`}
                              />
                              <div className="absolute right-0 bottom-full mb-2 hidden group-hover:block z-50 w-80">
                                <div className="bg-popover border border-border rounded-lg shadow-lg p-3 text-xs">
                                  <p className="font-medium mb-2">
                                    How to start Chrome with debugging:
                                  </p>
                                  <ol className="list-decimal list-inside space-y-1 text-muted-foreground">
                                    <li>Close all Chrome windows first</li>
                                    <li>
                                      Click the button below to restart Chrome with debugging, or:
                                    </li>
                                    <li>
                                      Create a shortcut with target:{" "}
                                      <code className="bg-muted px-1 rounded">
                                        chrome --remote-debugging-port=9222
                                      </code>
                                    </li>
                                  </ol>
                                </div>
                              </div>
                            </div>
                          </div>
                          <button
                            type="button"
                            onClick={async () => {
                              if (
                                !confirm(
                                  "This will close all Chrome windows and relaunch with debugging enabled. Continue?",
                                )
                              ) {
                                return;
                              }
                              try {
                                onLog("info", "Closing Chrome and relaunching with debug port...");
                                const response = await fetch(
                                  `${getApiBase()}/launch-debug-chrome`,
                                  {
                                    method: "POST",
                                  },
                                );
                                const result = await response.json();
                                if (result.success) {
                                  onLog(
                                    "success",
                                    "Chrome launched with debugging enabled. Navigate to your test page, then run the test.",
                                  );
                                } else {
                                  onLog("error", `Failed to launch Chrome: ${result.error}`);
                                }
                              } catch (error) {
                                onLog("error", `Failed to launch Chrome: ${error}`);
                              }
                            }}
                            className={`text-xs px-2 py-1 ${getAccentColors("amber").bg} ${getAccentColors("amber").text} rounded hover:opacity-80 transition-colors`}
                          >
                            Restart Chrome with Debug Port
                          </button>
                        </div>
                      )}
                    </div>
                  </div>

                  {/* Visual Context Settings for Auto-Refine (applies when using Save & Auto-Refine) */}
                  <div className="flex flex-wrap items-center gap-4 text-sm text-muted-foreground py-3 border-t border-border">
                    <div className="flex items-center gap-2">
                      <ImageIcon className="w-4 h-4" />
                      <span>Auto-Refine Visual Context:</span>
                    </div>
                    <label className="flex items-center gap-1.5 cursor-pointer">
                      <input
                        type="checkbox"
                        checked={includeScreenshot}
                        onChange={(e) => setIncludeScreenshot(e.target.checked)}
                        className="rounded border-border"
                      />
                      <span>Screenshot</span>
                    </label>
                    <label className="flex items-center gap-1.5 cursor-pointer">
                      <input
                        type="checkbox"
                        checked={includeTraceData}
                        onChange={(e) => setIncludeTraceData(e.target.checked)}
                        className="rounded border-border"
                      />
                      <span>Trace</span>
                    </label>
                    <label
                      className="flex items-center gap-1.5 cursor-pointer"
                      title="Video uses significantly more tokens. Only recommended for complex failures."
                    >
                      <input
                        type="checkbox"
                        checked={includeVideo}
                        onChange={(e) => setIncludeVideo(e.target.checked)}
                        className="rounded border-border"
                      />
                      <span>Video after</span>
                      <input
                        type="number"
                        min={1}
                        max={10}
                        value={includeVideoAfterIterations}
                        onChange={(e) =>
                          setIncludeVideoAfterIterations(Math.max(1, parseInt(e.target.value) || 3))
                        }
                        disabled={!includeVideo}
                        className={`w-12 px-1 py-0.5 text-center bg-background border border-border rounded ${!includeVideo ? "opacity-50" : ""}`}
                      />
                      <span className={!includeVideo ? "opacity-50" : ""}>iterations</span>
                      <span title="Video uses significantly more tokens. Enables after N failed iterations.">
                        <Info className="w-3.5 h-3.5 text-muted-foreground" />
                      </span>
                    </label>
                    <div className="border-l border-border pl-4 ml-2">
                      <label className="flex items-center gap-1.5 cursor-pointer">
                        <span>Max iterations:</span>
                        <input
                          type="number"
                          min={1}
                          max={50}
                          value={autoRefineMaxIterations}
                          onChange={(e) =>
                            setAutoRefineMaxIterations(
                              Math.max(1, Math.min(50, parseInt(e.target.value) || 10)),
                            )
                          }
                          className="w-12 px-1 py-0.5 text-center bg-background border border-border rounded"
                        />
                      </label>
                    </div>
                  </div>

                  <div className="flex justify-between items-center gap-2 pt-4 border-t border-border">
                    <button
                      onClick={() => {
                        setIsCreating(false);
                        resetForm();
                      }}
                      className="btn-secondary px-4 py-2"
                    >
                      Cancel
                    </button>
                    <div className="flex items-center gap-2">
                      <button
                        onClick={() => createScript()}
                        disabled={!formName || !formScriptContent}
                        className="btn-secondary px-4 py-2 flex items-center gap-2"
                      >
                        <Save className="w-4 h-4" />
                        Save Only
                      </button>
                      <button
                        onClick={() => createScript({ runTest: true })}
                        disabled={!formName || !formScriptContent}
                        className="btn-primary px-4 py-2 flex items-center gap-2"
                        title="Save and run the test once"
                      >
                        <Play className="w-4 h-4" />
                        Save & Run
                      </button>
                      <button
                        onClick={() => createScript({ autoRefine: true })}
                        disabled={!formName || !formScriptContent}
                        className="flex items-center gap-2 px-4 py-2 text-sm bg-gradient-to-r from-purple-500 to-pink-500 text-white rounded-lg hover:from-purple-600 hover:to-pink-600 disabled:opacity-50 disabled:cursor-not-allowed transition-all"
                        title="Save, run test, and refine with AI until it passes"
                      >
                        <Sparkles className="w-4 h-4" />
                        Save & Auto-Refine
                      </button>
                    </div>
                  </div>
                </div>
              )}
            </div>
          </>
        )}

        {/* Description Preview Modal */}
        {showDescriptionPreview && descriptionPreview && (
          <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50 p-4">
            <div className="bg-card border border-border rounded-lg shadow-xl max-w-2xl w-full max-h-[80vh] overflow-hidden">
              <div className="px-4 py-3 border-b border-border flex items-center justify-between">
                <h3 className="text-lg font-semibold flex items-center gap-2">
                  <Sparkles className={`w-5 h-5 ${getAccentColors("blue").text}`} />
                  Generated Description
                </h3>
                <button
                  onClick={rejectDescriptionPreview}
                  className="p-1 hover:bg-muted rounded transition-colors"
                >
                  <X className="w-5 h-5" />
                </button>
              </div>

              <div className="p-4 space-y-4 overflow-y-auto max-h-[60vh]">
                {/* Current description */}
                <div>
                  <label className="block text-sm font-medium text-muted-foreground mb-2">
                    Current Description
                  </label>
                  <div className="p-3 bg-muted/30 border border-border rounded-lg text-sm">
                    {formDescription || (
                      <span className="text-muted-foreground italic">No description</span>
                    )}
                  </div>
                </div>

                {/* Arrow */}
                <div className="flex justify-center">
                  <ChevronDown className="w-6 h-6 text-muted-foreground" />
                </div>

                {/* New description */}
                <div>
                  <label
                    className={`block text-sm font-medium ${getAccentColors("blue").text} mb-2`}
                  >
                    New Description (from code)
                  </label>
                  <div
                    className={`p-3 ${getAccentColors("blue").bg} border ${getAccentColors("blue").border} rounded-lg text-sm`}
                  >
                    {descriptionPreview}
                  </div>
                </div>
              </div>

              <div className="px-4 py-3 border-t border-border flex justify-end gap-2">
                <button
                  onClick={rejectDescriptionPreview}
                  className="px-4 py-2 text-sm bg-muted hover:bg-muted/80 rounded-lg transition-colors"
                >
                  Keep Original
                </button>
                <button
                  onClick={acceptDescriptionPreview}
                  className={`px-4 py-2 text-sm ${getAccentColors("blue").bgSolid} text-white hover:opacity-90 rounded-lg transition-colors flex items-center gap-2`}
                >
                  <CheckCircle className="w-4 h-4" />
                  Use New Description
                </button>
              </div>
            </div>
          </div>
        )}
      </div>

      {/* Batch Delete Dialog */}
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
