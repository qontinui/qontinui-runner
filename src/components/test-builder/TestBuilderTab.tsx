/**
 * Test Builder Tab
 *
 * Main component for the verification test builder.
 * Provides a 3-panel layout for creating and editing tests:
 * - Left: Test library panel (list of tests)
 * - Center: Code editor (Monaco) with optional AI analysis panel
 * - Right: Properties panel (metadata and configuration)
 * - Bottom: Execution panel (run tests and view results)
 */

import { useState, useEffect, useCallback } from "react";
import { Sparkles, Code, ScanSearch, Zap, FileJson, ExternalLink } from "lucide-react";
import { TestBuilderProvider, useTestBuilder } from "./TestBuilderContext";
import { TestLibraryPanel } from "./TestLibraryPanel";
import { TestEditorPanel, getCodeFromTest } from "./TestEditorPanel";
import { TestPropertiesPanel } from "./TestPropertiesPanel";
import { TestExecutionPanel } from "./TestExecutionPanel";
import { PageAnalyzer } from "./PageAnalyzer";
import { ElementPicker } from "./ElementPicker";
import { AiTestGenerator } from "./AiTestGenerator";
import { TestOrchestratorPanel } from "./TestOrchestratorPanel";
import { SpecWorkflowBuilder } from "../spec-workflow-builder/SpecWorkflowBuilder";
import type { UnifiedWorkflow } from "../../types/unified-workflow";
import type {
  PageAnalysis,
  TestType,
  AnalysisData,
  MultiRequestAnalysis,
  CollectedAnalysisSet,
} from "./types";

interface TestBuilderTabProps {
  onLog?: (level: string, message: string) => void;
}

function TestBuilderContent({ onLog }: TestBuilderTabProps) {
  const {
    selectedTest,
    state,
    setDirty,
    isDraftSelected,
    updateDraft,
    setCollectedAnalyses,
    startNewTest,
  } = useTestBuilder();

  // Track the code in local state for the editor
  const [code, setCode] = useState("");

  // Tab state: "ai", "code", "analysis", "orchestrator", or "spec-workflow" - AI selected by default
  const [activeTab, setActiveTab] = useState<
    "ai" | "code" | "analysis" | "orchestrator" | "spec-workflow"
  >("ai");
  const [analysisData, setAnalysisData] = useState<AnalysisData | null>(null);
  const [showGenerator, setShowGenerator] = useState(false);

  // Helper to get element count from analysis data
  const getElementCount = (data: AnalysisData | null): number => {
    if (!data) return 0;
    if (data.type === "single") {
      return data.data.elements?.length ?? 0;
    } else if (data.type === "multi") {
      return (
        data.data.total_elements ?? data.data.requests.filter((r) => r.status === "complete").length
      );
    } else if (data.type === "collected") {
      return data.data.analyses.length;
    }
    return 0;
  };

  // Helper to get PageAnalysis for components that need it (for backward compat)
  const pageAnalysis: PageAnalysis | null =
    analysisData?.type === "single" ? analysisData.data : null;

  // Helper to get MultiRequestAnalysis (legacy)
  const multiRequestAnalysis: MultiRequestAnalysis | null =
    analysisData?.type === "multi" ? analysisData.data : null;

  // Helper to get CollectedAnalysisSet
  const collectedAnalyses: CollectedAnalysisSet | null =
    analysisData?.type === "collected" ? analysisData.data : null;

  // Selected test type for AI generation - defaults to python_script
  const [selectedTestType, setSelectedTestType] = useState<TestType>("python_script");

  // Get current test type for AI generator - use selected test's type or the dropdown selection
  const currentTestType: TestType = selectedTest?.test_type || selectedTestType;

  // Sync code with selected test (or draft)
  useEffect(() => {
    if (isDraftSelected && state.draftTest) {
      // Restore code from draft
      setCode(state.draftTest.code || "");
    } else if (selectedTest) {
      setCode(getCodeFromTest(selectedTest));
    } else {
      setCode("");
    }
  }, [selectedTest, isDraftSelected, state.draftTest]);

  // Handle code changes from editor
  const handleCodeChange = useCallback(
    (newCode: string) => {
      setCode(newCode);
      // If editing a draft, update the draft's code for persistence
      if (isDraftSelected) {
        updateDraft({}, newCode);
      }
    },
    [isDraftSelected, updateDraft],
  );

  // Handle save completed
  const handleSave = useCallback(() => {
    if (onLog) {
      onLog("info", `Test saved: ${selectedTest?.name}`);
    }
  }, [selectedTest?.name, onLog]);

  // Handle page analysis complete
  const handleAnalysisComplete = useCallback(
    (analysis: AnalysisData) => {
      console.log("[TestBuilderTab] handleAnalysisComplete called with:", analysis);
      setAnalysisData(analysis);
      setShowGenerator(true);

      // Sync collected analyses to context for use during save (auto-populate api_request_config)
      if (analysis.type === "collected") {
        setCollectedAnalyses(analysis.data);
      } else {
        setCollectedAnalyses(null);
      }

      if (onLog) {
        if (analysis.type === "single") {
          onLog(
            "info",
            `Page analysis complete: ${analysis.data.elements.length} elements detected`,
          );
        } else if (analysis.type === "multi") {
          const successCount = analysis.data.requests.filter((r) => r.status === "complete").length;
          onLog("info", `Multi-request analysis complete: ${successCount} successful responses`);
        } else if (analysis.type === "collected") {
          onLog("info", `Collected analyses: ${analysis.data.analyses.length} analyses ready`);
        }
      }
    },
    [onLog, setCollectedAnalyses],
  );

  // Handle AI-generated test code
  const handleTestGenerated = useCallback(
    (generatedCode: string) => {
      setShowGenerator(false);

      // If no draft or test is selected, auto-create a draft with the generated code
      // This fixes the bug where users could generate AI code but have no way to save it
      if (!isDraftSelected && !selectedTest) {
        startNewTest(currentTestType, generatedCode);
        setCode(generatedCode);
        if (onLog) {
          onLog("info", "AI-generated test code applied - created new draft");
        }
        return;
      }

      setCode(generatedCode);
      setDirty(true);
      // If editing a draft, update the draft's code for persistence
      if (isDraftSelected) {
        updateDraft({}, generatedCode);
      }
      if (onLog) {
        onLog("info", "AI-generated test code applied to editor");
      }
    },
    [setDirty, onLog, isDraftSelected, updateDraft, selectedTest, startNewTest, currentTestType],
  );

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Main content area */}
      <div className="flex-1 flex min-h-0 overflow-hidden">
        {/* Left: Test Library */}
        <TestLibraryPanel onOpenAiTab={() => setActiveTab("ai")} />

        {/* Center: Tabbed panel for AI Generator and Code Editor */}
        <div className="flex-1 min-w-0 flex flex-col overflow-hidden">
          {/* Tab bar */}
          <div className="flex items-center bg-neutral-800 border-b border-neutral-700">
            <button
              onClick={() => setActiveTab("ai")}
              className={`flex items-center gap-2 px-4 py-2.5 text-sm font-medium transition-colors border-b-2 ${
                activeTab === "ai"
                  ? "text-purple-400 border-purple-400 bg-neutral-900/50"
                  : "text-neutral-400 border-transparent hover:text-neutral-200 hover:bg-neutral-700/30"
              }`}
              data-ui-id="test-builder-ai-tab-btn"
            >
              <Sparkles className="w-4 h-4" />
              AI Generator
            </button>
            <button
              onClick={() => setActiveTab("code")}
              className={`flex items-center gap-2 px-4 py-2.5 text-sm font-medium transition-colors border-b-2 ${
                activeTab === "code"
                  ? "text-blue-400 border-blue-400 bg-neutral-900/50"
                  : "text-neutral-400 border-transparent hover:text-neutral-200 hover:bg-neutral-700/30"
              }`}
              data-ui-id="test-builder-code-tab-btn"
            >
              <Code className="w-4 h-4" />
              Code Editor
            </button>
            <button
              onClick={() => setActiveTab("analysis")}
              className={`flex items-center gap-2 px-4 py-2.5 text-sm font-medium transition-colors border-b-2 ${
                activeTab === "analysis"
                  ? "text-green-400 border-green-400 bg-neutral-900/50"
                  : "text-neutral-400 border-transparent hover:text-neutral-200 hover:bg-neutral-700/30"
              }`}
              data-ui-id="test-builder-analysis-tab-btn"
            >
              <ScanSearch className="w-4 h-4" />
              Page Analysis
              {analysisData && (
                <span
                  data-content-role="badge"
                  data-content-label="analysis count"
                  className="ml-1 px-1.5 py-0.5 text-xs bg-green-500/20 text-green-400 rounded"
                >
                  {analysisData.type === "multi"
                    ? `${analysisData.data.requests.filter((r) => r.status === "complete").length} req`
                    : analysisData.type === "collected"
                      ? `${analysisData.data.analyses.length} analyses`
                      : getElementCount(analysisData)}
                </span>
              )}
            </button>
            <button
              onClick={() => setActiveTab("orchestrator")}
              className={`flex items-center gap-2 px-4 py-2.5 text-sm font-medium transition-colors border-b-2 ${
                activeTab === "orchestrator"
                  ? "text-yellow-400 border-yellow-400 bg-neutral-900/50"
                  : "text-neutral-400 border-transparent hover:text-neutral-200 hover:bg-neutral-700/30"
              }`}
              data-ui-id="test-builder-orchestrator-tab-btn"
            >
              <Zap className="w-4 h-4" />
              Orchestrator
            </button>
            <button
              onClick={() => setActiveTab("spec-workflow")}
              className={`flex items-center gap-2 px-4 py-2.5 text-sm font-medium transition-colors border-b-2 ${
                activeTab === "spec-workflow"
                  ? "text-emerald-400 border-emerald-400 bg-neutral-900/50"
                  : "text-neutral-400 border-transparent hover:text-neutral-200 hover:bg-neutral-700/30"
              }`}
              data-ui-id="test-builder-spec-workflow-tab-btn"
            >
              <FileJson className="w-4 h-4" />
              Spec Workflow
            </button>
            {/* Edit in Web button */}
            <a
              href="http://localhost:3001/build/tests"
              target="_blank"
              rel="noopener noreferrer"
              className="flex items-center gap-1.5 px-3 py-2.5 text-sm font-medium transition-colors text-indigo-400 hover:text-indigo-300 hover:bg-neutral-700/30 no-underline"
              title="Edit in Web (opens browser)"
              data-ui-id="test-builder-edit-in-web-btn"
            >
              <ExternalLink className="w-4 h-4" />
              Web
            </a>
            {/* Test type selector - only show when no test is selected */}
            {!selectedTest && activeTab === "ai" && (
              <div className="ml-auto mr-4 flex items-center gap-2">
                <span data-content-role="label" className="text-xs text-neutral-500">
                  Test Type:
                </span>
                <select
                  value={selectedTestType}
                  onChange={(e) => setSelectedTestType(e.target.value as TestType)}
                  className="text-xs bg-neutral-700 text-neutral-200 border border-neutral-600 rounded px-2 py-1 focus:outline-none focus:border-purple-500"
                  data-ui-id="test-builder-test-type-select"
                >
                  <option value="python_script">Python Script</option>
                  <option value="playwright_cdp">Playwright CDP</option>
                  <option value="qontinui_vision">Qontinui Vision</option>
                  <option value="repository_test">Repository Test</option>
                </select>
              </div>
            )}
            {analysisData && activeTab === "ai" && (
              <span
                data-content-role="status"
                data-content-label="analysis summary"
                className="ml-auto mr-4 text-xs text-neutral-500"
              >
                {analysisData.type === "multi"
                  ? `${analysisData.data.requests.filter((r) => r.status === "complete").length} API responses`
                  : analysisData.type === "collected"
                    ? `${analysisData.data.analyses.length} analyses collected`
                    : `${getElementCount(analysisData)} elements detected`}
              </span>
            )}
          </div>

          {/* Tab content - full height */}
          <div className="flex-1 min-h-0 overflow-hidden">
            {activeTab === "ai" && (
              /* AI Generator Panel */
              <div className="h-full flex overflow-hidden">
                {/* For Python scripts: Show generator directly, but include analysis if available */}
                {currentTestType === "python_script" ? (
                  <div className="w-full h-full overflow-auto">
                    <AiTestGenerator
                      analysis={pageAnalysis}
                      multiRequestAnalysis={multiRequestAnalysis}
                      collectedAnalyses={collectedAnalyses}
                      testType={currentTestType}
                      onTestGenerated={(generatedCode) => {
                        handleTestGenerated(generatedCode);
                        setActiveTab("code");
                      }}
                    />
                    {/* Hint about Page Analysis */}
                    {!analysisData && (
                      <div className="mx-4 mb-4 p-3 bg-green-900/20 border border-green-700/30 rounded-lg">
                        <p className="text-xs text-green-400">
                          <strong>Tip:</strong> Run Page Analysis (third tab) to provide ground
                          truth data. Add multiple analyses (Playwright, Vision, API Request) for
                          comprehensive verification.
                        </p>
                      </div>
                    )}
                    {analysisData && analysisData.type === "single" && (
                      <div className="mx-4 mb-4 p-3 bg-green-900/20 border border-green-700/30 rounded-lg">
                        <p className="text-xs text-green-400">
                          <strong>Page Analysis available:</strong>{" "}
                          {analysisData.data.elements.length} elements detected. The AI will use
                          this as ground truth for verification.
                        </p>
                      </div>
                    )}
                    {analysisData && analysisData.type === "multi" && (
                      <div className="mx-4 mb-4 p-3 bg-blue-900/20 border border-blue-700/30 rounded-lg">
                        <p className="text-xs text-blue-400">
                          <strong>Multi-Request Ground Truth:</strong>{" "}
                          {analysisData.data.requests.filter((r) => r.status === "complete").length}{" "}
                          successful API responses collected. The AI will use all response data for
                          verification test generation.
                        </p>
                      </div>
                    )}
                    {analysisData && analysisData.type === "collected" && (
                      <div className="mx-4 mb-4 p-3 bg-purple-900/20 border border-purple-700/30 rounded-lg">
                        <p className="text-xs text-purple-400">
                          <strong>Collected Analyses:</strong> {analysisData.data.analyses.length}{" "}
                          analyses ready (
                          {analysisData.data.analyses.filter((a) => a.type === "playwright").length}{" "}
                          Playwright,{" "}
                          {analysisData.data.analyses.filter((a) => a.type === "vision").length}{" "}
                          Vision,{" "}
                          {
                            analysisData.data.analyses.filter((a) => a.type === "api_request")
                              .length
                          }{" "}
                          API). The AI will use all data for verification test generation.
                        </p>
                      </div>
                    )}
                  </div>
                ) : (
                  <>
                    {/* Left: Page Analyzer or Element Picker */}
                    <div className="w-1/2 h-full overflow-auto border-r border-neutral-700">
                      {!pageAnalysis ? (
                        <div className="p-4">
                          <PageAnalyzer
                            onAnalysisComplete={handleAnalysisComplete}
                            onError={(error) => onLog?.("error", error)}
                            initialAnalyses={collectedAnalyses?.analyses}
                          />
                        </div>
                      ) : (
                        <ElementPicker
                          analysis={pageAnalysis}
                          onElementClick={(element) => {
                            if (onLog) {
                              onLog("info", `Selected element: ${element.label}`);
                            }
                          }}
                        />
                      )}
                    </div>

                    {/* Right: AI Test Generator */}
                    <div className="w-1/2 h-full overflow-auto">
                      {showGenerator ? (
                        <AiTestGenerator
                          analysis={pageAnalysis}
                          multiRequestAnalysis={multiRequestAnalysis}
                          collectedAnalyses={collectedAnalyses}
                          testType={currentTestType}
                          onTestGenerated={(generatedCode) => {
                            handleTestGenerated(generatedCode);
                            setActiveTab("code");
                          }}
                          onCancel={() => setShowGenerator(false)}
                        />
                      ) : (
                        <div className="h-full flex items-center justify-center p-4">
                          <div className="text-center">
                            <Sparkles className="w-8 h-8 text-neutral-600 mx-auto mb-2" />
                            <p className="text-sm text-neutral-500">
                              {pageAnalysis
                                ? "Analysis complete! Click 'Generate Test' to create AI-powered tests."
                                : "Analyze a page first to enable AI test generation."}
                            </p>
                            {pageAnalysis && (
                              <button
                                onClick={() => setShowGenerator(true)}
                                className="mt-4 px-4 py-2 bg-purple-600 text-white text-sm rounded hover:bg-purple-700 transition-colors"
                              >
                                Generate Test
                              </button>
                            )}
                          </div>
                        </div>
                      )}
                    </div>
                  </>
                )}
              </div>
            )}

            {activeTab === "code" && (
              /* Code Editor Panel */
              <div className="h-full">
                <TestEditorPanel code={code} onCodeChange={handleCodeChange} />
              </div>
            )}

            {activeTab === "analysis" && (
              /* Page Analysis Panel */
              <div className="h-full overflow-auto">
                {!analysisData ? (
                  <div className="p-4">
                    <div className="mb-4 p-3 bg-neutral-800 rounded-lg">
                      <h3 className="text-sm font-medium text-neutral-200 mb-2">Page Analysis</h3>
                      <p className="text-xs text-neutral-400 mb-3">
                        Capture page elements to use as ground truth for verification tests. Use{" "}
                        <strong>Multi-Request</strong> mode to run per-page extractions for
                        verifying state machine outputs.
                      </p>
                    </div>
                    <PageAnalyzer
                      onAnalysisComplete={handleAnalysisComplete}
                      onError={(error) => onLog?.("error", error)}
                      initialAnalyses={collectedAnalyses?.analyses}
                    />
                  </div>
                ) : analysisData.type === "single" ? (
                  <div className="h-full flex flex-col">
                    <div className="p-3 bg-green-900/20 border-b border-green-700/30">
                      <div className="flex items-center justify-between">
                        <p className="text-sm text-green-400">
                          <strong>{analysisData.data.elements.length} elements</strong> detected via{" "}
                          {analysisData.data.source}
                          {analysisData.data.url && (
                            <span className="text-green-500/70 ml-2">
                              ({analysisData.data.url})
                            </span>
                          )}
                        </p>
                        <button
                          onClick={() => {
                            setAnalysisData(null);
                            setCollectedAnalyses(null);
                          }}
                          className="text-xs px-2 py-1 bg-neutral-700 text-neutral-300 rounded hover:bg-neutral-600"
                        >
                          Re-analyze
                        </button>
                      </div>
                    </div>
                    <div className="flex-1 overflow-auto">
                      <ElementPicker
                        analysis={analysisData.data}
                        onElementClick={(element) => {
                          if (onLog) {
                            onLog(
                              "info",
                              `Element: ${element.label} (${element.element_type}) at ${element.selector || `(${element.bounding_box.x}, ${element.bounding_box.y})`}`,
                            );
                          }
                        }}
                      />
                    </div>
                  </div>
                ) : analysisData.type === "collected" ? (
                  /* Collected analyses - show PageAnalyzer which manages the collection */
                  <div className="h-full flex flex-col">
                    <div className="p-3 bg-purple-900/20 border-b border-purple-700/30">
                      <div className="flex items-center justify-between">
                        <p className="text-sm text-purple-400">
                          <strong>{analysisData.data.analyses.length} analyses</strong> collected
                        </p>
                        <button
                          onClick={() => {
                            setAnalysisData(null);
                            setCollectedAnalyses(null);
                          }}
                          className="text-xs px-2 py-1 bg-neutral-700 text-neutral-300 rounded hover:bg-neutral-600"
                        >
                          Clear All
                        </button>
                      </div>
                    </div>
                    <div className="flex-1 overflow-auto p-4">
                      <PageAnalyzer
                        onAnalysisComplete={handleAnalysisComplete}
                        onError={(error) => onLog?.("error", error)}
                        initialAnalyses={collectedAnalyses?.analyses}
                      />
                    </div>
                  </div>
                ) : (
                  /* Legacy multi-request analysis results - redirect to PageAnalyzer */
                  <div className="p-4">
                    <PageAnalyzer
                      onAnalysisComplete={handleAnalysisComplete}
                      onError={(error) => onLog?.("error", error)}
                      initialAnalyses={collectedAnalyses?.analyses}
                    />
                  </div>
                )}
              </div>
            )}

            {activeTab === "orchestrator" && (
              /* Test Orchestrator Panel - AI-driven multi-step API test creation */
              <div className="h-full">
                <TestOrchestratorPanel
                  onTestGenerated={(generatedCode, generatedTestType) => {
                    setCode(generatedCode);
                    setDirty(true);
                    setSelectedTestType(generatedTestType);
                    setActiveTab("code");
                    if (isDraftSelected) {
                      updateDraft({}, generatedCode);
                    }
                    onLog?.("info", "Orchestrator-generated test code applied to editor");
                  }}
                  onLog={onLog}
                />
              </div>
            )}

            {activeTab === "spec-workflow" && (
              /* Spec Workflow Builder - build workflows from test spec JSON files */
              <div className="h-full">
                <SpecWorkflowBuilder
                  onApplyWorkflow={async (workflow: UnifiedWorkflow) => {
                    try {
                      const res = await fetch("http://localhost:9876/unified-workflows", {
                        method: "POST",
                        headers: { "Content-Type": "application/json" },
                        body: JSON.stringify(workflow),
                      });
                      if (!res.ok) throw new Error(`HTTP ${res.status}`);
                      const saved = await res.json();
                      onLog?.(
                        "info",
                        `Workflow "${workflow.name}" saved (ID: ${saved.id || workflow.id}). Open it in the Workflow Builder tab.`,
                      );
                    } catch (err) {
                      onLog?.("error", `Failed to save workflow: ${err}`);
                    }
                  }}
                />
              </div>
            )}
          </div>
        </div>

        {/* Right: Properties */}
        <TestPropertiesPanel code={code} onSave={handleSave} />
      </div>

      {/* Bottom: Execution Panel */}
      <TestExecutionPanel />
    </div>
  );
}

export function TestBuilderTab({ onLog }: TestBuilderTabProps) {
  return (
    <TestBuilderProvider>
      <TestBuilderContent onLog={onLog} />
    </TestBuilderProvider>
  );
}
