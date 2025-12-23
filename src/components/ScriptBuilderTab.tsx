import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  TestTube,
  Plus,
  Search,
  Play,
  Trash2,
  Copy,
  Tag,
  FolderOpen,
  Save,
  X,
  Loader2,
  Upload,
  Download,
  Code,
  FileText,
  Globe,
  Clock,
  CheckCircle,
  XCircle,
  Monitor,
  ChevronDown,
  ChevronUp,
  Sparkles,
  RefreshCw,
  Info,
  ImageIcon,
} from "lucide-react";
import type {
  PlaywrightScript,
  PlaywrightResult,
  ScriptViewMode,
  ScriptExecutionState,
  DisplayMode,
} from "../types";

type LogLevel = "info" | "warning" | "error" | "debug" | "success";

interface ScriptBuilderTabProps {
  onLog: (level: LogLevel, message: string) => void;
}

const API_BASE = "http://localhost:9876";

const DEFAULT_SCRIPT_CONTENT = `import { test, expect } from '@playwright/test';

test('example test', async ({ page }) => {
  // Navigate to the target page
  await page.goto('/');

  // Add your test assertions here
  await expect(page).toHaveTitle(/./);
});
`;

export function ScriptBuilderTab({ onLog }: ScriptBuilderTabProps) {
  // State
  const [scripts, setScripts] = useState<PlaywrightScript[]>([]);
  const [loading, setLoading] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [selectedCategory, setSelectedCategory] = useState<string | null>(null);
  const [categories, setCategories] = useState<string[]>([]);

  // Edit modal state
  const [editingScript, setEditingScript] = useState<PlaywrightScript | null>(null);
  const [isCreating, setIsCreating] = useState(false);

  // View mode toggle
  const [viewMode, setViewMode] = useState<ScriptViewMode>("natural_language");

  // Form state
  const [formName, setFormName] = useState("");
  const [formDescription, setFormDescription] = useState("");
  const [formAiInstructions, setFormAiInstructions] = useState("");
  const [formTargetUrl, setFormTargetUrl] = useState("");
  const [formScriptContent, setFormScriptContent] = useState(DEFAULT_SCRIPT_CONTENT);
  const [formCategory, setFormCategory] = useState("");
  const [formTags, setFormTags] = useState("");
  const [formTimeoutSeconds, setFormTimeoutSeconds] = useState(60);
  const [formDisplayMode, setFormDisplayMode] = useState<DisplayMode>("headless");
  const [formBrowser, setFormBrowser] = useState<"chromium" | "firefox" | "webkit">("chromium");

  // Execution state
  const [_executingScriptId, setExecutingScriptId] = useState<string | null>(null);
  const [executionState, setExecutionState] = useState<ScriptExecutionState>("idle");
  const [executionResult, setExecutionResult] = useState<PlaywrightResult | null>(null);

  // AI generation state
  const [isGenerating, setIsGenerating] = useState(false);

  // Refinement prompt for improving scripts based on test results
  const [refinementPrompt, setRefinementPrompt] = useState("");

  // Auto-refinement loop state
  const [isAutoRefining, setIsAutoRefining] = useState(false);
  const [autoRefineIteration, setAutoRefineIteration] = useState(0);
  const [autoRefineMaxIterations] = useState(10);
  const [autoRefineLog, setAutoRefineLog] = useState<string[]>([]);
  const autoRefineAbortRef = useRef(false);

  // Description regeneration state
  const [isRegeneratingDescription, setIsRegeneratingDescription] = useState(false);
  const [descriptionPreview, setDescriptionPreview] = useState<string | null>(null);
  const [showDescriptionPreview, setShowDescriptionPreview] = useState(false);

  // Track if description was manually changed (to trigger auto-save)
  const descriptionChangedByUser = useRef(false);

  // Track the description that was used to generate the current code
  // This is used to warn users if they try to run tests when description has changed
  const [lastGeneratedFromDescription, setLastGeneratedFromDescription] = useState<string | null>(
    null,
  );

  // Auto-save description when editing an existing script
  useEffect(() => {
    // Only auto-save if editing an existing script and description was changed by user
    if (!editingScript || !descriptionChangedByUser.current) {
      return;
    }

    const timeoutId = setTimeout(async () => {
      try {
        await fetch(`${API_BASE}/playwright/scripts/${editingScript.id}`, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ description: formDescription }),
        });
        // Don't show a log message for auto-save to avoid noise
      } catch (error) {
        console.error("Failed to auto-save description:", error);
      }
    }, 500); // 500ms debounce

    return () => clearTimeout(timeoutId);
  }, [formDescription, editingScript]);

  // Expanded sections in execution result
  const [expandedSpecs, setExpandedSpecs] = useState<Set<number>>(new Set());

  // Show screenshot in execution result
  const [showScreenshot, setShowScreenshot] = useState(true);
  const [screenshotDataUrl, setScreenshotDataUrl] = useState<string | null>(null);
  const [screenshotLoading, setScreenshotLoading] = useState(false);
  const [screenshotError, setScreenshotError] = useState<string | null>(null);

  // Expanded script for viewing details
  const [expandedScriptId, setExpandedScriptId] = useState<string | null>(null);

  // Load scripts on mount
  useEffect(() => {
    loadScripts();
    loadCategories();
  }, []);

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
      const response = await fetch(`${API_BASE}/playwright/scripts`);
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

  const loadCategories = async () => {
    try {
      const response = await fetch(`${API_BASE}/playwright/scripts/categories`);
      const result = await response.json();
      if (result.success) {
        setCategories(result.data || []);
      }
    } catch (error) {
      console.error("Failed to load categories:", error);
    }
  };

  // CRUD operations
  const createScript = async () => {
    try {
      const response = await fetch(`${API_BASE}/playwright/scripts`, {
        method: "POST",
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
        }),
      });
      const result = await response.json();
      if (result.success) {
        onLog("success", `Created script: ${formName}`);
        loadScripts();
        loadCategories();
        resetForm();
        setIsCreating(false);
      } else {
        onLog("error", `Failed to create script: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to create script: ${error}`);
    }
  };

  const deleteScript = async (id: string) => {
    if (!confirm("Are you sure you want to delete this script?")) return;
    try {
      const response = await fetch(`${API_BASE}/playwright/scripts/${id}`, {
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

  const duplicateScript = async (id: string) => {
    try {
      const response = await fetch(`${API_BASE}/playwright/scripts/${id}/duplicate`, {
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
      const response = await fetch(`${API_BASE}/playwright/scripts/${id}/run`, {
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
        const response = await fetch(`${API_BASE}/playwright/scripts/import`, {
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
      const response = await fetch(`${API_BASE}/playwright/scripts/export`);
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
      const response = await fetch(`${API_BASE}/trigger-ai-analysis`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          prompt,
          display_prompt: `Generate Playwright: ${formName || formDescription.substring(0, 50)}`,
          timeout_seconds: 120,
          wait_for_completion: true,
        }),
      });

      const result = await response.json();

      if (result.success && result.data?.output) {
        // Extract code from the AI response
        const output = result.data.output;

        // Try multiple extraction patterns
        let generatedCode: string | null = null;

        // Pattern 1: Code block with language tag
        const codeBlockMatch = output.match(/```(?:typescript|ts|javascript|js)?\s*([\s\S]*?)```/);
        if (codeBlockMatch && codeBlockMatch[1]) {
          generatedCode = codeBlockMatch[1].trim();
        }

        // Pattern 2: If no code block found, look for import statement and extract from there
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

        // Pattern 3: Look for test() function
        if (!generatedCode && output.includes("test(")) {
          const testIndex = output.indexOf("test(");
          generatedCode = output.substring(testIndex).trim();
        }

        if (generatedCode && generatedCode.includes("test(")) {
          setFormScriptContent(generatedCode);
          setLastGeneratedFromDescription(formDescription); // Track which description generated this code
          setViewMode("code"); // Switch to code view to show the result
          onLog("success", "Script generated successfully!");
        } else {
          onLog(
            "warning",
            "AI response didn't contain valid Playwright code. Check AI Output tab.",
          );
          console.log("AI output:", output);
        }
      } else if (result.success) {
        // AI analysis completed but no output field - this shouldn't happen now
        onLog("info", "Script generation completed. Check AI Output tab for results.");
      } else {
        onLog(
          "error",
          `Failed to generate script: ${result.error || result.data?.error || "Unknown error"}`,
        );
      }
    } catch (error) {
      onLog("error", `Failed to generate script: ${error}`);
    } finally {
      setIsGenerating(false);
    }
  };

  // Refine script based on test results
  const refineScript = async () => {
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
      const response = await fetch(`${API_BASE}/trigger-ai-analysis`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          prompt,
          display_prompt: `Refine Playwright: ${formName || "script"}`,
          timeout_seconds: 120,
          wait_for_completion: true,
        }),
      });

      const result = await response.json();

      if (result.success && result.data?.output) {
        const output = result.data.output;
        let generatedCode: string | null = null;

        // Extract code from response
        const codeBlockMatch = output.match(/```(?:typescript|ts|javascript|js)?\s*([\s\S]*?)```/);
        if (codeBlockMatch && codeBlockMatch[1]) {
          generatedCode = codeBlockMatch[1].trim();
        }

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
        onLog("error", `Failed to refine script: ${result.error || "Unknown error"}`);
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
    const log = (msg: string) => {
      setAutoRefineLog((prev) => [...prev, `[${new Date().toLocaleTimeString()}] ${msg}`]);
      onLog("info", msg);
    };

    let currentScriptContent = formScriptContent;
    let scriptId = editingScript.id;
    let iteration = 0;

    try {
      while (iteration < autoRefineMaxIterations && !autoRefineAbortRef.current) {
        iteration++;
        setAutoRefineIteration(iteration);
        log(`Iteration ${iteration}/${autoRefineMaxIterations}: Running test...`);

        // Run the test
        setExecutionState("running");
        const runResponse = await fetch(`${API_BASE}/playwright/scripts/${scriptId}/run`, {
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

        // Check if stopped by user
        if (autoRefineAbortRef.current) {
          log("Stopped by user");
          break;
        }

        if (testResult.passed) {
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

        const prompt = `Refine this Playwright test script based on the test results.

## Current Script
\`\`\`typescript
${currentScriptContent}
\`\`\`

## Test Results
Tests failed with the following errors:
${testErrors}
${pageSnapshotSection}${aiInstructionsSection}
## Requirements
1. Fix the issues identified in the test results
2. Keep the overall test structure and intent
3. Use the Page Snapshot above to find the correct element roles and names
4. Use more robust selectors if elements weren't found (check if 'link' should be 'button', etc.)
5. Add appropriate waits if there were timing issues
6. Update assertions to match actual behavior if needed

## Output Format
First, write a single line starting with "CHANGES:" that briefly describes what you changed (e.g., "CHANGES: Fixed button selector from 'link' to 'button', added waitForLoadState").
Then provide the corrected Playwright script code in a code block.

CHANGES: <your brief description here>

\`\`\`typescript
import { test, expect } from '@playwright/test';
// Your corrected code here
\`\`\``;

        // Call AI to refine
        setIsGenerating(true);
        const aiResponse = await fetch(`${API_BASE}/trigger-ai-analysis`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            prompt,
            display_prompt: `Auto-refine iteration ${iteration}: ${formName || "script"}`,
            timeout_seconds: 120,
            wait_for_completion: true,
          }),
        });
        setIsGenerating(false);

        const aiResult = await aiResponse.json();

        // Check if stopped by user
        if (autoRefineAbortRef.current) {
          log("Stopped by user");
          break;
        }

        if (!aiResult.success || !aiResult.data?.output) {
          log(`AI refinement failed: ${aiResult.error || "No output"}`);
          break;
        }

        // Extract code and changes summary from AI response
        const output = aiResult.data.output;
        let generatedCode: string | null = null;

        // Extract CHANGES summary
        const changesMatch = output.match(/CHANGES:\s*(.+?)(?:\n|$)/i);
        if (changesMatch && changesMatch[1]) {
          const changesSummary = changesMatch[1].trim();
          log(`FIX: ${changesSummary}`);
        }

        const codeBlockMatch = output.match(/```(?:typescript|ts|javascript|js)?\s*([\s\S]*?)```/);
        if (codeBlockMatch && codeBlockMatch[1]) {
          generatedCode = codeBlockMatch[1].trim();
        }

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
          break;
        }

        currentScriptContent = generatedCode;
        setFormScriptContent(generatedCode);
        log("Saving updated script...");

        // Save the updated script
        const saveResponse = await fetch(`${API_BASE}/playwright/scripts/${scriptId}`, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            script_content: generatedCode,
          }),
        });
        const saveResult = await saveResponse.json();

        if (!saveResult.success) {
          log(`Failed to save script: ${saveResult.error}`);
          break;
        }

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
  const stopAutoRefine = () => {
    autoRefineAbortRef.current = true;
    setIsAutoRefining(false);
    onLog("info", "Auto-refinement stopped by user");
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
      const saveResponse = await fetch(`${API_BASE}/playwright/scripts/${id}`, {
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
  const regenerateDescriptionFromCode = async () => {
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
      const response = await fetch(`${API_BASE}/trigger-ai-analysis`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          prompt,
          display_prompt: `Generate description for: ${formName || "script"}`,
          timeout_seconds: 60,
          wait_for_completion: true,
        }),
      });

      const result = await response.json();

      if (result.success && result.data?.output) {
        const output = result.data.output.trim();
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
        onLog("error", `Failed to generate description: ${result.error || "Unknown error"}`);
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
        await fetch(`${API_BASE}/playwright/scripts/${editingScript.id}`, {
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
    setViewMode("natural_language");
  };

  const startEditing = (script: PlaywrightScript) => {
    descriptionChangedByUser.current = false; // Reset flag when loading a script
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
    setIsCreating(false);
  };

  const startCreating = () => {
    resetForm();
    setEditingScript(null);
    setIsCreating(true);
  };

  // Filter scripts
  const filteredScripts = scripts.filter((s) => {
    const matchesSearch =
      !searchQuery ||
      s.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      s.description.toLowerCase().includes(searchQuery.toLowerCase()) ||
      s.target_url.toLowerCase().includes(searchQuery.toLowerCase()) ||
      s.tags.some((t) => t.toLowerCase().includes(searchQuery.toLowerCase()));

    const matchesCategory = !selectedCategory || s.category === selectedCategory;

    return matchesSearch && matchesCategory;
  });

  return (
    <div className="p-6 space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <TestTube className="w-6 h-6 text-green-500" />
          <h2 className="text-xl font-semibold">Playwright Script Builder</h2>
          <span className="text-sm text-muted-foreground">({scripts.length} scripts)</span>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={importScripts}
            className="btn-secondary flex items-center gap-2 px-3 py-2 text-sm"
          >
            <Upload className="w-4 h-4" />
            Import
          </button>
          <button
            onClick={exportScripts}
            className="btn-secondary flex items-center gap-2 px-3 py-2 text-sm"
          >
            <Download className="w-4 h-4" />
            Export
          </button>
          <button
            onClick={startCreating}
            className="btn-primary flex items-center gap-2 px-3 py-2 text-sm"
          >
            <Plus className="w-4 h-4" />
            New Script
          </button>
        </div>
      </div>

      {/* Search and Filter */}
      <div className="flex items-center gap-4">
        <div className="relative flex-1">
          <Search className="absolute left-3 top-1/2 transform -translate-y-1/2 w-4 h-4 text-muted-foreground" />
          <input
            type="text"
            placeholder="Search scripts..."
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            className="w-full pl-10 pr-4 py-2 bg-card border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
          />
        </div>
        <select
          value={selectedCategory || ""}
          onChange={(e) => setSelectedCategory(e.target.value || null)}
          className="px-4 py-2 bg-card border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
        >
          <option value="">All Categories</option>
          {categories.map((cat) => (
            <option key={cat} value={cat}>
              {cat}
            </option>
          ))}
        </select>
      </div>

      {/* Create New Script Form (only for new scripts) */}
      {isCreating && (
        <div className="card p-6 space-y-4 border-2 border-green-500/30">
          <div className="flex items-center justify-between">
            <h3 className="text-lg font-semibold flex items-center gap-2">
              <TestTube className="w-5 h-5 text-green-500" />
              Create New Script
            </h3>
            <div className="flex items-center gap-2">
              {/* View Mode Toggle */}
              <div className="flex items-center bg-card border border-border rounded-lg p-1">
                <button
                  onClick={() => setViewMode("natural_language")}
                  className={`px-3 py-1 text-sm rounded flex items-center gap-1 ${
                    viewMode === "natural_language"
                      ? "bg-green-500/20 text-green-500"
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
                      ? "bg-green-500/20 text-green-500"
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
                <label className="block text-sm font-medium">Description (Natural Language)</label>
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
              <textarea
                value={formDescription}
                onChange={(e) => setFormDescription(e.target.value)}
                placeholder="Describe what this test should do in plain English...&#10;&#10;Example: There is a Capture Screen button on the Image Extraction page. Click it and select Capture Screen in the dialog box."
                rows={6}
                className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-green-500/50"
              />
              <p className="text-xs text-muted-foreground mt-1">
                Describe what you want to test, then click "Generate Script" to create the
                Playwright code.
              </p>

              {/* AI Instructions (optional) */}
              <div className="mt-4">
                <label className="block text-sm font-medium mb-1 flex items-center gap-2">
                  <Sparkles className="w-4 h-4 text-purple-500" />
                  AI Instructions (Optional)
                </label>
                <textarea
                  value={formAiInstructions}
                  onChange={(e) => setFormAiInstructions(e.target.value)}
                  placeholder="Additional instructions for the AI that modify how the description is interpreted...&#10;&#10;Example: The feature is currently broken. Stop after capturing the screen and take a screenshot. Expect failure but capture the state for debugging."
                  rows={3}
                  className="w-full px-3 py-2 bg-purple-500/5 border border-purple-500/30 rounded-lg focus:outline-none focus:ring-2 focus:ring-purple-500/50 text-sm"
                />
                <p className="text-xs text-muted-foreground mt-1">
                  Use this to give the AI additional context without changing the test description.
                </p>
              </div>
            </div>
          ) : (
            <div>
              <div className="flex items-center justify-between mb-1">
                <label className="block text-sm font-medium">Script Content (.spec.ts) *</label>
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
              <label className="block text-sm font-medium mb-1">Tags (comma-separated)</label>
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
                    <p className="text-xs text-amber-500">
                      Requires Chrome started with: --remote-debugging-port=9222
                    </p>
                    <div className="relative group">
                      <Info className="w-3.5 h-3.5 text-amber-500 cursor-help" />
                      <div className="absolute right-0 bottom-full mb-2 hidden group-hover:block z-50 w-80">
                        <div className="bg-popover border border-border rounded-lg shadow-lg p-3 text-xs">
                          <p className="font-medium mb-2">How to start Chrome with debugging:</p>
                          <ol className="list-decimal list-inside space-y-1 text-muted-foreground">
                            <li>Close all Chrome windows first</li>
                            <li>Click the button below to restart Chrome with debugging, or:</li>
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
                        const response = await fetch(`${API_BASE}/launch-debug-chrome`, {
                          method: "POST",
                        });
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
                    className="text-xs px-2 py-1 bg-amber-500/20 text-amber-500 rounded hover:bg-amber-500/30 transition-colors"
                  >
                    Restart Chrome with Debug Port
                  </button>
                </div>
              )}
            </div>
          </div>

          <div className="flex justify-end gap-2 pt-4">
            <button
              onClick={() => {
                setIsCreating(false);
                resetForm();
              }}
              className="btn-secondary px-4 py-2"
            >
              Cancel
            </button>
            <button
              onClick={createScript}
              disabled={!formName || !formScriptContent}
              className="btn-primary px-4 py-2 flex items-center gap-2"
            >
              <Save className="w-4 h-4" />
              Create Script
            </button>
          </div>
        </div>
      )}

      {/* Script List */}
      {loading ? (
        <div className="flex items-center justify-center py-12">
          <Loader2 className="w-8 h-8 animate-spin text-muted-foreground" />
        </div>
      ) : filteredScripts.length === 0 ? (
        <div className="text-center py-12 text-muted-foreground">
          <TestTube className="w-12 h-12 mx-auto mb-3 opacity-50" />
          <p>{searchQuery || selectedCategory ? "No matching scripts found" : "No scripts yet"}</p>
          <p className="text-sm">Create a new script to get started</p>
        </div>
      ) : (
        <div className="space-y-3">
          {filteredScripts.map((script) => (
            <div key={script.id} className="card p-4 hover:border-border/80 transition-colors">
              <div className="flex items-start justify-between">
                <div className="flex-1">
                  <div className="flex items-center gap-2">
                    <h3 className="font-medium">{script.name}</h3>
                    {script.last_result && (
                      <span
                        className={`px-2 py-0.5 text-xs rounded ${
                          script.last_result.passed
                            ? "bg-green-500/20 text-green-500"
                            : "bg-red-500/20 text-red-500"
                        }`}
                      >
                        {script.last_result.passed ? "Passed" : "Failed"}
                      </span>
                    )}
                    {script.category && (
                      <span className="px-2 py-0.5 text-xs bg-card rounded flex items-center gap-1">
                        <FolderOpen className="w-3 h-3" />
                        {script.category}
                      </span>
                    )}
                  </div>
                  {script.description && (
                    <p className="text-sm text-muted-foreground mt-1 line-clamp-2">
                      {script.description}
                    </p>
                  )}
                  <div className="flex items-center gap-4 mt-2 text-xs text-muted-foreground">
                    {script.target_url && (
                      <span className="flex items-center gap-1">
                        <Globe className="w-3 h-3" />
                        {script.target_url}
                      </span>
                    )}
                    <span className="flex items-center gap-1">
                      <Monitor className="w-3 h-3" />
                      {script.browser}
                    </span>
                    <span className="flex items-center gap-1">
                      <Clock className="w-3 h-3" />
                      {script.timeout_seconds}s
                    </span>
                    {script.tags.length > 0 && (
                      <span className="flex items-center gap-1">
                        <Tag className="w-3 h-3" />
                        {script.tags.join(", ")}
                      </span>
                    )}
                  </div>
                </div>
                <div className="flex items-center gap-1">
                  <button
                    onClick={() => duplicateScript(script.id)}
                    className="p-2 hover:bg-card rounded"
                    title="Duplicate"
                  >
                    <Copy className="w-4 h-4" />
                  </button>
                  <button
                    onClick={() => deleteScript(script.id)}
                    className="p-2 hover:bg-card rounded text-red-500"
                    title="Delete"
                  >
                    <Trash2 className="w-4 h-4" />
                  </button>
                  <button
                    onClick={() => {
                      if (expandedScriptId === script.id) {
                        setExpandedScriptId(null);
                        setEditingScript(null);
                        resetForm();
                      } else {
                        setExpandedScriptId(script.id);
                        startEditing(script);
                      }
                    }}
                    className="p-2 hover:bg-card rounded"
                    title={expandedScriptId === script.id ? "Collapse" : "Expand & Edit"}
                  >
                    {expandedScriptId === script.id ? (
                      <ChevronUp className="w-4 h-4" />
                    ) : (
                      <ChevronDown className="w-4 h-4" />
                    )}
                  </button>
                </div>
              </div>

              {/* Expanded edit form */}
              {expandedScriptId === script.id && editingScript?.id === script.id && (
                <div className="mt-4 pt-4 border-t border-border space-y-4">
                  {/* View Mode Toggle */}
                  <div className="flex items-center justify-between">
                    <div className="flex items-center bg-card border border-border rounded-lg p-1">
                      <button
                        onClick={() => setViewMode("natural_language")}
                        className={`px-3 py-1 text-sm rounded flex items-center gap-1 ${
                          viewMode === "natural_language"
                            ? "bg-green-500/20 text-green-500"
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
                            ? "bg-green-500/20 text-green-500"
                            : "text-muted-foreground hover:text-foreground"
                        }`}
                      >
                        <Code className="w-4 h-4" />
                        Code
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
                      <textarea
                        value={formDescription}
                        onChange={(e) => {
                          descriptionChangedByUser.current = true;
                          setFormDescription(e.target.value);
                        }}
                        placeholder="Describe what this test should do in plain English..."
                        rows={4}
                        className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-green-500/50"
                      />

                      {/* AI Instructions (optional) */}
                      <div className="mt-3">
                        <label className="block text-sm font-medium mb-1 flex items-center gap-2">
                          <Sparkles className="w-3 h-3 text-purple-500" />
                          AI Instructions (Optional)
                        </label>
                        <textarea
                          value={formAiInstructions}
                          onChange={(e) => setFormAiInstructions(e.target.value)}
                          placeholder="Additional context for the AI (e.g., 'Feature is broken - capture state for debugging')"
                          rows={2}
                          className="w-full px-3 py-2 bg-purple-500/5 border border-purple-500/30 rounded-lg focus:outline-none focus:ring-2 focus:ring-purple-500/50 text-sm"
                        />
                      </div>
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
                        rows={10}
                        className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-green-500/50 font-mono text-sm"
                        spellCheck={false}
                      />
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
                        list="category-suggestions-inline"
                      />
                      <datalist id="category-suggestions-inline">
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
                            <p className="text-xs text-amber-500">
                              Requires Chrome with --remote-debugging-port=9222
                            </p>
                            <div className="relative group">
                              <Info className="w-3.5 h-3.5 text-amber-500 cursor-help" />
                              <div className="absolute right-0 bottom-full mb-2 hidden group-hover:block z-50 w-80">
                                <div className="bg-popover border border-border rounded-lg shadow-lg p-3 text-xs">
                                  <p className="font-medium mb-2">
                                    How to start Chrome with debugging:
                                  </p>
                                  <ol className="list-decimal list-inside space-y-1 text-muted-foreground">
                                    <li>Close all Chrome windows first</li>
                                    <li>Click the button below to restart Chrome with debugging</li>
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
                                const response = await fetch(`${API_BASE}/launch-debug-chrome`, {
                                  method: "POST",
                                });
                                const result = await response.json();
                                if (result.success) {
                                  onLog("success", "Chrome launched with debugging enabled.");
                                } else {
                                  onLog("error", `Failed to launch Chrome: ${result.error}`);
                                }
                              } catch (error) {
                                onLog("error", `Failed to launch Chrome: ${error}`);
                              }
                            }}
                            className="text-xs px-2 py-1 bg-amber-500/20 text-amber-500 rounded hover:bg-amber-500/30 transition-colors"
                          >
                            Restart Chrome with Debug Port
                          </button>
                        </div>
                      )}
                    </div>
                  </div>

                  {/* Auto-refinement progress */}
                  {isAutoRefining && (
                    <div className="p-4 bg-purple-500/10 border-2 border-purple-500/30 rounded-lg">
                      <div className="flex items-center justify-between mb-3">
                        <div className="flex items-center gap-2">
                          <Loader2 className="w-5 h-5 animate-spin text-purple-500" />
                          <span className="text-lg font-medium text-purple-500">
                            Auto-Refining: Iteration {autoRefineIteration}/{autoRefineMaxIterations}
                          </span>
                        </div>
                        <button
                          onClick={stopAutoRefine}
                          className="px-3 py-1.5 text-sm bg-red-500 text-white rounded-lg hover:bg-red-600 transition-colors"
                        >
                          Stop
                        </button>
                      </div>
                      {autoRefineLog.length > 0 && (
                        <div className="bg-background rounded-lg p-3 max-h-40 overflow-y-auto">
                          {autoRefineLog.map((line, i) => (
                            <div key={i} className="text-sm text-muted-foreground font-mono">
                              {line}
                            </div>
                          ))}
                        </div>
                      )}
                    </div>
                  )}

                  {/* Execution Result */}
                  {(executionState === "running" || executionResult) &&
                    editingScript?.id === script.id && (
                      <div
                        className={`p-4 rounded-lg border-2 ${
                          executionState === "running"
                            ? "border-blue-500/50 bg-blue-500/5"
                            : executionResult?.passed
                              ? "border-green-500/50 bg-green-500/5"
                              : "border-red-500/50 bg-red-500/5"
                        }`}
                      >
                        <div className="flex items-center justify-between mb-3">
                          <h4 className="font-medium flex items-center gap-2">
                            {executionState === "running" ? (
                              <>
                                <Loader2 className="w-4 h-4 animate-spin text-blue-500" />
                                Running Test...
                              </>
                            ) : executionResult?.passed ? (
                              <>
                                <CheckCircle className="w-4 h-4 text-green-500" />
                                Test Passed
                              </>
                            ) : (
                              <>
                                <XCircle className="w-4 h-4 text-red-500" />
                                Test Failed
                              </>
                            )}
                          </h4>
                          {executionResult && (
                            <button
                              onClick={() => {
                                setExecutionResult(null);
                                setExpandedSpecs(new Set());
                              }}
                              className="p-1 hover:bg-card rounded"
                            >
                              <X className="w-4 h-4" />
                            </button>
                          )}
                        </div>

                        {executionResult && (
                          <>
                            <div className="grid grid-cols-4 gap-3 mb-3">
                              <div className="text-center p-2 bg-background rounded-lg">
                                <div className="text-xl font-bold text-green-500">
                                  {executionResult.tests_passed}
                                </div>
                                <div className="text-xs text-muted-foreground">Passed</div>
                              </div>
                              <div className="text-center p-2 bg-background rounded-lg">
                                <div className="text-xl font-bold text-red-500">
                                  {executionResult.tests_failed}
                                </div>
                                <div className="text-xs text-muted-foreground">Failed</div>
                              </div>
                              <div className="text-center p-2 bg-background rounded-lg">
                                <div className="text-xl font-bold text-muted-foreground">
                                  {executionResult.tests_skipped}
                                </div>
                                <div className="text-xs text-muted-foreground">Skipped</div>
                              </div>
                              <div className="text-center p-2 bg-background rounded-lg">
                                <div className="text-xl font-bold">
                                  {(executionResult.duration_ms / 1000).toFixed(1)}s
                                </div>
                                <div className="text-xs text-muted-foreground">Duration</div>
                              </div>
                            </div>

                            {executionResult.structured_output?.specs &&
                              executionResult.structured_output.specs.length > 0 && (
                                <div className="mb-3 space-y-2">
                                  <div className="text-sm font-medium mb-2">Test Results</div>
                                  {executionResult.structured_output.specs.map((spec, index) => (
                                    <div
                                      key={index}
                                      className={`border rounded-lg overflow-hidden ${
                                        spec.status === "expected"
                                          ? "border-green-500/30 bg-green-500/5"
                                          : "border-red-500/30 bg-red-500/5"
                                      }`}
                                    >
                                      <button
                                        onClick={() => {
                                          const newExpanded = new Set(expandedSpecs);
                                          if (newExpanded.has(index)) {
                                            newExpanded.delete(index);
                                          } else {
                                            newExpanded.add(index);
                                          }
                                          setExpandedSpecs(newExpanded);
                                        }}
                                        className="w-full px-3 py-2 flex items-center justify-between text-left hover:bg-black/5"
                                      >
                                        <div className="flex items-center gap-2">
                                          {spec.status === "expected" ? (
                                            <CheckCircle className="w-4 h-4 text-green-500 flex-shrink-0" />
                                          ) : (
                                            <XCircle className="w-4 h-4 text-red-500 flex-shrink-0" />
                                          )}
                                          <span className="text-sm font-medium truncate">
                                            {spec.title}
                                          </span>
                                        </div>
                                        <div className="flex items-center gap-2">
                                          <span className="text-xs text-muted-foreground">
                                            {(spec.duration_ms / 1000).toFixed(1)}s
                                          </span>
                                          {spec.error &&
                                            (expandedSpecs.has(index) ? (
                                              <ChevronUp className="w-4 h-4" />
                                            ) : (
                                              <ChevronDown className="w-4 h-4" />
                                            ))}
                                        </div>
                                      </button>
                                      {spec.error && expandedSpecs.has(index) && (
                                        <div className="px-3 py-2 border-t border-red-500/20 bg-red-500/5">
                                          <pre className="text-xs text-red-400 whitespace-pre-wrap font-mono max-h-48 overflow-auto">
                                            {spec.error}
                                          </pre>
                                        </div>
                                      )}
                                    </div>
                                  ))}
                                </div>
                              )}

                            {executionResult.error &&
                              (!executionResult.structured_output?.specs ||
                                executionResult.structured_output.specs.length === 0) && (
                                <div className="bg-red-500/10 border border-red-500/30 rounded-lg p-3 mb-3">
                                  <div className="text-sm font-medium text-red-500 mb-1">Error</div>
                                  <pre className="text-xs text-red-400 whitespace-pre-wrap font-mono max-h-48 overflow-auto">
                                    {executionResult.error}
                                  </pre>
                                </div>
                              )}

                            {/* Screenshot Display */}
                            {executionResult.screenshots &&
                              executionResult.screenshots.length > 0 && (
                                <div className="mb-3">
                                  <button
                                    onClick={() => setShowScreenshot(!showScreenshot)}
                                    className="flex items-center gap-2 text-sm font-medium mb-2 hover:text-primary transition-colors"
                                  >
                                    <ImageIcon className="w-4 h-4" />
                                    Last Screenshot ({executionResult.screenshots.length} total)
                                    {showScreenshot ? (
                                      <ChevronUp className="w-4 h-4" />
                                    ) : (
                                      <ChevronDown className="w-4 h-4" />
                                    )}
                                  </button>
                                  {showScreenshot && (
                                    <div className="border border-border rounded-lg overflow-hidden bg-background">
                                      <div className="relative min-h-[100px]">
                                        {screenshotLoading ? (
                                          <div className="flex items-center justify-center p-8">
                                            <Loader2 className="w-8 h-8 animate-spin text-muted-foreground" />
                                          </div>
                                        ) : screenshotError ? (
                                          <div className="p-4 text-center text-muted-foreground text-sm">
                                            <ImageIcon className="w-8 h-8 mx-auto mb-2 opacity-50" />
                                            Failed to load screenshot
                                            <div className="text-xs mt-1 text-red-400">
                                              {screenshotError}
                                            </div>
                                          </div>
                                        ) : screenshotDataUrl ? (
                                          <img
                                            src={screenshotDataUrl}
                                            alt="Last test screenshot"
                                            className="w-full h-auto max-h-96 object-contain"
                                          />
                                        ) : (
                                          <div className="p-4 text-center text-muted-foreground text-sm">
                                            <ImageIcon className="w-8 h-8 mx-auto mb-2 opacity-50" />
                                            No screenshot available
                                          </div>
                                        )}
                                      </div>
                                      <div className="px-3 py-2 bg-muted/30 border-t border-border flex items-center justify-between text-xs text-muted-foreground">
                                        <span
                                          className="truncate flex-1 mr-2"
                                          title={
                                            executionResult.screenshots[
                                              executionResult.screenshots.length - 1
                                            ]
                                          }
                                        >
                                          {executionResult.screenshots[
                                            executionResult.screenshots.length - 1
                                          ]
                                            .split(/[/\\]/)
                                            .pop()}
                                        </span>
                                        <button
                                          onClick={async () => {
                                            const path =
                                              executionResult.screenshots[
                                                executionResult.screenshots.length - 1
                                              ];
                                            await navigator.clipboard.writeText(path);
                                            onLog("info", "Screenshot path copied to clipboard");
                                          }}
                                          className="flex items-center gap-1 px-2 py-1 hover:bg-muted rounded transition-colors"
                                          title="Copy path to clipboard"
                                        >
                                          <Copy className="w-3 h-3" />
                                          Copy Path
                                        </button>
                                      </div>
                                    </div>
                                  )}
                                </div>
                              )}

                            {/* Manual Refinement */}
                            {!executionResult.passed && !isAutoRefining && (
                              <div className="border-t border-border pt-3 mt-3">
                                <div className="text-sm font-medium mb-2 flex items-center gap-1 text-foreground">
                                  <Sparkles className="w-4 h-4 text-purple-500" />
                                  Manual Refinement
                                </div>
                                <div className="flex gap-2">
                                  <input
                                    type="text"
                                    value={refinementPrompt}
                                    onChange={(e) => setRefinementPrompt(e.target.value)}
                                    placeholder="Optional: describe how to fix the test"
                                    className="flex-1 px-3 py-2 bg-background border border-border rounded-lg text-foreground placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-purple-500/50 text-sm"
                                    onKeyDown={(e) => {
                                      if (e.key === "Enter" && !isGenerating) {
                                        refineScript();
                                      }
                                    }}
                                  />
                                  <button
                                    onClick={refineScript}
                                    disabled={isGenerating}
                                    className="flex items-center gap-2 px-3 py-2 bg-purple-500 text-white rounded-lg hover:bg-purple-600 disabled:opacity-50 disabled:cursor-not-allowed transition-colors text-sm"
                                  >
                                    {isGenerating ? (
                                      <>
                                        <Loader2 className="w-4 h-4 animate-spin" />
                                        Refining...
                                      </>
                                    ) : (
                                      <>
                                        <RefreshCw className="w-4 h-4" />
                                        Refine Once
                                      </>
                                    )}
                                  </button>
                                </div>
                              </div>
                            )}
                          </>
                        )}
                      </div>
                    )}

                  {/* Action buttons */}
                  <div className="flex justify-between gap-2 pt-2">
                    <div className="flex items-center gap-2">
                      <button
                        onClick={() => saveAndRunScript(script.id)}
                        disabled={
                          executionState === "running" ||
                          isAutoRefining ||
                          !formName ||
                          !formScriptContent
                        }
                        className="flex items-center gap-2 px-4 py-2 bg-green-500 text-white rounded-lg hover:bg-green-600 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                        title="Save changes and run this test"
                      >
                        {executionState === "running" && !isAutoRefining ? (
                          <>
                            <Loader2 className="w-4 h-4 animate-spin" />
                            Running...
                          </>
                        ) : (
                          <>
                            <Play className="w-4 h-4" />
                            Run Test
                          </>
                        )}
                      </button>
                      <button
                        onClick={runAutoRefinementLoop}
                        disabled={executionState === "running" || isAutoRefining || isGenerating}
                        className="flex items-center gap-2 px-4 py-2 bg-gradient-to-r from-purple-500 to-pink-500 text-white rounded-lg hover:from-purple-600 hover:to-pink-600 disabled:opacity-50 disabled:cursor-not-allowed transition-all"
                        title="Run test, refine with AI on failure, repeat until pass"
                      >
                        {isAutoRefining ? (
                          <>
                            <Loader2 className="w-4 h-4 animate-spin" />
                            Auto-Refining...
                          </>
                        ) : (
                          <>
                            <Sparkles className="w-4 h-4" />
                            Auto-Refine Until Pass
                          </>
                        )}
                      </button>
                      {isAutoRefining && (
                        <button
                          onClick={stopAutoRefine}
                          className="flex items-center gap-2 px-3 py-2 bg-red-500 text-white rounded-lg hover:bg-red-600 transition-colors"
                        >
                          <X className="w-4 h-4" />
                          Stop
                        </button>
                      )}
                    </div>
                    {/* Regenerate description from code button */}
                    <button
                      onClick={regenerateDescriptionFromCode}
                      disabled={
                        isRegeneratingDescription || isAutoRefining || executionState === "running"
                      }
                      className="flex items-center gap-2 px-4 py-2 bg-blue-500 text-white rounded-lg hover:bg-blue-600 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                      title="Use AI to analyze the code and replace the description with what the code actually does"
                    >
                      {isRegeneratingDescription ? (
                        <>
                          <Loader2 className="w-4 h-4 animate-spin" />
                          Analyzing Code...
                        </>
                      ) : (
                        <>
                          <Sparkles className="w-4 h-4" />
                          Describe Code
                        </>
                      )}
                    </button>
                  </div>
                </div>
              )}
            </div>
          ))}
        </div>
      )}

      {/* Description Preview Modal */}
      {showDescriptionPreview && descriptionPreview && (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50 p-4">
          <div className="bg-card border border-border rounded-lg shadow-xl max-w-2xl w-full max-h-[80vh] overflow-hidden">
            <div className="px-4 py-3 border-b border-border flex items-center justify-between">
              <h3 className="text-lg font-semibold flex items-center gap-2">
                <Sparkles className="w-5 h-5 text-blue-500" />
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
                <label className="block text-sm font-medium text-blue-500 mb-2">
                  New Description (from code)
                </label>
                <div className="p-3 bg-blue-500/10 border border-blue-500/30 rounded-lg text-sm">
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
                className="px-4 py-2 text-sm bg-blue-500 text-white hover:bg-blue-600 rounded-lg transition-colors flex items-center gap-2"
              >
                <CheckCircle className="w-4 h-4" />
                Use New Description
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
