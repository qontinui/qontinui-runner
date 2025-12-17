/**
 * AiBuilderTab.tsx
 *
 * AI Automation Builder panel that allows users to:
 * 1. Build an ordered list of workflows and states to execute
 * 2. Configure screenshot capture for each step
 * 3. Enter natural language goals
 * 4. Generate and run AI-powered recursive automation
 */

import { useState, useEffect, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Sparkles,
  Play,
  ChevronDown,
  Target,
  Image as ImageIcon,
  RefreshCw,
  Info,
  Loader2,
  CheckCircle,
  XCircle,
  Copy,
  History,
  Workflow,
  Camera,
  Plus,
  Trash2,
  ChevronUp,
  GripVertical,
} from "lucide-react";
import { useExecution } from "../contexts";
import CollapsiblePanel from "./CollapsiblePanel";

interface StateInfo {
  name: string;
  description?: string;
  images: string[];
}

interface ImageInfo {
  name: string;
  stateName: string;
}

interface WorkflowInfo {
  id: string;
  name: string;
  category?: string;
}

/** A single step in the execution sequence */
interface ExecutionStep {
  id: string;
  type: "workflow" | "state";
  name: string;
  takeScreenshot: boolean;
}

interface PromptHistoryEntry {
  id: string;
  timestamp: number;
  steps: ExecutionStep[];
  goal: string;
  success?: boolean;
}

const PROMPT_TEMPLATE = `# Recursive Automation Loop

Execute automation steps in order, analyze results, fix issues, and recursively continue until success.

## Configuration

**Execution Steps:** {{STEP_COUNT}} steps
**Goal:** {{GOAL}}

## Execution Sequence

{{EXECUTION_STEPS}}

## Post-Execution Analysis

### Analyze Logs

After completing all steps, check for errors:
\`\`\`bash
tail -200 /mnt/c/Users/Joshua/Documents/qontinui_parent_directory/.dev-logs/runner-backend.log
grep -i "error\\|exception\\|failed\\|panic" /mnt/c/Users/Joshua/Documents/qontinui_parent_directory/.dev-logs/runner-backend.log | tail -50
\`\`\`

### Analyze Screenshots

Read any screenshots saved during execution to visually inspect the UI state.

### Fix Issues

If any errors were found:
1. Identify the root cause from logs and screenshots
2. Read the relevant source code
3. Make the fix
4. Restart affected services if needed

### Recursive Continuation

**If fixes were made**, use trigger_ai_analysis to spawn a new session with this same prompt.
**If no issues found**, report success and stop.

## Rules

- Execute steps IN THE EXACT ORDER specified above
- ALWAYS analyze logs after all steps complete
- ALWAYS visually inspect screenshots
- NEVER ask the user to check things manually
- STOP when all steps succeed with no errors
- MAX ITERATIONS: 10
`;

export function AiBuilderTab() {
  const execution = useExecution();

  // Ordered execution steps
  const [executionSteps, setExecutionSteps] = useState<ExecutionStep[]>([]);
  const [goal, setGoal] = useState("");

  // UI state
  const [showAddDropdown, setShowAddDropdown] = useState(false);
  const [isRunning, setIsRunning] = useState(false);
  const [lastResult, setLastResult] = useState<{ success: boolean; message: string } | null>(null);

  // History
  const [history, setHistory] = useState<PromptHistoryEntry[]>(() => {
    try {
      const stored = localStorage.getItem("qontinui-ai-builder-history");
      return stored ? JSON.parse(stored) : [];
    } catch {
      return [];
    }
  });

  // Parse states, images, and workflows from config
  const { states, images, workflows } = useMemo(() => {
    const stateList: StateInfo[] = [];
    const imageList: ImageInfo[] = [];
    const workflowList: WorkflowInfo[] = [];

    // Parse states and images
    if (execution.config?.states && Array.isArray(execution.config.states)) {
      for (const state of execution.config.states) {
        const stateName = state.name || state.id || "Unknown";
        const stateImages: string[] = [];

        if (state.images && Array.isArray(state.images)) {
          for (const img of state.images) {
            const imgName = img.name || img.id || "";
            if (imgName) {
              stateImages.push(imgName);
              imageList.push({ name: imgName, stateName });
            }
          }
        }
        stateList.push({
          name: stateName,
          description: state.description || "",
          images: stateImages,
        });
      }
    }

    // Parse workflows (filter to "Main" category, exclude internal)
    if (execution.config?.workflows && Array.isArray(execution.config.workflows)) {
      for (const workflow of execution.config.workflows) {
        const category = workflow.category?.toLowerCase() || "";
        const visibility = workflow.visibility || "public";

        if (category === "main" && visibility !== "internal") {
          workflowList.push({
            id: workflow.id || workflow.name || "unknown",
            name: workflow.name || workflow.id || "Unknown",
            category: workflow.category,
          });
        }
      }
    }

    return { states: stateList, images: imageList, workflows: workflowList };
  }, [execution.config]);

  // Save history to localStorage
  useEffect(() => {
    localStorage.setItem("qontinui-ai-builder-history", JSON.stringify(history.slice(0, 20)));
  }, [history]);

  // Add a step to the execution list
  const addStep = (type: "workflow" | "state", name: string) => {
    const newStep: ExecutionStep = {
      id: crypto.randomUUID(),
      type,
      name,
      takeScreenshot: true, // Default to taking screenshots
    };
    setExecutionSteps((prev) => [...prev, newStep]);
    setShowAddDropdown(false);
  };

  // Remove a step
  const removeStep = (stepId: string) => {
    setExecutionSteps((prev) => prev.filter((s) => s.id !== stepId));
  };

  // Toggle screenshot for a step
  const toggleStepScreenshot = (stepId: string) => {
    setExecutionSteps((prev) =>
      prev.map((s) => (s.id === stepId ? { ...s, takeScreenshot: !s.takeScreenshot } : s)),
    );
  };

  // Move step up
  const moveStepUp = (index: number) => {
    if (index === 0) return;
    setExecutionSteps((prev) => {
      const newSteps = [...prev];
      [newSteps[index - 1], newSteps[index]] = [newSteps[index], newSteps[index - 1]];
      return newSteps;
    });
  };

  // Move step down
  const moveStepDown = (index: number) => {
    setExecutionSteps((prev) => {
      if (index === prev.length - 1) return prev;
      const newSteps = [...prev];
      [newSteps[index], newSteps[index + 1]] = [newSteps[index + 1], newSteps[index]];
      return newSteps;
    });
  };

  // Generate the prompt
  const generatePrompt = () => {
    const goalStr = goal.trim() || "Verify automation works correctly";

    // Generate execution steps instructions
    let executionInstructions = "";
    if (executionSteps.length === 0) {
      executionInstructions = "No steps configured. Nothing to execute.";
    } else {
      executionInstructions = executionSteps
        .map((step, index) => {
          const stepNum = index + 1;
          if (step.type === "workflow") {
            let instruction = `### Step ${stepNum}: Run Workflow "${step.name}"

Execute the workflow using the MCP tool:
\`\`\`
mcp__qontinui__run_workflow with workflow_name="${step.name}", timeout_seconds=300
\`\`\``;
            if (step.takeScreenshot) {
              instruction += `

After the workflow completes, take a screenshot to capture the result.`;
            }
            return instruction;
          } else {
            let instruction = `### Step ${stepNum}: Navigate to State "${step.name}"

Navigate to the state using the MCP tool:
\`\`\`
mcp__qontinui__go_to_state with state_names=["${step.name}"], take_screenshot=${step.takeScreenshot}
\`\`\``;
            return instruction;
          }
        })
        .join("\n\n");
    }

    return PROMPT_TEMPLATE.replace("{{STEP_COUNT}}", String(executionSteps.length))
      .replace("{{EXECUTION_STEPS}}", executionInstructions)
      .replace("{{GOAL}}", goalStr);
  };

  // Copy prompt to clipboard
  const copyPrompt = async () => {
    const prompt = generatePrompt();
    await navigator.clipboard.writeText(prompt);
    setLastResult({ success: true, message: "Prompt copied to clipboard!" });
    setTimeout(() => setLastResult(null), 3000);
  };

  // Run the automation
  const runAutomation = async () => {
    if (executionSteps.length === 0) {
      setLastResult({
        success: false,
        message: "Please add at least one workflow or state to execute",
      });
      return;
    }

    setIsRunning(true);
    setLastResult(null);

    try {
      const prompt = generatePrompt();

      // Add to history
      const historyEntry: PromptHistoryEntry = {
        id: crypto.randomUUID(),
        timestamp: Date.now(),
        steps: [...executionSteps],
        goal: goal.trim() || "Verify automation works correctly",
      };
      setHistory((prev) => [historyEntry, ...prev]);

      // Call the trigger_ai_analysis endpoint
      const response = await fetch("http://localhost:9876/trigger-ai-analysis", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          prompt,
          timeout_seconds: 600,
        }),
      });

      const result = await response.json();

      if (result.success && result.data?.success) {
        setLastResult({ success: true, message: "AI automation started successfully!" });
        setHistory((prev) =>
          prev.map((h) => (h.id === historyEntry.id ? { ...h, success: true } : h)),
        );
      } else {
        setLastResult({
          success: false,
          message: result.error || result.data?.error || result.data?.message || "Failed to start AI automation",
        });
        setHistory((prev) =>
          prev.map((h) => (h.id === historyEntry.id ? { ...h, success: false } : h)),
        );
      }
    } catch (error) {
      setLastResult({
        success: false,
        message: `Error: ${error instanceof Error ? error.message : String(error)}`,
      });
    } finally {
      setIsRunning(false);
    }
  };

  const loadFromHistory = (entry: PromptHistoryEntry) => {
    setExecutionSteps(entry.steps);
    setGoal(entry.goal);
  };

  const getHistorySummary = (entry: PromptHistoryEntry): string => {
    const workflowCount = entry.steps.filter((s) => s.type === "workflow").length;
    const stateCount = entry.steps.filter((s) => s.type === "state").length;
    const parts: string[] = [];
    if (workflowCount > 0) parts.push(`${workflowCount} workflow${workflowCount > 1 ? "s" : ""}`);
    if (stateCount > 0) parts.push(`${stateCount} state${stateCount > 1 ? "s" : ""}`);
    return parts.join(" • ") || "No steps";
  };

  const hasConfig = execution.configLoaded && (states.length > 0 || workflows.length > 0);

  return (
    <div className="p-6 space-y-6">
      {/* Header */}
      <div className="flex items-center gap-3">
        <div className="p-2 bg-primary/10 rounded-lg">
          <Sparkles className="w-6 h-6 text-primary" />
        </div>
        <div>
          <h2 className="text-xl font-semibold">AI Automation Builder</h2>
          <p className="text-sm text-muted-foreground">
            Build ordered execution sequences with visual feedback
          </p>
        </div>
      </div>

      {!hasConfig ? (
        <div className="card p-8 text-center space-y-4">
          <Info className="w-12 h-12 mx-auto text-muted-foreground" />
          <div>
            <h3 className="font-medium">No Configuration Loaded</h3>
            <p className="text-sm text-muted-foreground mt-1">
              Load a workflow configuration in the Run tab to use the AI Builder.
            </p>
          </div>
        </div>
      ) : (
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
          {/* Left Panel - Configuration */}
          <div className="space-y-4">
            {/* Execution Steps */}
            <div className="card p-4 space-y-3">
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2">
                  <Play className="w-4 h-4 text-primary" />
                  <span className="font-medium">Execution Steps</span>
                  <span className="text-xs text-muted-foreground">
                    ({executionSteps.length} step{executionSteps.length !== 1 ? "s" : ""})
                  </span>
                </div>

                {/* Add Step Dropdown */}
                <div className="relative">
                  <button
                    onClick={() => setShowAddDropdown(!showAddDropdown)}
                    className="flex items-center gap-1 px-2 py-1 text-sm bg-primary/10 text-primary rounded hover:bg-primary/20 transition-colors"
                  >
                    <Plus className="w-4 h-4" />
                    Add Step
                    <ChevronDown
                      className={`w-3 h-3 transition-transform ${showAddDropdown ? "rotate-180" : ""}`}
                    />
                  </button>

                  {showAddDropdown && (
                    <div className="absolute right-0 z-20 w-64 mt-1 bg-card border border-border rounded-md shadow-lg max-h-80 overflow-y-auto">
                      {/* Workflows section */}
                      {workflows.length > 0 && (
                        <>
                          <div className="px-3 py-2 text-xs font-semibold text-muted-foreground bg-muted/30 border-b border-border flex items-center gap-2">
                            <Workflow className="w-3 h-3" />
                            Workflows
                          </div>
                          {workflows.map((workflow) => (
                            <button
                              key={workflow.id}
                              onClick={() => addStep("workflow", workflow.name)}
                              className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors"
                            >
                              <Workflow className="w-4 h-4 text-purple-500" />
                              <span>{workflow.name}</span>
                            </button>
                          ))}
                        </>
                      )}

                      {/* States section */}
                      {states.length > 0 && (
                        <>
                          <div className="px-3 py-2 text-xs font-semibold text-muted-foreground bg-muted/30 border-b border-border flex items-center gap-2">
                            <Target className="w-3 h-3" />
                            States
                          </div>
                          {states.map((state) => (
                            <button
                              key={state.name}
                              onClick={() => addStep("state", state.name)}
                              className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors"
                            >
                              <Target className="w-4 h-4 text-primary" />
                              <span>{state.name}</span>
                              {state.images.length > 0 && (
                                <span className="text-xs text-muted-foreground ml-auto">
                                  {state.images.length} img
                                </span>
                              )}
                            </button>
                          ))}
                        </>
                      )}
                    </div>
                  )}
                </div>
              </div>

              {/* Steps List */}
              {executionSteps.length === 0 ? (
                <div className="text-center py-8 text-muted-foreground">
                  <GripVertical className="w-8 h-8 mx-auto mb-2 opacity-30" />
                  <p className="text-sm">No steps added yet</p>
                  <p className="text-xs mt-1">Click "Add Step" to build your execution sequence</p>
                </div>
              ) : (
                <div className="space-y-2">
                  {executionSteps.map((step, index) => (
                    <div
                      key={step.id}
                      className={`flex items-center gap-2 p-2 rounded-md border ${
                        step.type === "workflow"
                          ? "bg-purple-500/5 border-purple-500/20"
                          : "bg-primary/5 border-primary/20"
                      }`}
                    >
                      {/* Step number */}
                      <span className="w-6 h-6 flex items-center justify-center text-xs font-medium rounded bg-background">
                        {index + 1}
                      </span>

                      {/* Icon */}
                      {step.type === "workflow" ? (
                        <Workflow className="w-4 h-4 text-purple-500 flex-shrink-0" />
                      ) : (
                        <Target className="w-4 h-4 text-primary flex-shrink-0" />
                      )}

                      {/* Name */}
                      <span className="flex-1 text-sm truncate">{step.name}</span>

                      {/* Screenshot toggle */}
                      <button
                        onClick={() => toggleStepScreenshot(step.id)}
                        className={`p-1 rounded transition-colors ${
                          step.takeScreenshot
                            ? "text-green-500 hover:text-green-400"
                            : "text-muted-foreground hover:text-foreground"
                        }`}
                        title={step.takeScreenshot ? "Screenshot enabled" : "Screenshot disabled"}
                      >
                        <Camera className="w-4 h-4" />
                      </button>

                      {/* Move up */}
                      <button
                        onClick={() => moveStepUp(index)}
                        disabled={index === 0}
                        className="p-1 text-muted-foreground hover:text-foreground disabled:opacity-30 disabled:cursor-not-allowed transition-colors"
                        title="Move up"
                      >
                        <ChevronUp className="w-4 h-4" />
                      </button>

                      {/* Move down */}
                      <button
                        onClick={() => moveStepDown(index)}
                        disabled={index === executionSteps.length - 1}
                        className="p-1 text-muted-foreground hover:text-foreground disabled:opacity-30 disabled:cursor-not-allowed transition-colors"
                        title="Move down"
                      >
                        <ChevronDown className="w-4 h-4" />
                      </button>

                      {/* Remove */}
                      <button
                        onClick={() => removeStep(step.id)}
                        className="p-1 text-muted-foreground hover:text-red-500 transition-colors"
                        title="Remove step"
                      >
                        <Trash2 className="w-4 h-4" />
                      </button>
                    </div>
                  ))}
                </div>
              )}

              {executionSteps.length > 0 && (
                <p className="text-xs text-muted-foreground">
                  <Camera className="w-3 h-3 inline mr-1" />
                  Click the camera icon to toggle screenshots for each step
                </p>
              )}
            </div>

            {/* Goal */}
            <div className="card p-4 space-y-3">
              <div className="flex items-center gap-2">
                <Sparkles className="w-4 h-4 text-accent" />
                <span className="font-medium">Goal</span>
              </div>

              <textarea
                value={goal}
                onChange={(e) => setGoal(e.target.value)}
                placeholder="Describe what you want to verify or achieve..."
                className="w-full h-24 px-3 py-2 bg-background border border-border rounded-md text-sm resize-none focus:outline-none focus:ring-2 focus:ring-primary"
              />
            </div>

            {/* Actions */}
            <div className="flex gap-3">
              <button
                onClick={runAutomation}
                disabled={isRunning || executionSteps.length === 0}
                className="flex-1 flex items-center justify-center gap-2 px-4 py-3 bg-primary text-primary-foreground rounded-md font-medium hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
              >
                {isRunning ? (
                  <>
                    <Loader2 className="w-4 h-4 animate-spin" />
                    Running...
                  </>
                ) : (
                  <>
                    <Play className="w-4 h-4" />
                    Run AI Loop
                  </>
                )}
              </button>

              <button
                onClick={copyPrompt}
                className="flex items-center gap-2 px-4 py-3 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 transition-colors"
                title="Copy prompt to clipboard"
              >
                <Copy className="w-4 h-4" />
              </button>
            </div>

            {/* Result message */}
            {lastResult && (
              <div
                className={`flex items-center gap-2 p-3 rounded-md ${
                  lastResult.success
                    ? "bg-green-500/10 text-green-500"
                    : "bg-red-500/10 text-red-500"
                }`}
              >
                {lastResult.success ? (
                  <CheckCircle className="w-4 h-4" />
                ) : (
                  <XCircle className="w-4 h-4" />
                )}
                <span className="text-sm">{lastResult.message}</span>
              </div>
            )}
          </div>

          {/* Right Panel - Preview & History */}
          <div className="space-y-4">
            {/* Prompt Preview */}
            <CollapsiblePanel
              title="Prompt Preview"
              icon={<Sparkles className="w-4 h-4" />}
              defaultCollapsed={false}
              storageKey="ai-builder-preview"
            >
              <pre className="text-xs bg-background p-3 rounded-md overflow-auto max-h-64 whitespace-pre-wrap">
                {generatePrompt()}
              </pre>
            </CollapsiblePanel>

            {/* History */}
            <CollapsiblePanel
              title="Recent Runs"
              icon={<History className="w-4 h-4" />}
              defaultCollapsed={true}
              storageKey="ai-builder-history"
            >
              {history.length === 0 ? (
                <p className="text-sm text-muted-foreground p-3">No previous runs</p>
              ) : (
                <div className="space-y-2 max-h-64 overflow-y-auto">
                  {history.slice(0, 10).map((entry) => (
                    <button
                      key={entry.id}
                      onClick={() => loadFromHistory(entry)}
                      className="w-full text-left p-3 bg-background rounded-md hover:bg-muted/30 transition-colors"
                    >
                      <div className="flex items-center gap-2">
                        {entry.success === true && (
                          <CheckCircle className="w-4 h-4 text-green-500" />
                        )}
                        {entry.success === false && <XCircle className="w-4 h-4 text-red-500" />}
                        {entry.success === undefined && (
                          <RefreshCw className="w-4 h-4 text-muted-foreground" />
                        )}
                        <span className="text-sm font-medium truncate">{entry.goal}</span>
                      </div>
                      <div className="text-xs text-muted-foreground mt-1">
                        {getHistorySummary(entry)}
                        {" • "}
                        {new Date(entry.timestamp).toLocaleString()}
                      </div>
                    </button>
                  ))}
                </div>
              )}
            </CollapsiblePanel>

            {/* Available Images */}
            {images.length > 0 && (
              <CollapsiblePanel
                title={`Available Images (${images.length})`}
                icon={<ImageIcon className="w-4 h-4" />}
                defaultCollapsed={true}
                storageKey="ai-builder-images"
              >
                <div className="space-y-1 max-h-48 overflow-y-auto">
                  {images.map((img) => (
                    <div
                      key={`${img.stateName}-${img.name}`}
                      className="flex items-center justify-between text-sm p-2 bg-background rounded"
                    >
                      <span>{img.name}</span>
                      <span className="text-xs text-muted-foreground">{img.stateName}</span>
                    </div>
                  ))}
                </div>
              </CollapsiblePanel>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
