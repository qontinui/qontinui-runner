/**
 * AiBuilderTab.tsx
 *
 * AI Automation Builder panel that allows users to:
 * 1. Select states and images from loaded configuration
 * 2. Enter natural language goals
 * 3. Generate and run AI-powered recursive automation
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

interface PromptHistoryEntry {
  id: string;
  timestamp: number;
  states: string[];
  screenshotStates: string[];
  goal: string;
  success?: boolean;
}

const PROMPT_TEMPLATE = `# Recursive Automation Loop

Execute automation steps, analyze results, fix issues, and recursively continue until success.

## Configuration

**States to Navigate:** {{STATES}}
**Screenshot States:** {{SCREENSHOT_STATES}}
**Goal:** {{GOAL}}

## Instructions

### Step 1: Navigate to Each State

For each state in the list, navigate using the MCP tool:
\`\`\`
mcp__qontinui__go_to_state with state_names=["STATE_NAME"], take_screenshot=true
\`\`\`

### Step 2: Analyze Logs

After completing all state visits, check for errors:
\`\`\`bash
tail -200 /mnt/c/Users/Joshua/Documents/qontinui_parent_directory/.dev-logs/runner-backend.log
grep -i "error\\|exception\\|failed\\|panic" /mnt/c/Users/Joshua/Documents/qontinui_parent_directory/.dev-logs/runner-backend.log | tail -50
\`\`\`

### Step 3: Analyze Screenshots

Read any screenshots saved during navigation to visually inspect the UI state.

### Step 4: Fix Issues

If any errors were found:
1. Identify the root cause from logs and screenshots
2. Read the relevant source code
3. Make the fix
4. Restart affected services if needed

### Step 5: Recursive Continuation

**If fixes were made**, use trigger_ai_analysis to spawn a new session with this same prompt.
**If no issues found**, report success and stop.

## Rules

- ALWAYS analyze logs after navigation
- ALWAYS visually inspect screenshots
- NEVER ask the user to check things manually
- STOP when all navigations succeed with no errors
- MAX ITERATIONS: 10
`;

export function AiBuilderTab() {
  const execution = useExecution();

  // State selections
  const [selectedStates, setSelectedStates] = useState<string[]>([]);
  const [screenshotStates, setScreenshotStates] = useState<string[]>([]);
  const [goal, setGoal] = useState("");

  // UI state
  const [showStatesDropdown, setShowStatesDropdown] = useState(false);
  const [showScreenshotDropdown, setShowScreenshotDropdown] = useState(false);
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

  // Parse states and images from config
  const { states, images } = useMemo(() => {
    const stateList: StateInfo[] = [];
    const imageList: ImageInfo[] = [];

    if (execution.config?.states && Array.isArray(execution.config.states)) {
      for (const state of execution.config.states) {
        // Get state name from name or id field
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

    return { states: stateList, images: imageList };
  }, [execution.config]);

  // Save history to localStorage
  useEffect(() => {
    localStorage.setItem("qontinui-ai-builder-history", JSON.stringify(history.slice(0, 20)));
  }, [history]);

  // Toggle state selection
  const toggleState = (stateName: string, list: "navigate" | "screenshot") => {
    if (list === "navigate") {
      setSelectedStates((prev) =>
        prev.includes(stateName) ? prev.filter((s) => s !== stateName) : [...prev, stateName],
      );
    } else {
      setScreenshotStates((prev) =>
        prev.includes(stateName) ? prev.filter((s) => s !== stateName) : [...prev, stateName],
      );
    }
  };

  // Generate the prompt
  const generatePrompt = () => {
    const statesStr = selectedStates.length > 0 ? selectedStates.join(", ") : "(none selected)";
    const screenshotStr =
      screenshotStates.length > 0 ? screenshotStates.join(", ") : "(all states)";
    const goalStr = goal.trim() || "Verify automation works correctly";

    return PROMPT_TEMPLATE.replace("{{STATES}}", statesStr)
      .replace("{{SCREENSHOT_STATES}}", screenshotStr)
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
    if (selectedStates.length === 0) {
      setLastResult({ success: false, message: "Please select at least one state to navigate" });
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
        states: [...selectedStates],
        screenshotStates: [...screenshotStates],
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

      if (result.success) {
        setLastResult({ success: true, message: "AI automation started successfully!" });
        // Update history entry
        setHistory((prev) =>
          prev.map((h) => (h.id === historyEntry.id ? { ...h, success: true } : h)),
        );
      } else {
        setLastResult({
          success: false,
          message: result.error || "Failed to start AI automation",
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

  // Load from history
  const loadFromHistory = (entry: PromptHistoryEntry) => {
    setSelectedStates(entry.states);
    setScreenshotStates(entry.screenshotStates);
    setGoal(entry.goal);
  };

  const hasConfig = execution.configLoaded && states.length > 0;

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
            Build recursive AI automation loops with visual feedback
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
            {/* States to Navigate */}
            <div className="card p-4 space-y-3">
              <div className="flex items-center gap-2">
                <Target className="w-4 h-4 text-primary" />
                <span className="font-medium">States to Navigate</span>
                <span className="text-xs text-muted-foreground">
                  ({selectedStates.length} selected)
                </span>
              </div>

              <div className="relative">
                <button
                  onClick={() => setShowStatesDropdown(!showStatesDropdown)}
                  className="w-full flex items-center justify-between px-3 py-2 bg-background border border-border rounded-md text-sm hover:bg-muted/30 transition-colors"
                >
                  <span>
                    {selectedStates.length > 0
                      ? selectedStates.join(", ")
                      : "Select states to navigate..."}
                  </span>
                  <ChevronDown
                    className={`w-4 h-4 transition-transform ${showStatesDropdown ? "rotate-180" : ""}`}
                  />
                </button>

                {showStatesDropdown && (
                  <div className="absolute z-10 w-full mt-1 bg-card border border-border rounded-md shadow-lg max-h-60 overflow-y-auto">
                    {states.map((state) => (
                      <button
                        key={state.name}
                        onClick={() => toggleState(state.name, "navigate")}
                        className={`w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors ${
                          selectedStates.includes(state.name) ? "bg-primary/10" : ""
                        }`}
                      >
                        <div
                          className={`w-4 h-4 border rounded flex items-center justify-center ${
                            selectedStates.includes(state.name)
                              ? "bg-primary border-primary"
                              : "border-border"
                          }`}
                        >
                          {selectedStates.includes(state.name) && (
                            <CheckCircle className="w-3 h-3 text-white" />
                          )}
                        </div>
                        <span>{state.name}</span>
                        {state.images.length > 0 && (
                          <span className="text-xs text-muted-foreground ml-auto">
                            {state.images.length} images
                          </span>
                        )}
                      </button>
                    ))}
                  </div>
                )}
              </div>
            </div>

            {/* Screenshot States */}
            <div className="card p-4 space-y-3">
              <div className="flex items-center gap-2">
                <ImageIcon className="w-4 h-4 text-secondary" />
                <span className="font-medium">Screenshot States</span>
                <span className="text-xs text-muted-foreground">(optional)</span>
              </div>

              <div className="relative">
                <button
                  onClick={() => setShowScreenshotDropdown(!showScreenshotDropdown)}
                  className="w-full flex items-center justify-between px-3 py-2 bg-background border border-border rounded-md text-sm hover:bg-muted/30 transition-colors"
                >
                  <span>
                    {screenshotStates.length > 0
                      ? screenshotStates.join(", ")
                      : "All states (default)"}
                  </span>
                  <ChevronDown
                    className={`w-4 h-4 transition-transform ${showScreenshotDropdown ? "rotate-180" : ""}`}
                  />
                </button>

                {showScreenshotDropdown && (
                  <div className="absolute z-10 w-full mt-1 bg-card border border-border rounded-md shadow-lg max-h-60 overflow-y-auto">
                    {states.map((state) => (
                      <button
                        key={state.name}
                        onClick={() => toggleState(state.name, "screenshot")}
                        className={`w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors ${
                          screenshotStates.includes(state.name) ? "bg-secondary/10" : ""
                        }`}
                      >
                        <div
                          className={`w-4 h-4 border rounded flex items-center justify-center ${
                            screenshotStates.includes(state.name)
                              ? "bg-secondary border-secondary"
                              : "border-border"
                          }`}
                        >
                          {screenshotStates.includes(state.name) && (
                            <CheckCircle className="w-3 h-3 text-white" />
                          )}
                        </div>
                        <span>{state.name}</span>
                      </button>
                    ))}
                  </div>
                )}
              </div>
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
                disabled={isRunning || selectedStates.length === 0}
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
                        {entry.states.length} states • {new Date(entry.timestamp).toLocaleString()}
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
