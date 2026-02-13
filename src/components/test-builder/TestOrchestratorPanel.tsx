/**
 * Test Orchestrator Panel
 *
 * Main UI component for AI-driven multi-step API test orchestration.
 * Enables users to select saved API requests, describe a test goal,
 * and have AI create an execution plan with variable chaining.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Play,
  Sparkles,
  Check,
  AlertCircle,
  ChevronRight,
  RefreshCw,
  Trash2,
  FileCode,
  Search,
  Key,
  Shield,
  Terminal,
  Settings,
} from "lucide-react";
import { PlanVisualization } from "./PlanVisualization";
import { ExecutionLog } from "./ExecutionLog";
import { TestStepsPreview } from "./TestStepsPreview";
import type {
  TestOrchestrationPlan,
  TestOrchestrationRequest,
  OrchestrationExecutionResult,
  GeneratedTest,
  SavedApiRequest,
  OrchestratorPhase,
} from "../../types/test-orchestrator";
import type { ResolvedExecutionContext } from "../../types";
import type { CommandResponse, TestType } from "./types";

interface TestOrchestratorPanelProps {
  onTestGenerated?: (code: string, testType: TestType) => void;
  onLog?: (level: string, message: string) => void;
}

export function TestOrchestratorPanel({ onTestGenerated, onLog }: TestOrchestratorPanelProps) {
  // Phase state
  const [phase, setPhase] = useState<OrchestratorPhase>("select");

  // Selection state
  const [availableRequests, setAvailableRequests] = useState<SavedApiRequest[]>([]);
  const [selectedRequestIds, setSelectedRequestIds] = useState<Set<string>>(new Set());
  const [searchQuery, setSearchQuery] = useState("");

  // Input state
  const [testDescription, setTestDescription] = useState("");
  const [additionalContext, setAdditionalContext] = useState("");
  const [testType, setTestType] = useState<TestType>("python_script");

  // Plan state
  const [plan, setPlan] = useState<TestOrchestrationPlan | null>(null);

  // Execution state
  const [executionResult, setExecutionResult] = useState<OrchestrationExecutionResult | null>(null);
  const [currentStepIndex, setCurrentStepIndex] = useState(0);

  // Generation state
  const [generatedTest, setGeneratedTest] = useState<GeneratedTest | null>(null);

  // UI state
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Auth context state
  const [authContext, setAuthContext] = useState<ResolvedExecutionContext | null>(null);

  const loadAuthContext = useCallback(async () => {
    try {
      const response = await invoke<{ success: boolean; data?: ResolvedExecutionContext }>(
        "get_resolved_execution_context",
      );
      if (response.success && response.data) {
        setAuthContext(response.data);
      }
    } catch (err) {
      console.error("Failed to load auth context:", err);
    }
  }, []);

  const loadSavedRequests = useCallback(async () => {
    try {
      const response = await invoke<CommandResponse<SavedApiRequest[]>>(
        "get_saved_requests_for_orchestration",
      );
      if (response.success && response.data) {
        setAvailableRequests(response.data);
      } else {
        onLog?.("error", response.message || "Failed to load saved requests");
      }
    } catch (err) {
      onLog?.("error", `Failed to load saved requests: ${err}`);
    }
  }, [onLog]);

  // Load saved requests and auth context on mount
  useEffect(() => {
    loadSavedRequests();
    loadAuthContext();
  }, [loadSavedRequests, loadAuthContext]);

  // Filter requests by search query
  const filteredRequests = availableRequests.filter(
    (req) =>
      req.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      req.url.toLowerCase().includes(searchQuery.toLowerCase()) ||
      req.description.toLowerCase().includes(searchQuery.toLowerCase()),
  );

  // Toggle request selection
  const toggleRequest = (id: string) => {
    setSelectedRequestIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  };

  // Select all filtered
  const selectAll = () => {
    setSelectedRequestIds(new Set(filteredRequests.map((r) => r.id)));
  };

  // Clear selection
  const clearSelection = () => {
    setSelectedRequestIds(new Set());
  };

  // Create plan
  const createPlan = useCallback(async () => {
    if (selectedRequestIds.size === 0) {
      setError("Please select at least one API request");
      return;
    }
    if (!testDescription.trim()) {
      setError("Please describe what the test should verify");
      return;
    }

    setIsLoading(true);
    setError(null);
    setPhase("planning");

    try {
      const request: TestOrchestrationRequest = {
        request_ids: Array.from(selectedRequestIds),
        test_description: testDescription.trim(),
        context: additionalContext.trim() || undefined,
      };

      onLog?.("info", "Creating orchestration plan with AI...");

      const response = await invoke<CommandResponse<TestOrchestrationPlan>>(
        "plan_test_orchestration",
        { request },
      );

      if (response.success && response.data) {
        setPlan(response.data);
        setPhase("review");
        onLog?.("info", `Plan created with ${response.data.steps.length} steps`);
      } else {
        setError(response.message || "Failed to create plan");
        setPhase("select");
      }
    } catch (err) {
      setError(`Failed to create plan: ${err}`);
      setPhase("select");
      onLog?.("error", `Failed to create plan: ${err}`);
    } finally {
      setIsLoading(false);
    }
  }, [selectedRequestIds, testDescription, additionalContext, onLog]);

  // Execute plan
  const executePlan = useCallback(async () => {
    if (!plan) return;

    setIsLoading(true);
    setError(null);
    setPhase("executing");
    setCurrentStepIndex(0);

    try {
      onLog?.("info", `Executing plan with ${plan.steps.length} steps...`);

      const response = await invoke<CommandResponse<OrchestrationExecutionResult>>(
        "execute_test_orchestration",
        { plan },
      );

      if (response.success && response.data) {
        setExecutionResult(response.data);
        setPhase(response.data.success ? "generating" : "review");

        if (response.data.success) {
          onLog?.("info", "Execution complete, generating test...");
          // Auto-generate test
          await generateTest(response.data);
        } else {
          // Get the error message from the failed step
          const failedStepIdx = response.data.failed_at_step;
          const failedStep =
            failedStepIdx !== undefined ? response.data.step_results[failedStepIdx] : undefined;
          const statusCode = failedStep?.response?.status_code;
          const errorDetail = failedStep?.error || `HTTP ${statusCode || "unknown"}`;

          // Check if it's an authentication error
          const isAuthError = statusCode === 401 || statusCode === 403;
          const authHint = isAuthError
            ? " Auth tokens in saved requests may have expired. Try re-capturing the requests while logged in."
            : "";

          onLog?.("warn", `Execution failed at step ${failedStepIdx}: ${errorDetail}${authHint}`);
          // Also set the error for display
          setError(
            `Step ${failedStepIdx !== undefined ? failedStepIdx + 1 : "?"} failed: ${errorDetail}${authHint}`,
          );
        }
      } else {
        setError(response.message || "Failed to execute plan");
        setPhase("review");
      }
    } catch (err) {
      setError(`Failed to execute plan: ${err}`);
      setPhase("review");
      onLog?.("error", `Failed to execute plan: ${err}`);
    } finally {
      setIsLoading(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- generateTest is defined below and causes circular dependency warning
  }, [plan, onLog]);

  // Generate test code
  const generateTest = useCallback(
    async (result: OrchestrationExecutionResult) => {
      if (!plan) return;

      setIsLoading(true);
      setError(null);
      setPhase("generating");

      try {
        onLog?.("info", `Generating ${testType} test from execution results...`);

        const response = await invoke<CommandResponse<GeneratedTest>>(
          "generate_test_from_orchestration",
          {
            executionResult: result,
            plan,
            testType,
            additionalInstructions: null,
          },
        );

        if (response.success && response.data) {
          setGeneratedTest(response.data);
          setPhase("complete");
          onLog?.("info", `Test generated: ${response.data.name}`);
        } else {
          setError(response.message || "Failed to generate test");
          setPhase("review");
        }
      } catch (err) {
        setError(`Failed to generate test: ${err}`);
        setPhase("review");
        onLog?.("error", `Failed to generate test: ${err}`);
      } finally {
        setIsLoading(false);
      }
    },
    [plan, testType, onLog],
  );

  // Apply generated test
  const applyTest = useCallback(() => {
    if (generatedTest && onTestGenerated) {
      onTestGenerated(generatedTest.code, testType);
      onLog?.("info", "Test code applied to editor");
    }
  }, [generatedTest, testType, onTestGenerated, onLog]);

  // Reset to start
  const reset = () => {
    setPhase("select");
    setPlan(null);
    setExecutionResult(null);
    setGeneratedTest(null);
    setError(null);
    setCurrentStepIndex(0);
  };

  // Get selected requests as array (currently unused but kept for future use)
  const _selectedRequests = availableRequests.filter((r) => selectedRequestIds.has(r.id));

  return (
    <div className="h-full flex flex-col bg-neutral-900 overflow-hidden">
      {/* Header with phase indicator */}
      <div className="flex items-center gap-4 p-4 border-b border-neutral-700 bg-neutral-800">
        <PhaseIndicator phase={phase} />
        {error && (
          <div className="flex-1 flex items-center gap-2 text-red-400 text-sm">
            <AlertCircle className="w-4 h-4" />
            {error}
          </div>
        )}
        {/* Auth indicator */}
        {authContext && (
          <AuthIndicator
            context={authContext}
            onOpenSettings={() => {
              // Navigate to settings - emit an event or use a callback
              onLog?.("info", "Open Settings > Execution Variables to configure auth");
            }}
          />
        )}
      </div>

      {/* Main content */}
      <div className="flex-1 overflow-auto">
        {/* Phase: Select Requests */}
        {phase === "select" && (
          <div className="p-4 space-y-4">
            {/* Step 1: Select API Requests */}
            <div className="space-y-2">
              <h3 className="text-sm font-medium text-neutral-200 flex items-center gap-2">
                <span className="w-6 h-6 rounded-full bg-purple-600 text-white text-xs flex items-center justify-center">
                  1
                </span>
                Select API Requests
              </h3>

              {/* Search and actions */}
              <div className="flex items-center gap-2">
                <div className="flex-1 relative">
                  <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-neutral-500" />
                  <input
                    type="text"
                    placeholder="Search requests..."
                    value={searchQuery}
                    onChange={(e) => setSearchQuery(e.target.value)}
                    className="w-full pl-9 pr-3 py-2 bg-neutral-800 border border-neutral-700 rounded text-sm text-neutral-200 placeholder-neutral-500 focus:outline-none focus:border-purple-500"
                    data-ui-id="test-builder-orchestrator-search-input"
                  />
                </div>
                <button
                  onClick={selectAll}
                  className="px-3 py-2 text-xs bg-neutral-700 text-neutral-300 rounded hover:bg-neutral-600"
                  data-ui-id="test-builder-orchestrator-select-all-btn"
                >
                  Select All
                </button>
                <button
                  onClick={clearSelection}
                  className="px-3 py-2 text-xs bg-neutral-700 text-neutral-300 rounded hover:bg-neutral-600"
                  data-ui-id="test-builder-orchestrator-clear-btn"
                >
                  Clear
                </button>
              </div>

              {/* Request list */}
              <div className="max-h-48 overflow-auto border border-neutral-700 rounded bg-neutral-800">
                {filteredRequests.length === 0 ? (
                  <div className="p-4 text-center text-neutral-500 text-sm">
                    {availableRequests.length === 0
                      ? "No saved API requests found. Create some in the API Request Library first."
                      : "No requests match your search."}
                  </div>
                ) : (
                  filteredRequests.map((req) => (
                    <label
                      key={req.id}
                      className={`flex items-center gap-3 p-3 border-b border-neutral-700 last:border-b-0 cursor-pointer hover:bg-neutral-700/50 ${
                        selectedRequestIds.has(req.id) ? "bg-purple-900/20" : ""
                      }`}
                    >
                      <input
                        type="checkbox"
                        checked={selectedRequestIds.has(req.id)}
                        onChange={() => toggleRequest(req.id)}
                        className="rounded border-neutral-600 bg-neutral-700 text-purple-500 focus:ring-purple-500"
                      />
                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-2">
                          <span
                            className={`px-1.5 py-0.5 text-xs font-mono rounded ${
                              req.method === "GET"
                                ? "bg-green-900/50 text-green-400"
                                : req.method === "POST"
                                  ? "bg-blue-900/50 text-blue-400"
                                  : req.method === "PUT"
                                    ? "bg-yellow-900/50 text-yellow-400"
                                    : req.method === "DELETE"
                                      ? "bg-red-900/50 text-red-400"
                                      : "bg-neutral-700 text-neutral-400"
                            }`}
                          >
                            {req.method}
                          </span>
                          <span
                            data-content-role="label"
                            data-content-label="request name"
                            className="text-sm text-neutral-200 truncate"
                          >
                            {req.name}
                          </span>
                        </div>
                        <div
                          data-content-role="code"
                          data-content-label="request url"
                          className="text-xs text-neutral-500 truncate"
                        >
                          {req.url}
                        </div>
                      </div>
                    </label>
                  ))
                )}
              </div>

              {selectedRequestIds.size > 0 && (
                <div
                  data-content-role="metric"
                  data-content-label="selected requests count"
                  className="text-xs text-neutral-500"
                >
                  {selectedRequestIds.size} request(s) selected
                </div>
              )}
            </div>

            {/* Step 2: Describe Test */}
            <div className="space-y-2">
              <h3 className="text-sm font-medium text-neutral-200 flex items-center gap-2">
                <span className="w-6 h-6 rounded-full bg-purple-600 text-white text-xs flex items-center justify-center">
                  2
                </span>
                Describe Your Test
              </h3>
              <textarea
                value={testDescription}
                onChange={(e) => setTestDescription(e.target.value)}
                placeholder="What should the test verify? e.g., 'Verify that the extraction returns more than 3 states'"
                rows={3}
                className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded text-sm text-neutral-200 placeholder-neutral-500 focus:outline-none focus:border-purple-500 resize-none"
                data-ui-id="test-builder-orchestrator-description-input"
              />
            </div>

            {/* Step 3: Additional Context (Optional) */}
            <div className="space-y-2">
              <h3 className="text-sm font-medium text-neutral-200 flex items-center gap-2">
                <span className="w-6 h-6 rounded-full bg-neutral-600 text-white text-xs flex items-center justify-center">
                  3
                </span>
                Additional Context
                <span className="text-xs text-neutral-500">(Optional)</span>
              </h3>
              <textarea
                value={additionalContext}
                onChange={(e) => setAdditionalContext(e.target.value)}
                placeholder="Any additional context or constraints for the AI..."
                rows={2}
                className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded text-sm text-neutral-200 placeholder-neutral-500 focus:outline-none focus:border-purple-500 resize-none"
                data-ui-id="test-builder-orchestrator-context-input"
              />
            </div>

            {/* Test type selector */}
            <div className="flex items-center gap-4">
              <label className="text-sm text-neutral-400">Test Type:</label>
              <select
                value={testType}
                onChange={(e) => setTestType(e.target.value as TestType)}
                className="px-3 py-1.5 bg-neutral-800 border border-neutral-700 rounded text-sm text-neutral-200 focus:outline-none focus:border-purple-500"
                data-ui-id="test-builder-orchestrator-test-type-select"
              >
                <option value="python_script">Python Script</option>
                <option value="playwright_cdp">Playwright CDP</option>
              </select>
            </div>

            {/* Create Plan button */}
            <button
              onClick={createPlan}
              disabled={isLoading || selectedRequestIds.size === 0 || !testDescription.trim()}
              className="w-full flex items-center justify-center gap-2 px-4 py-3 bg-purple-600 text-white rounded hover:bg-purple-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
              data-ui-id="test-builder-orchestrator-create-plan-btn"
            >
              {isLoading ? (
                <RefreshCw className="w-4 h-4 animate-spin" />
              ) : (
                <Sparkles className="w-4 h-4" />
              )}
              Create Orchestration Plan
            </button>
          </div>
        )}

        {/* Phase: Planning (loading) */}
        {phase === "planning" && (
          <div className="flex-1 flex items-center justify-center p-8">
            <div className="text-center">
              <RefreshCw className="w-8 h-8 text-purple-400 animate-spin mx-auto mb-4" />
              <p className="text-neutral-300">AI is creating your execution plan...</p>
              <p className="text-sm text-neutral-500 mt-2">
                Analyzing {selectedRequestIds.size} requests and planning variable chaining
              </p>
            </div>
          </div>
        )}

        {/* Phase: Review Plan */}
        {phase === "review" && plan && (
          <div className="p-4 space-y-4">
            <div className="flex items-center justify-between">
              <h3 data-content-role="heading" className="text-sm font-medium text-neutral-200">
                Execution Plan
              </h3>
              <div className="flex items-center gap-2">
                <button
                  onClick={reset}
                  className="px-3 py-1.5 text-xs bg-neutral-700 text-neutral-300 rounded hover:bg-neutral-600"
                >
                  <Trash2 className="w-3 h-3 inline mr-1" />
                  Reset
                </button>
              </div>
            </div>

            {/* Plan explanation */}
            <div className="p-3 bg-neutral-800 rounded border border-neutral-700">
              <p className="text-sm text-neutral-300">{plan.explanation}</p>
            </div>

            {/* Plan visualization */}
            <PlanVisualization plan={plan} currentStepIndex={currentStepIndex} />

            {/* Verification suggestions */}
            {plan.verification_suggestions.length > 0 && (
              <div className="space-y-2">
                <h4 className="text-xs font-medium text-neutral-400 uppercase">
                  Suggested Verifications
                </h4>
                {plan.verification_suggestions.map((suggestion, idx) => (
                  <div
                    key={idx}
                    className="p-2 bg-green-900/20 border border-green-700/30 rounded text-sm"
                  >
                    <div className="text-green-400">{suggestion.description}</div>
                    <div className="text-xs text-green-500/70 font-mono mt-1">
                      {suggestion.condition}
                    </div>
                  </div>
                ))}
              </div>
            )}

            {/* Execution result (if re-viewing after execution) */}
            {executionResult && <ExecutionLog result={executionResult} />}

            {/* Execute button */}
            <button
              onClick={executePlan}
              disabled={isLoading}
              className="w-full flex items-center justify-center gap-2 px-4 py-3 bg-green-600 text-white rounded hover:bg-green-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
              data-ui-id="test-builder-orchestrator-execute-btn"
            >
              {isLoading ? (
                <RefreshCw className="w-4 h-4 animate-spin" />
              ) : (
                <Play className="w-4 h-4" />
              )}
              Execute Plan
            </button>
          </div>
        )}

        {/* Phase: Executing */}
        {phase === "executing" && plan && (
          <div className="p-4 space-y-4">
            <div className="flex items-center justify-between">
              <h3 data-content-role="heading" className="text-sm font-medium text-neutral-200">
                Executing Plan
              </h3>
              <div
                data-content-role="metric"
                data-content-label="execution progress"
                className="text-sm text-neutral-400"
              >
                Step {currentStepIndex + 1} of {plan.steps.length}
              </div>
            </div>

            <PlanVisualization plan={plan} currentStepIndex={currentStepIndex} isExecuting />

            <div className="flex items-center justify-center p-4">
              <RefreshCw className="w-6 h-6 text-blue-400 animate-spin" />
              <span className="ml-2 text-neutral-300">
                Running {plan.steps[currentStepIndex]?.name || "..."}
              </span>
            </div>
          </div>
        )}

        {/* Phase: Generating */}
        {phase === "generating" && (
          <div className="flex-1 flex items-center justify-center p-8">
            <div className="text-center">
              <RefreshCw className="w-8 h-8 text-purple-400 animate-spin mx-auto mb-4" />
              <p className="text-neutral-300">AI is generating verification test code...</p>
              <p className="text-sm text-neutral-500 mt-2">
                Creating {testType} test from execution results
              </p>
            </div>
          </div>
        )}

        {/* Phase: Complete */}
        {phase === "complete" && generatedTest && (
          <div className="p-4 space-y-4 overflow-auto">
            <div className="flex items-center gap-2 text-green-400">
              <Check className="w-5 h-5" />
              <h3 data-content-role="status" className="text-sm font-medium">
                Test Generated Successfully
              </h3>
            </div>

            {/* Test info */}
            <div className="p-3 bg-neutral-800 rounded border border-neutral-700">
              <div
                data-content-role="label"
                data-content-label="generated test name"
                className="text-sm font-medium text-neutral-200"
              >
                {generatedTest.name}
              </div>
              <div
                data-content-role="description"
                data-content-label="generated test description"
                className="text-xs text-neutral-400 mt-1"
              >
                {generatedTest.description}
              </div>
            </div>

            {/* Explanation */}
            <div className="p-3 bg-purple-900/20 border border-purple-700/30 rounded">
              <div
                data-content-role="heading"
                className="text-xs font-medium text-purple-400 uppercase mb-1"
              >
                AI Explanation
              </div>
              <p className="text-sm text-purple-300">{generatedTest.explanation}</p>
            </div>

            {/* Test Steps Preview */}
            {generatedTest.steps && generatedTest.steps.length > 0 && (
              <TestStepsPreview steps={generatedTest.steps} />
            )}

            {/* Execution Results (API request outputs) */}
            {executionResult && (
              <div className="space-y-2">
                <h4 className="text-xs font-medium text-neutral-400 uppercase">
                  Execution Results
                </h4>
                <ExecutionLog result={executionResult} />
              </div>
            )}

            {/* Code preview */}
            <div className="space-y-2">
              <h4 className="text-xs font-medium text-neutral-400 uppercase">Generated Code</h4>
              <pre className="p-3 bg-neutral-950 rounded border border-neutral-800 text-xs text-neutral-300 overflow-auto max-h-64">
                {generatedTest.code}
              </pre>
            </div>

            {/* Actions */}
            <div className="flex items-center gap-2">
              <button
                onClick={applyTest}
                className="flex-1 flex items-center justify-center gap-2 px-4 py-2 bg-purple-600 text-white rounded hover:bg-purple-700 transition-colors"
                data-ui-id="test-builder-orchestrator-apply-btn"
              >
                <FileCode className="w-4 h-4" />
                Apply to Editor
              </button>
              <button
                onClick={reset}
                className="px-4 py-2 bg-neutral-700 text-neutral-300 rounded hover:bg-neutral-600 transition-colors"
                data-ui-id="test-builder-orchestrator-reset-btn"
              >
                Start Over
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

/**
 * Phase indicator showing progress through the orchestration workflow
 */
function PhaseIndicator({ phase }: { phase: OrchestratorPhase }) {
  const phases: { key: OrchestratorPhase; label: string }[] = [
    { key: "select", label: "Select" },
    { key: "review", label: "Review" },
    { key: "executing", label: "Execute" },
    { key: "complete", label: "Complete" },
  ];

  const currentIndex = phases.findIndex(
    (p) =>
      p.key === phase ||
      (phase === "planning" && p.key === "select") ||
      (phase === "generating" && p.key === "executing"),
  );

  return (
    <div className="flex items-center gap-1">
      {phases.map((p, idx) => (
        <div key={p.key} className="flex items-center">
          <div
            className={`px-2 py-1 text-xs rounded ${
              idx < currentIndex
                ? "bg-green-600 text-white"
                : idx === currentIndex
                  ? "bg-purple-600 text-white"
                  : "bg-neutral-700 text-neutral-400"
            }`}
          >
            {idx < currentIndex ? <Check className="w-3 h-3 inline mr-1" /> : null}
            {p.label}
          </div>
          {idx < phases.length - 1 && <ChevronRight className="w-4 h-4 text-neutral-600 mx-1" />}
        </div>
      ))}
    </div>
  );
}

/**
 * Auth indicator showing the current execution variables configuration
 */
function AuthIndicator({
  context,
  onOpenSettings,
}: {
  context: ResolvedExecutionContext;
  onOpenSettings: () => void;
}) {
  const getAuthIcon = () => {
    switch (context.authSource) {
      case "captured":
        return <Shield className="w-3 h-3" />;
      case "manual":
        return <Key className="w-3 h-3" />;
      case "environment":
        return <Terminal className="w-3 h-3" />;
      default:
        return <Shield className="w-3 h-3" />;
    }
  };

  const getAuthLabel = () => {
    switch (context.authSource) {
      case "captured":
        return "Captured Auth";
      case "manual":
        return "Manual Token";
      case "environment":
        return `$${context.authTokenStatus?.includes("$") ? context.authTokenStatus.split("$")[1]?.split(" ")[0] : "ENV"}`;
      default:
        return "Auth";
    }
  };

  const authStatus = context.authTokenStatus ?? "";
  const isWarning =
    context.authSource !== "captured" &&
    (authStatus.includes("not set") || authStatus.includes("not configured"));

  return (
    <button
      onClick={onOpenSettings}
      className={`ml-auto flex items-center gap-1.5 px-2 py-1 text-xs rounded border transition-colors ${
        isWarning
          ? "bg-yellow-900/30 border-yellow-600/50 text-yellow-400 hover:bg-yellow-900/50"
          : "bg-neutral-700/50 border-neutral-600 text-neutral-300 hover:bg-neutral-700"
      }`}
      title={`Auth: ${authStatus || "Unknown"}. Click to configure.`}
    >
      {getAuthIcon()}
      <span>{getAuthLabel()}</span>
      <Settings className="w-3 h-3 opacity-50" />
    </button>
  );
}
