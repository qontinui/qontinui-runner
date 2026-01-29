/**
 * AI Test Generator Component
 *
 * Generates test code from natural language descriptions using Claude.
 * Uses page analysis data and step outputs to understand element references in prompts.
 * Supports the modular step output piping system for flexible test generation.
 */

import { useState, useCallback, useMemo, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Sparkles, Loader2, RefreshCw, Check, X, Copy, Wand2, Info, Upload, FileText, Image as ImageIcon, Database, ChevronDown } from "lucide-react";
import type { PageAnalysis, TestType, MultiRequestAnalysis, CollectedAnalysisSet, ReferenceDocument, ReferenceDocumentType, TaskRunSummary, WorkflowRunContext } from "./types";
import { logRender } from "../../utils/renderLogger";
import { getStepOutputFromAnalysis } from "./types";
import {
  stepOutputRegistry,
  collectAssertableFields,
  type AssertableField,
} from "../../lib/step-output-handlers";

interface GenerateTestResponse {
  success: boolean;
  code?: string;
  error?: string;
}

interface AiTestGeneratorProps {
  analysis: PageAnalysis | null;
  multiRequestAnalysis?: MultiRequestAnalysis | null;
  collectedAnalyses?: CollectedAnalysisSet | null;
  testType: TestType;
  onTestGenerated: (code: string) => void;
  onCancel?: () => void;
}

// Template prompts for quick start
const PROMPT_TEMPLATES = [
  {
    label: "Verify element visible",
    prompt: "Verify that the {element} is visible on the page",
  },
  {
    label: "Click and verify",
    prompt: "Click on {element} and verify the expected response",
  },
  {
    label: "Form submission",
    prompt: "Fill in the form fields and submit, then verify success",
  },
  {
    label: "Navigation test",
    prompt: "Navigate to {page} and verify the page loads correctly",
  },
];

// Extended templates for Python script verification (shown when test_type is python_script)
const PYTHON_SCRIPT_TEMPLATES = [
  {
    label: "Chained API Requests",
    prompt: `Generate a Python test that chains multiple API requests.

Look at the collected step outputs above. The test should:
1. Execute the first request (POST/PUT) using the URL and body from the step output
2. Extract any ID or reference from the response (examine the actual response structure in the step output to find the correct field path)
3. Execute the second request (GET), replacing any ID in the URL path with the value extracted from step 1
4. Parse the final response and run verification assertions

Use the 'requests' library. Copy headers from the step outputs if auth is needed.
Include error handling and logging.

My verification requirement:`,
  },
  {
    label: "Async Operation Test",
    prompt: `Generate a Python test for an async operation that requires polling.

Look at the collected step outputs. The workflow is:
1. First request STARTS an operation and returns a reference ID
2. Second request FETCHES the results using that ID

The test should:
1. Execute the start request (use URL/body from step output)
2. Extract the ID from the response (look at the actual response structure in step output)
3. Poll or wait for the operation to complete (retry with delays)
4. Fetch the results using the extracted ID
5. Verify the results

Use 'requests' library. Handle auth headers if present.

Verify that:`,
  },
  {
    label: "API Response Validation",
    prompt: `Verify the API response structure and data integrity.
The _api_response variable contains the piped API response with fields:
- _api_response["status_code"] - HTTP status
- _api_response["json"] - parsed response body

Validate required fields exist and check data types are correct.`,
  },
  {
    label: "Web Extraction State Verification",
    prompt: `Verify the web extraction state discovery results.

The API returns state extraction data with this structure:
- states: list of discovered states, each with images and screens
- annotated_screenshots: screenshots with bounding boxes around detected elements

Verification rules:
1. NO DUPLICATE IMAGES ACROSS STATES: Each image should appear in exactly one state. If image "Docs" appears in state1 AND state2, that's a bug. Check that image IDs are unique across all states.

2. VALID BOUNDING BOXES: Every bounding box should correspond to an actual clickable element. Check that bounding boxes have reasonable dimensions (width > 0, height > 0, within screen bounds).

3. STATE GROUPING LOGIC: Images that appear together on the same screens should be grouped in the same state. If images A and B both appear on screens [1,2,3], they should be in the same state.

4. NO ORPHAN IMAGES: Every detected image should be assigned to exactly one state.

Parse the response, check these rules, and report specific violations with image IDs and state names.`,
  },
  {
    label: "Check No Duplicates",
    prompt: `Check that no items are duplicated across categories/groups in the response.
Parse LAST_API_RESPONSE, build a map of item IDs to their parent groups, flag any item appearing in multiple groups.`,
  },
  {
    label: "Bounding Box Validation",
    prompt: `Validate all bounding boxes in the response are valid.
Check: x >= 0, y >= 0, width > 0, height > 0, coordinates within reasonable screen bounds (e.g., 0-4000px).
Report any invalid bounding boxes with their element IDs.`,
  },
];

export function AiTestGenerator({
  analysis,
  multiRequestAnalysis,
  collectedAnalyses,
  testType,
  onTestGenerated,
  onCancel,
}: AiTestGeneratorProps) {
  // Debug: Log component mount
  console.log("[AiTestGenerator] Component rendering, testType:", testType);

  const [prompt, setPrompt] = useState("");
  const [isGenerating, setIsGenerating] = useState(false);
  const [generatedCode, setGeneratedCode] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [referenceDocuments, setReferenceDocuments] = useState<ReferenceDocument[]>([]);
  const fileInputRef = useRef<HTMLInputElement>(null);

  // Workflow run context state
  const [recentTaskRuns, setRecentTaskRuns] = useState<TaskRunSummary[]>([]);
  const [selectedTaskRunId, setSelectedTaskRunId] = useState<string | null>(null);
  const [workflowRunContext, setWorkflowRunContext] = useState<WorkflowRunContext | null>(null);
  const [isLoadingContext, setIsLoadingContext] = useState(false);
  const [showWorkflowSelector, setShowWorkflowSelector] = useState(false);

  // Fetch recent task runs on mount
  useEffect(() => {
    const fetchRecentRuns = async () => {
      try {
        console.log("[AiTestGenerator] Fetching recent task runs...");
        const response = await invoke<{ success: boolean; data?: TaskRunSummary[]; message?: string }>(
          "list_recent_task_runs",
          { limit: 20 }
        );
        console.log("[AiTestGenerator] Response:", response);
        if (response.success && response.data) {
          console.log("[AiTestGenerator] Setting recentTaskRuns:", response.data.length, "runs");
          setRecentTaskRuns(response.data);
          // Log render with dropdown state
          logRender("AiTestGenerator", "task_runs_loaded", {
            task_run_count: response.data.length,
            task_runs: response.data.map(r => ({
              id: r.id,
              task_name: r.task_name,
              status: r.status,
              created_at: r.created_at,
            })),
            dropdown_has_options: response.data.length > 0,
          });
        } else {
          console.warn("[AiTestGenerator] No data in response or not successful:", response.message);
          // Log render with empty state
          logRender("AiTestGenerator", "task_runs_empty", {
            task_run_count: 0,
            success: response.success,
            message: response.message,
            dropdown_has_options: false,
          });
        }
      } catch (err) {
        console.error("[AiTestGenerator] Failed to fetch recent task runs:", err);
        // Log render with error state
        logRender("AiTestGenerator", "task_runs_error", {
          error: String(err),
          dropdown_has_options: false,
        });
      }
    };
    fetchRecentRuns();
  }, []);

  // Fetch full context when a task run is selected
  useEffect(() => {
    if (!selectedTaskRunId) {
      setWorkflowRunContext(null);
      return;
    }

    const fetchContext = async () => {
      setIsLoadingContext(true);
      try {
        const response = await invoke<{ success: boolean; data?: WorkflowRunContext }>(
          "get_workflow_run_context",
          { taskRunId: selectedTaskRunId }
        );
        if (response.success && response.data) {
          setWorkflowRunContext(response.data);
        }
      } catch (err) {
        console.error("Failed to fetch workflow context:", err);
      } finally {
        setIsLoadingContext(false);
      }
    };
    fetchContext();
  }, [selectedTaskRunId]);


  // Check if we have any analysis data - used for conditional rendering below
  const _hasAnalysis = analysis || multiRequestAnalysis || collectedAnalyses;

  // Handle file upload for reference documents
  const handleFileUpload = useCallback(async (event: React.ChangeEvent<HTMLInputElement>) => {
    const files = event.target.files;
    if (!files || files.length === 0) return;

    const newDocs: ReferenceDocument[] = [];

    for (const file of Array.from(files)) {
      const extension = file.name.split('.').pop()?.toLowerCase();
      let docType: ReferenceDocumentType;

      // Determine document type from extension
      if (extension === 'md' || extension === 'markdown') {
        docType = 'markdown';
      } else if (extension === 'json') {
        docType = 'json';
      } else if (['png', 'jpg', 'jpeg', 'gif', 'webp'].includes(extension || '')) {
        docType = 'image';
      } else {
        docType = 'text';
      }

      try {
        let content: string;

        if (docType === 'image') {
          // Read image as base64
          content = await new Promise<string>((resolve, reject) => {
            const reader = new FileReader();
            reader.onload = () => {
              const result = reader.result as string;
              // Extract base64 part (remove data:image/...;base64, prefix)
              const base64 = result.split(',')[1] || result;
              resolve(base64);
            };
            reader.onerror = reject;
            reader.readAsDataURL(file);
          });
        } else {
          // Read text content
          content = await file.text();
        }

        newDocs.push({
          id: `doc-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`,
          name: file.name.replace(/\.[^/.]+$/, ''), // Remove extension for display
          type: docType,
          content,
          filename: file.name,
          size: file.size,
        });
      } catch (err) {
        console.error(`Failed to read file ${file.name}:`, err);
      }
    }

    setReferenceDocuments(prev => [...prev, ...newDocs]);

    // Reset file input
    if (fileInputRef.current) {
      fileInputRef.current.value = '';
    }
  }, []);

  // Remove a reference document
  const handleRemoveDocument = useCallback((docId: string) => {
    setReferenceDocuments(prev => prev.filter(d => d.id !== docId));
  }, []);

  // Build element summary for the prompt (single page analysis)
  const elementSummary = useMemo(() => {
    if (!analysis) return "";

    const lines = analysis.elements.slice(0, 20).map((el, idx) => {
      const selector = el.selector || `(${el.bounding_box.x}, ${el.bounding_box.y})`;
      return `${idx + 1}. ${el.label} (${el.element_type}) - ${el.text_content || "no text"} [${selector}]`;
    });

    return lines.join("\n");
  }, [analysis]);

  // Build multi-request summary for display
  const _multiRequestSummary = useMemo(() => {
    if (!multiRequestAnalysis) return "";

    const successfulRequests = multiRequestAnalysis.requests.filter((r) => r.status === "complete");
    return `${successfulRequests.length} API responses collected (${multiRequestAnalysis.total_elements ?? 0} total elements)`;
  }, [multiRequestAnalysis]);

  // Build collected analyses summary for display (including step outputs)
  const collectedSummary = useMemo(() => {
    if (!collectedAnalyses) return "";

    const playwrightCount = collectedAnalyses.analyses.filter(
      (a) => a.type === "playwright",
    ).length;
    const visionCount = collectedAnalyses.analyses.filter((a) => a.type === "vision").length;
    const apiCount = collectedAnalyses.analyses.filter((a) => a.type === "api_request").length;
    const stepOutputCount = collectedAnalyses.analyses.filter(
      (a) => a.type === "step_output",
    ).length;

    const parts: string[] = [];
    if (playwrightCount > 0) parts.push(`${playwrightCount} Playwright`);
    if (visionCount > 0) parts.push(`${visionCount} Vision`);
    if (apiCount > 0) parts.push(`${apiCount} API`);
    if (stepOutputCount > 0) parts.push(`${stepOutputCount} Step Outputs`);

    return `${collectedAnalyses.analyses.length} analyses (${parts.join(", ")})`;
  }, [collectedAnalyses]);

  // Extract assertable fields from step outputs
  const assertableFields = useMemo((): AssertableField[] => {
    if (!collectedAnalyses) return [];

    const stepOutputs = collectedAnalyses.analyses
      .map((a) => getStepOutputFromAnalysis(a))
      .filter((o): o is NonNullable<typeof o> => o !== undefined);

    return collectAssertableFields(stepOutputs);
  }, [collectedAnalyses]);

  // Build step output details for display
  const stepOutputDetails = useMemo(() => {
    if (!collectedAnalyses) return [];

    return collectedAnalyses.analyses
      .map((analysis) => {
        const output = getStepOutputFromAnalysis(analysis);
        if (!output) return null;

        const handler = stepOutputRegistry.get(output.step_type);
        return {
          id: output.id,
          name: output.step_name,
          type: handler?.displayName || output.step_type,
          success: output.success,
          duration: output.duration_ms,
        };
      })
      .filter((d): d is NonNullable<typeof d> => d !== null);
  }, [collectedAnalyses]);

  // Generate test using configured AI provider
  const handleGenerate = useCallback(async () => {
    if (!prompt.trim()) {
      setError("Please enter a test description");
      return;
    }

    setIsGenerating(true);
    setError(null);
    setGeneratedCode(null);

    try {
      // Call the backend to generate test code using AI
      const response = await invoke<{
        success: boolean;
        message?: string;
        data?: GenerateTestResponse;
      }>("generate_test_with_ai", {
        input: {
          user_prompt: prompt,
          test_type: testType,
          page_analysis: analysis
            ? {
                screenshot_base64: analysis.screenshot_base64,
                elements: analysis.elements,
                source: analysis.source,
                url: analysis.url,
              }
            : null,
          multi_request_analysis: multiRequestAnalysis
            ? {
                requests: multiRequestAnalysis.requests.map((r) => ({
                  id: r.id,
                  name: r.name,
                  method: r.method,
                  endpoint: r.endpoint,
                  body: r.body,
                  status: r.status,
                  response: r.response,
                  error: r.error,
                  duration_ms: r.duration_ms,
                })),
                total_elements: multiRequestAnalysis.total_elements,
                collected_at: multiRequestAnalysis.collected_at,
              }
            : null,
          collected_analyses: collectedAnalyses
            ? {
                analyses: collectedAnalyses.analyses.map((a) => ({
                  type: a.type,
                  id: a.id,
                  name: a.name,
                  data: a.data,
                })),
                collected_at: collectedAnalyses.collected_at,
              }
            : null,
          reference_documents: referenceDocuments.length > 0
            ? referenceDocuments.map((d) => ({
                id: d.id,
                name: d.name,
                type: d.type,
                content: d.content,
                filename: d.filename,
              }))
            : null,
          workflow_run_context: workflowRunContext,
        },
      });

      if (response.success && response.data?.code) {
        setGeneratedCode(response.data.code);
      } else {
        setError(response.data?.error || response.message || "Failed to generate test");
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setIsGenerating(false);
    }
  }, [prompt, analysis, multiRequestAnalysis, collectedAnalyses, referenceDocuments, workflowRunContext, testType]);

  // Accept generated code
  const handleAccept = useCallback(() => {
    if (generatedCode) {
      onTestGenerated(generatedCode);
    }
  }, [generatedCode, onTestGenerated]);

  // Copy to clipboard
  const handleCopy = useCallback(async () => {
    if (generatedCode) {
      await navigator.clipboard.writeText(generatedCode);
    }
  }, [generatedCode]);

  // Apply template
  const handleApplyTemplate = useCallback((template: string) => {
    setPrompt(template);
  }, []);

  // Insert assertable field reference into prompt
  const handleInsertField = useCallback((field: AssertableField) => {
    const fieldRef = `{${field.name}}`;
    setPrompt((prev) => {
      // If prompt is empty, create a basic template with the field
      if (!prev.trim()) {
        return `Verify that ${fieldRef} meets expected criteria`;
      }
      // Otherwise, append the field reference at cursor position or end
      return prev + (prev.endsWith(" ") ? "" : " ") + fieldRef;
    });
  }, []);

  return (
    <div className="flex flex-col h-full bg-neutral-900 rounded-lg border border-neutral-700 overflow-hidden">
      {/* Header */}
      <div className="flex items-center justify-between p-3 border-b border-neutral-700">
        <div className="flex items-center gap-2">
          <Sparkles className="w-4 h-4 text-purple-400" />
          <span className="text-sm font-medium text-neutral-200">AI Test Generator</span>
        </div>
        {onCancel && (
          <button
            onClick={onCancel}
            className="p-1 text-neutral-500 hover:text-neutral-300 transition-colors"
          >
            <X className="w-4 h-4" />
          </button>
        )}
      </div>

      {/* Main content */}
      <div className="flex-1 flex flex-col min-h-0 overflow-hidden p-4">
        {/* Template quick picks */}
        {!generatedCode && (
          <div className="mb-4">
            <label className="block text-xs text-neutral-400 mb-2">Quick Templates</label>
            <div className="flex flex-wrap gap-2">
              {PROMPT_TEMPLATES.map((template) => (
                <button
                  key={template.label}
                  onClick={() => handleApplyTemplate(template.prompt)}
                  className="px-2 py-1 text-xs bg-neutral-800 text-neutral-300 rounded hover:bg-neutral-700 transition-colors"
                >
                  {template.label}
                </button>
              ))}
            </div>
            {/* Python-specific templates */}
            {testType === "python_script" && (
              <>
                <label className="block text-xs text-neutral-400 mb-2 mt-3">
                  Python Verification Templates
                </label>
                <div className="flex flex-wrap gap-2">
                  {PYTHON_SCRIPT_TEMPLATES.map((template) => (
                    <button
                      key={template.label}
                      onClick={() => handleApplyTemplate(template.prompt)}
                      className="px-2 py-1 text-xs bg-purple-900/50 text-purple-300 rounded hover:bg-purple-800/50 transition-colors border border-purple-700/50"
                    >
                      {template.label}
                    </button>
                  ))}
                </div>
              </>
            )}
          </div>
        )}

        {/* Prompt input or generated code */}
        {!generatedCode ? (
          <>
            {/* Prompt textarea with document upload */}
            <div className="flex-1 flex flex-col min-h-0">
              <div className="flex items-center justify-between mb-2">
                <label className="text-xs text-neutral-400">
                  Describe what you want to test
                </label>
                {/* Document upload button */}
                <div className="flex items-center gap-2">
                  <input
                    ref={fileInputRef}
                    type="file"
                    multiple
                    accept=".md,.markdown,.txt,.json,.png,.jpg,.jpeg,.gif,.webp"
                    onChange={handleFileUpload}
                    className="hidden"
                  />
                  <button
                    onClick={() => fileInputRef.current?.click()}
                    className="flex items-center gap-1 px-2 py-1 text-xs bg-neutral-800 text-neutral-300 rounded hover:bg-neutral-700 transition-colors border border-neutral-700"
                    title="Upload specs document, expected data, or reference images"
                    data-ui-id="test-builder-ai-upload-specs-btn"
                  >
                    <Upload className="w-3 h-3" />
                    Add Specs
                  </button>
                </div>
              </div>
              <textarea
                value={prompt}
                onChange={(e) => setPrompt(e.target.value)}
                placeholder="Example: Click the 'Submit' button and verify a success message appears"
                className="flex-1 px-3 py-2 bg-neutral-800 border border-neutral-700 rounded text-sm text-neutral-200 placeholder-neutral-500 resize-none focus:outline-none focus:border-purple-500"
                data-ui-id="test-builder-ai-prompt-input"
              />
            </div>

            {/* Uploaded Reference Documents */}
            {referenceDocuments.length > 0 && (
              <div className="mt-3">
                <label className="block text-xs text-neutral-400 mb-2">
                  Reference Documents ({referenceDocuments.length})
                </label>
                <div className="flex flex-wrap gap-2">
                  {referenceDocuments.map((doc) => (
                    <div
                      key={doc.id}
                      className="flex items-center gap-1.5 px-2 py-1 bg-neutral-800 border border-neutral-700 rounded text-xs"
                    >
                      {doc.type === 'image' ? (
                        <ImageIcon className="w-3 h-3 text-blue-400" />
                      ) : (
                        <FileText className="w-3 h-3 text-green-400" />
                      )}
                      <span className="text-neutral-300 max-w-24 truncate" title={doc.filename}>
                        {doc.name}
                      </span>
                      <span className="text-neutral-500">
                        ({doc.type})
                      </span>
                      <button
                        onClick={() => handleRemoveDocument(doc.id)}
                        className="p-0.5 text-neutral-500 hover:text-red-400 transition-colors"
                        title="Remove document"
                      >
                        <X className="w-3 h-3" />
                      </button>
                    </div>
                  ))}
                </div>
                <p className="mt-1 text-xs text-neutral-500">
                  These documents will be included as context for test generation
                </p>
              </div>
            )}

            {/* Workflow Run Context Selector */}
            <div className="mt-4">
              <div className="flex items-center justify-between mb-2">
                <label className="text-xs text-neutral-400">
                  Workflow Run Context
                </label>
                <button
                  onClick={() => setShowWorkflowSelector(!showWorkflowSelector)}
                  className="flex items-center gap-1 px-2 py-1 text-xs bg-neutral-800 text-neutral-300 rounded hover:bg-neutral-700 transition-colors border border-neutral-700"
                  title="Include data from a previous workflow run"
                >
                  <Database className="w-3 h-3" />
                  {selectedTaskRunId ? "Change Run" : "Add Run Data"}
                  <ChevronDown className={`w-3 h-3 transition-transform ${showWorkflowSelector ? "rotate-180" : ""}`} />
                </button>
              </div>

              {showWorkflowSelector && (
                <div className="mb-3 p-2 bg-neutral-800/50 rounded border border-neutral-700">
                  <select
                    value={selectedTaskRunId || ""}
                    onChange={(e) => {
                      setSelectedTaskRunId(e.target.value || null);
                      setShowWorkflowSelector(false);
                    }}
                    className="w-full px-2 py-1.5 bg-neutral-900 border border-neutral-600 rounded text-sm text-neutral-200 focus:outline-none focus:border-purple-500"
                  >
                    <option value="">-- Select a workflow run --</option>
                    {recentTaskRuns.map((run) => (
                      <option key={run.id} value={run.id}>
                        {run.task_name}
                        {run.workflow_name ? ` (${run.workflow_name})` : ""}
                        {" - "}
                        {run.status}
                        {" - "}
                        {new Date(run.created_at).toLocaleDateString()}
                      </option>
                    ))}
                  </select>
                </div>
              )}

              {/* Selected workflow run summary */}
              {selectedTaskRunId && workflowRunContext && (
                <div className="p-2 bg-neutral-800/50 rounded border border-neutral-700 text-xs">
                  <div className="flex items-center justify-between mb-2">
                    <span className="text-neutral-300 font-medium">
                      {workflowRunContext.task_run.task_name}
                    </span>
                    <button
                      onClick={() => setSelectedTaskRunId(null)}
                      className="p-0.5 text-neutral-500 hover:text-red-400 transition-colors"
                      title="Remove workflow context"
                    >
                      <X className="w-3 h-3" />
                    </button>
                  </div>
                  <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-neutral-500">
                    <span>Status: <span className="text-neutral-400">{workflowRunContext.task_run.status}</span></span>
                    <span>Screenshots: <span className="text-neutral-400">{workflowRunContext.screenshots.length}</span></span>
                    <span>API Requests: <span className="text-neutral-400">{workflowRunContext.api_requests.length}</span></span>
                    <span>Events: <span className="text-neutral-400">{workflowRunContext.events.length}</span></span>
                    {workflowRunContext.automation && (
                      <span>Automation: <span className="text-neutral-400">{workflowRunContext.automation.automation_status}</span></span>
                    )}
                    {workflowRunContext.knowledge.length > 0 && (
                      <span>Findings: <span className="text-neutral-400">{workflowRunContext.knowledge.length}</span></span>
                    )}
                  </div>
                  {workflowRunContext.task_run.summary && (
                    <p className="mt-2 text-neutral-400 line-clamp-2">
                      {workflowRunContext.task_run.summary}
                    </p>
                  )}
                </div>
              )}

              {isLoadingContext && (
                <div className="flex items-center gap-2 text-xs text-neutral-500">
                  <Loader2 className="w-3 h-3 animate-spin" />
                  Loading workflow context...
                </div>
              )}
            </div>

            {/* Element context */}
            {analysis && (
              <div className="mt-4">
                <label className="block text-xs text-neutral-400 mb-2">
                  Available Elements (reference by name or number)
                </label>
                <div className="max-h-32 overflow-y-auto p-2 bg-neutral-800/50 rounded text-xs font-mono text-neutral-500">
                  <pre className="whitespace-pre-wrap">{elementSummary}</pre>
                </div>
              </div>
            )}

            {/* Step Output Context */}
            {stepOutputDetails.length > 0 && (
              <div className="mt-4">
                <label className="block text-xs text-neutral-400 mb-2">
                  Collected Step Outputs (available for verification)
                </label>
                <div className="max-h-32 overflow-y-auto p-2 bg-neutral-800/50 rounded text-xs">
                  {stepOutputDetails.map((detail) => (
                    <div
                      key={detail.id}
                      className="flex items-center gap-2 py-1 text-neutral-400"
                    >
                      <span
                        className={`w-2 h-2 rounded-full ${
                          detail.success ? "bg-green-500" : "bg-red-500"
                        }`}
                      />
                      <span className="text-neutral-300">{detail.name}</span>
                      <span className="text-neutral-500">({detail.type})</span>
                      {detail.duration !== undefined && (
                        <span className="text-neutral-600">{detail.duration}ms</span>
                      )}
                    </div>
                  ))}
                </div>
              </div>
            )}

            {/* Assertable Fields (click to insert) */}
            {assertableFields.length > 0 && (
              <div className="mt-4">
                <div className="flex items-center gap-1 mb-2">
                  <label className="text-xs text-neutral-400">
                    Assertable Fields ({assertableFields.length})
                  </label>
                  <span title="Click a field to insert it into your prompt">
                    <Info className="w-3 h-3 text-neutral-500" />
                  </span>
                  <span className="text-xs text-neutral-600 ml-1">(click to insert)</span>
                </div>
                <div className="max-h-32 overflow-y-auto p-2 bg-neutral-800/50 rounded text-xs">
                  {assertableFields.slice(0, 15).map((field, idx) => (
                    <button
                      key={idx}
                      onClick={() => handleInsertField(field)}
                      className="w-full text-left py-1 px-1 hover:bg-neutral-700/50 rounded transition-colors group"
                      title={`Click to insert {${field.name}} - ${field.suggested_assertions?.join(", ") || "Assert this field"}`}
                    >
                      <span className="text-purple-400 font-mono group-hover:text-purple-300">
                        {field.name}
                      </span>
                      <span className="text-neutral-500 group-hover:text-neutral-400">
                        {" "}
                        ({field.type})
                      </span>
                      <span className="text-neutral-600 ml-2 group-hover:text-neutral-500">
                        {field.description.length > 40
                          ? field.description.slice(0, 40) + "..."
                          : field.description}
                      </span>
                    </button>
                  ))}
                  {assertableFields.length > 15 && (
                    <div className="py-1 px-1 text-neutral-500 italic">
                      ... and {assertableFields.length - 15} more fields
                    </div>
                  )}
                </div>
              </div>
            )}

            {/* Collected Analyses Summary */}
            {collectedSummary && (
              <div className="mt-4 p-2 bg-blue-900/20 border border-blue-700/30 rounded">
                <p className="text-xs text-blue-300">
                  <Info className="w-3 h-3 inline mr-1" />
                  {collectedSummary} will be included as ground truth data
                </p>
              </div>
            )}

            {/* Error display */}
            {error && (
              <div className="mt-4 p-3 bg-red-900/30 border border-red-700 rounded">
                <p className="text-sm text-red-300">{error}</p>
              </div>
            )}

            {/* Generate button */}
            <button
              onClick={handleGenerate}
              disabled={isGenerating || !prompt.trim()}
              className="mt-4 flex items-center justify-center gap-2 px-4 py-2 bg-purple-600 text-white rounded hover:bg-purple-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
              data-ui-id="test-builder-ai-generate-btn"
            >
              {isGenerating ? (
                <>
                  <Loader2 className="w-4 h-4 animate-spin" />
                  Generating...
                </>
              ) : (
                <>
                  <Wand2 className="w-4 h-4" />
                  Generate Test
                </>
              )}
            </button>
          </>
        ) : (
          <>
            {/* Generated code preview */}
            <div className="flex-1 flex flex-col min-h-0">
              <div className="flex items-center justify-between mb-2">
                <label className="text-xs text-neutral-400">Generated Code</label>
                <button
                  onClick={handleCopy}
                  className="p-1 text-neutral-500 hover:text-neutral-300 transition-colors"
                  title="Copy to clipboard"
                  data-ui-id="test-builder-ai-copy-btn"
                >
                  <Copy className="w-4 h-4" />
                </button>
              </div>
              <div className="flex-1 overflow-auto p-3 bg-neutral-950 rounded border border-neutral-700">
                <pre className="text-sm font-mono text-neutral-300 whitespace-pre-wrap">
                  {generatedCode}
                </pre>
              </div>
            </div>

            {/* Action buttons */}
            <div className="mt-4 flex gap-2">
              <button
                onClick={() => {
                  setGeneratedCode(null);
                  handleGenerate();
                }}
                className="flex-1 flex items-center justify-center gap-2 px-4 py-2 bg-neutral-700 text-white rounded hover:bg-neutral-600 transition-colors"
                data-ui-id="test-builder-ai-regenerate-btn"
              >
                <RefreshCw className="w-4 h-4" />
                Regenerate
              </button>
              <button
                onClick={handleAccept}
                className="flex-1 flex items-center justify-center gap-2 px-4 py-2 bg-green-600 text-white rounded hover:bg-green-700 transition-colors"
                data-ui-id="test-builder-ai-accept-btn"
              >
                <Check className="w-4 h-4" />
                Accept
              </button>
            </div>
          </>
        )}
      </div>

      {/* Footer */}
      <div className="p-2 border-t border-neutral-700 text-xs text-neutral-500">
        Test type: {testType} | AI-generated tests should be reviewed before use
      </div>
    </div>
  );
}
