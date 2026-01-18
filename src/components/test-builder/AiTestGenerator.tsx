/**
 * AI Test Generator Component
 *
 * Generates test code from natural language descriptions using Claude.
 * Uses page analysis data to understand element references in prompts.
 */

import { useState, useCallback, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Sparkles, Loader2, RefreshCw, Check, X, Copy, Wand2 } from "lucide-react";
import type { PageAnalysis, TestType, MultiRequestAnalysis, CollectedAnalysisSet } from "./types";

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
    label: "API Response Validation",
    prompt: `Verify the API response structure and data integrity.
Parse LAST_API_RESPONSE from environment, validate required fields exist, check data types are correct.`,
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
  const [prompt, setPrompt] = useState("");
  const [isGenerating, setIsGenerating] = useState(false);
  const [generatedCode, setGeneratedCode] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Check if we have any analysis data
  const hasAnalysis = analysis || multiRequestAnalysis || collectedAnalyses;

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
  const multiRequestSummary = useMemo(() => {
    if (!multiRequestAnalysis) return "";

    const successfulRequests = multiRequestAnalysis.requests.filter((r) => r.status === "complete");
    return `${successfulRequests.length} API responses collected (${multiRequestAnalysis.total_elements ?? 0} total elements)`;
  }, [multiRequestAnalysis]);

  // Build collected analyses summary for display
  const collectedSummary = useMemo(() => {
    if (!collectedAnalyses) return "";
    const playwrightCount = collectedAnalyses.analyses.filter(
      (a) => a.type === "playwright",
    ).length;
    const visionCount = collectedAnalyses.analyses.filter((a) => a.type === "vision").length;
    const apiCount = collectedAnalyses.analyses.filter((a) => a.type === "api_request").length;
    return `${collectedAnalyses.analyses.length} analyses (${playwrightCount} Playwright, ${visionCount} Vision, ${apiCount} API)`;
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
  }, [prompt, analysis, multiRequestAnalysis, collectedAnalyses, testType]);

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
            {/* Prompt textarea */}
            <div className="flex-1 flex flex-col min-h-0">
              <label className="block text-xs text-neutral-400 mb-2">
                Describe what you want to test
              </label>
              <textarea
                value={prompt}
                onChange={(e) => setPrompt(e.target.value)}
                placeholder="Example: Click the 'Submit' button and verify a success message appears"
                className="flex-1 px-3 py-2 bg-neutral-800 border border-neutral-700 rounded text-sm text-neutral-200 placeholder-neutral-500 resize-none focus:outline-none focus:border-purple-500"
              />
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
              >
                <RefreshCw className="w-4 h-4" />
                Regenerate
              </button>
              <button
                onClick={handleAccept}
                className="flex-1 flex items-center justify-center gap-2 px-4 py-2 bg-green-600 text-white rounded hover:bg-green-700 transition-colors"
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
