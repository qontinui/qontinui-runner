/**
 * ExecuteTab.tsx
 *
 * Unified execution page that provides access to all runnable task types:
 * - GUI Automation (workflows from loaded config)
 * - AI Tasks (from Library)
 * - AI Workflows (from Library)
 * - Playwright Scripts (from Library)
 * - Verifications (from Library, requires config)
 */

import { useState, useEffect, useCallback } from "react";
import {
  Play,
  Loader2,
  FileText,
  Sparkles,
  TestTube,
  ShieldCheck,
  Cpu,
  RefreshCw,
  ChevronDown as _ChevronDown,
  Settings,
  FolderOpen,
} from "lucide-react";
import CollapsiblePanel from "./CollapsiblePanel";
import { ConfigurationPanel } from "./ConfigurationPanel";
import { ExecutionControlPanel } from "./ExecutionControlPanel";
import { MonitorSelector as _MonitorSelector } from "./MonitorSelector";
import { useExecution } from "../contexts/ExecutionContext";
import type { SavedVerification, VerificationTaskConfig } from "../types";
import { useVerificationAgent } from "../hooks/useVerificationAgent";

type LogLevel = "info" | "warning" | "error" | "debug" | "success";

type TaskType = "gui" | "tasks" | "workflows" | "scripts" | "verifications";

interface SavedPrompt {
  id: string;
  name: string;
  description: string;
  content: string;
  category: string;
  tags: string[];
  max_sessions: number | null;
  created_at: string;
  modified_at: string;
}

interface ExecutionStep {
  id: string;
  type: "workflow" | "state" | "playwright" | "prompt" | "action" | "screenshot";
  name: string;
  takeScreenshot: boolean;
  screenshotDelay?: number;
  screenshotMonitor?: number | "all" | null;
  playwrightScriptId?: string;
  playwrightScriptContent?: string;
  playwrightTargetUrl?: string;
  promptId?: string;
  promptContent?: string;
  actionType?: "click" | "double_click" | "right_click";
  targetImageId?: string;
  targetImageName?: string;
}

interface SavedAiWorkflow {
  id: string;
  name: string;
  description: string;
  steps: ExecutionStep[];
  goal: string;
  max_iterations: number;
  persistent_session: boolean;
  capture_input_validation: boolean;
  category: string;
  tags: string[];
  created_at: string;
  modified_at: string;
}

interface PlaywrightScript {
  id: string;
  name: string;
  description: string;
  target_url: string;
  script_content: string;
  created_at: string;
  modified_at: string;
}

interface TaskTypeConfig {
  id: TaskType;
  label: string;
  icon: React.ComponentType<{ className?: string }>;
  color: string;
  description: string;
  requiresConfig: boolean;
}

const TASK_TYPES: TaskTypeConfig[] = [
  {
    id: "gui",
    label: "GUI Automation",
    icon: Cpu,
    color: "text-blue-500",
    description: "Run workflows from loaded config",
    requiresConfig: true,
  },
  {
    id: "tasks",
    label: "AI Tasks",
    icon: FileText,
    color: "text-amber-500",
    description: "Single-step AI prompts",
    requiresConfig: false,
  },
  {
    id: "workflows",
    label: "AI Workflows",
    icon: Sparkles,
    color: "text-green-500",
    description: "Multi-step AI workflows",
    requiresConfig: false,
  },
  {
    id: "scripts",
    label: "Scripts",
    icon: TestTube,
    color: "text-purple-500",
    description: "Playwright browser tests",
    requiresConfig: false,
  },
  {
    id: "verifications",
    label: "Verifications",
    icon: ShieldCheck,
    color: "text-emerald-500",
    description: "State verification runs",
    requiresConfig: true,
  },
];

const API_BASE = "http://localhost:9876";

interface ExecuteTabProps {
  onLog: (level: LogLevel, message: string) => void;
  onNavigateToActive: () => void;
}

export function ExecuteTab({ onLog, onNavigateToActive }: ExecuteTabProps) {
  const execution = useExecution();
  const { startVerification, verificationRunning } = useVerificationAgent();

  // Task type selection
  const [selectedTaskType, setSelectedTaskType] = useState<TaskType>("gui");

  // Data state
  const [prompts, setPrompts] = useState<SavedPrompt[]>([]);
  const [aiWorkflows, setAiWorkflows] = useState<SavedAiWorkflow[]>([]);
  const [scripts, setScripts] = useState<PlaywrightScript[]>([]);
  const [verifications, setVerifications] = useState<SavedVerification[]>([]);
  const [loading, setLoading] = useState(true);
  const [runningId, setRunningId] = useState<string | null>(null);

  // UI state for workflow dropdown
  const [showWorkflowDropdown, setShowWorkflowDropdown] = useState(false);

  // Fetch library items
  const fetchLibraryItems = useCallback(async () => {
    setLoading(true);
    try {
      const [promptsRes, workflowsRes, scriptsRes, verificationsRes] = await Promise.all([
        fetch(`${API_BASE}/prompts`).catch(() => ({ ok: false })),
        fetch(`${API_BASE}/ai-workflows`).catch(() => ({ ok: false })),
        fetch(`${API_BASE}/playwright/scripts`).catch(() => ({ ok: false })),
        fetch(`${API_BASE}/verifications`).catch(() => ({ ok: false })),
      ]);

      const [promptsData, workflowsData, scriptsData, verificationsData] = await Promise.all([
        promptsRes.ok ? (promptsRes as Response).json() : { success: false },
        workflowsRes.ok ? (workflowsRes as Response).json() : { success: false },
        scriptsRes.ok ? (scriptsRes as Response).json() : { success: false },
        verificationsRes.ok ? (verificationsRes as Response).json() : { success: false },
      ]);

      // Sort by modified_at descending (most recent first)
      const sortByDate = <T extends { modified_at: string }>(items: T[]): T[] =>
        [...items].sort(
          (a, b) => new Date(b.modified_at).getTime() - new Date(a.modified_at).getTime(),
        );

      if (promptsData.success) setPrompts(sortByDate(promptsData.data || []));
      if (workflowsData.success) setAiWorkflows(sortByDate(workflowsData.data || []));
      if (scriptsData.success) setScripts(sortByDate(scriptsData.data || []));
      if (verificationsData.success) setVerifications(verificationsData.data || []);
    } catch (error) {
      console.error("Failed to fetch library items:", error);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchLibraryItems();
  }, [fetchLibraryItems]);

  // Run handlers
  const runTask = async (task: SavedPrompt) => {
    setRunningId(task.id);
    try {
      const response = await fetch(`${API_BASE}/sessions/start`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: task.name,
          prompt: task.content,
          uses_gui: false,
          timeout_seconds: 1800,
        }),
      });

      const result = await response.json();
      if (result.success) {
        onLog("success", `Started task: ${task.name}`);
        onNavigateToActive();
      } else {
        throw new Error(result.error || "Failed to run task");
      }
    } catch (error) {
      onLog("error", `Failed to run task: ${error}`);
    } finally {
      setRunningId(null);
    }
  };

  const runAiWorkflow = async (workflow: SavedAiWorkflow) => {
    setRunningId(workflow.id);
    try {
      const prompt = generateWorkflowPrompt(workflow);

      // Determine if this session uses GUI (has any execution steps that need GUI)
      const guiStepTypes = ["workflow", "state", "action", "screenshot"];
      const usesGui = workflow.steps.some((step) => guiStepTypes.includes(step.type));

      // Convert ExecutionStep[] to ExecutionStepConfig[] for deterministic execution
      const executionStepsConfig = workflow.steps.map((step) => ({
        type: step.type,
        name: step.name,
        actionType: step.actionType || null,
        targetImageId: step.targetImageId || null,
        targetImageName: step.targetImageName || null,
        monitorIndex: null,
        takeScreenshot: step.takeScreenshot,
        screenshotDelay: step.screenshotDelay || 0,
        screenshotMonitor: step.screenshotMonitor ?? null,
        playwrightScriptId: step.playwrightScriptId || null,
        promptContent: step.promptContent || null,
      }));

      const response = await fetch(`${API_BASE}/sessions/start`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: workflow.name,
          prompt: prompt,
          total_phases: workflow.max_iterations,
          uses_gui: usesGui,
          timeout_seconds: 1800,
          execution_steps: executionStepsConfig,
        }),
      });

      const result = await response.json();
      if (result.success) {
        onLog("success", `Started workflow: ${workflow.name}`);
        onNavigateToActive();
      } else {
        throw new Error(result.error || "Failed to run workflow");
      }
    } catch (error) {
      onLog("error", `Failed to run workflow: ${error}`);
    } finally {
      setRunningId(null);
    }
  };

  const runScript = async (script: PlaywrightScript) => {
    setRunningId(script.id);
    try {
      const response = await fetch(`${API_BASE}/playwright/scripts/${script.id}/run`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({}),
      });

      const result = await response.json();
      if (result.success) {
        onLog("success", `Started script: ${script.name}`);
        onNavigateToActive();
      } else {
        throw new Error(result.error || "Failed to run script");
      }
    } catch (error) {
      onLog("error", `Failed to run script: ${error}`);
    } finally {
      setRunningId(null);
    }
  };

  const runVerification = async (verification: SavedVerification) => {
    if (!execution.config) {
      onLog("error", "No configuration loaded. Please load a config first.");
      return;
    }

    setRunningId(verification.id);
    try {
      const configPath = execution.config.path || execution.config.name || "";
      const config: VerificationTaskConfig = {
        config_path: configPath,
        strategy: verification.config.strategy,
        max_states: verification.config.max_states,
        max_duration_seconds: verification.config.max_duration_seconds,
        target_state_ids: verification.config.target_state_ids,
        target_transition_ids: verification.config.target_transition_ids,
        monitor_index:
          execution.selectedMonitors.length > 0 ? execution.selectedMonitors[0] : undefined,
        capture_screenshots: verification.config.capture_screenshots,
        capture_transition_screenshots: verification.config.capture_transition_screenshots,
        state_delay_ms: verification.config.state_delay_ms,
        stop_on_first_failure: verification.config.stop_on_first_failure,
      };

      const result = await startVerification(config);
      if (result) {
        onLog("success", `Started verification: ${verification.name}`);
        onNavigateToActive();
      }
    } catch (error) {
      onLog("error", `Failed to run verification: ${error}`);
    } finally {
      setRunningId(null);
    }
  };

  const generateWorkflowPrompt = (workflow: SavedAiWorkflow): string => {
    const lines: string[] = [];
    lines.push(`# AI Automation Task: ${workflow.name}`);
    lines.push("");
    lines.push(`## Goal`);
    lines.push(workflow.goal || "(No goal specified)");
    lines.push("");
    lines.push(`## Execution Steps`);
    workflow.steps.forEach((step, index) => {
      lines.push(
        `${index + 1}. [${step.type}] ${step.name}${step.takeScreenshot ? " (screenshot)" : ""}`,
      );
    });
    lines.push("");
    lines.push(`## Settings`);
    lines.push(`- Max Iterations: ${workflow.max_iterations}`);
    lines.push(`- Mode: ${workflow.persistent_session ? "Persistent Session" : "Standard"}`);
    return lines.join("\n");
  };

  // Get current task type config
  const currentTaskType = TASK_TYPES.find((t) => t.id === selectedTaskType)!;
  const TaskIcon = currentTaskType.icon;

  // Render task type content
  const renderTaskContent = () => {
    const configLoaded = execution.configLoaded;

    switch (selectedTaskType) {
      case "gui":
        return (
          <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
            <ConfigurationPanel
              config={execution.config}
              onLoadConfiguration={execution.loadConfiguration}
              onLoadLastConfiguration={execution.loadLastConfiguration}
              onLog={onLog}
            />
            <ExecutionControlPanel
              workflows={execution.workflows}
              selectedWorkflow={execution.selectedWorkflow}
              configLoaded={execution.configLoaded}
              showWorkflowDropdown={showWorkflowDropdown}
              onWorkflowDropdownToggle={setShowWorkflowDropdown}
              onWorkflowSelect={(id) => {
                execution.selectWorkflowWithPersistence(id);
                setShowWorkflowDropdown(false);
              }}
              selectedMonitors={execution.selectedMonitors}
              onMonitorSelectionChange={(indices) => {
                if (indices.length > 0) {
                  execution.selectMonitorsWithPersistence(indices);
                }
              }}
              autoMinimize={execution.autoMinimize}
              onAutoMinimizeChange={execution.setAutoMinimize}
              executionActive={execution.executionActive}
              onStartExecution={execution.startExecution}
              onStopExecution={execution.stopExecution}
              onNavigateToActive={onNavigateToActive}
              states={execution.config?.states}
              resolvedInitialStates={execution.resolvedInitialStates}
              initialStatesOverride={execution.initialStatesOverride}
              onInitialStatesOverrideChange={execution.setInitialStatesOverride}
            />
          </div>
        );

      case "tasks":
        return (
          <div className="grid grid-cols-1 lg:grid-cols-2 xl:grid-cols-3 gap-4">
            {loading ? (
              <div className="col-span-full flex items-center justify-center py-12">
                <Loader2 className="w-8 h-8 animate-spin text-muted-foreground" />
              </div>
            ) : prompts.length === 0 ? (
              <div className="col-span-full text-center py-12 text-muted-foreground">
                <FolderOpen className="w-12 h-12 mx-auto mb-4 opacity-50" />
                <p className="text-lg mb-2">No AI tasks found</p>
                <p className="text-sm">Create tasks in the Library to run them here.</p>
              </div>
            ) : (
              prompts.map((task) => (
                <div
                  key={task.id}
                  className="p-4 border border-border rounded-lg bg-card hover:border-amber-500/50 transition-colors"
                >
                  <div className="flex items-start gap-3">
                    <FileText className="w-5 h-5 text-amber-500 flex-shrink-0 mt-0.5" />
                    <div className="flex-1 min-w-0">
                      <h4 className="font-medium truncate">{task.name}</h4>
                      {task.description && (
                        <p className="text-sm text-muted-foreground line-clamp-3">
                          {task.description}
                        </p>
                      )}
                    </div>
                    <button
                      onClick={() => runTask(task)}
                      disabled={runningId === task.id}
                      className="flex-shrink-0 p-2 bg-amber-500 text-white rounded-lg hover:bg-amber-600 transition-colors disabled:opacity-50"
                      title="Run Task"
                    >
                      {runningId === task.id ? (
                        <Loader2 className="w-4 h-4 animate-spin" />
                      ) : (
                        <Play className="w-4 h-4" />
                      )}
                    </button>
                  </div>
                </div>
              ))
            )}
          </div>
        );

      case "workflows":
        return (
          <div className="grid grid-cols-1 lg:grid-cols-2 xl:grid-cols-3 gap-4">
            {loading ? (
              <div className="col-span-full flex items-center justify-center py-12">
                <Loader2 className="w-8 h-8 animate-spin text-muted-foreground" />
              </div>
            ) : aiWorkflows.length === 0 ? (
              <div className="col-span-full text-center py-12 text-muted-foreground">
                <FolderOpen className="w-12 h-12 mx-auto mb-4 opacity-50" />
                <p className="text-lg mb-2">No AI workflows found</p>
                <p className="text-sm">
                  Create workflows in the Workflow Builder to run them here.
                </p>
              </div>
            ) : (
              aiWorkflows.map((workflow) => (
                <div
                  key={workflow.id}
                  className="p-4 border border-border rounded-lg bg-card hover:border-green-500/50 transition-colors"
                >
                  <div className="flex items-start gap-3">
                    <Sparkles className="w-5 h-5 text-green-500 flex-shrink-0 mt-0.5" />
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <h4 className="font-medium truncate">{workflow.name}</h4>
                        <span className="text-xs bg-muted px-1.5 py-0.5 rounded">
                          {workflow.steps.length} steps
                        </span>
                      </div>
                      {workflow.goal && (
                        <p className="text-sm text-muted-foreground line-clamp-3">
                          {workflow.goal}
                        </p>
                      )}
                    </div>
                    <button
                      onClick={() => runAiWorkflow(workflow)}
                      disabled={runningId === workflow.id}
                      className="flex-shrink-0 p-2 bg-green-500 text-white rounded-lg hover:bg-green-600 transition-colors disabled:opacity-50"
                      title="Run Workflow"
                    >
                      {runningId === workflow.id ? (
                        <Loader2 className="w-4 h-4 animate-spin" />
                      ) : (
                        <Play className="w-4 h-4" />
                      )}
                    </button>
                  </div>
                </div>
              ))
            )}
          </div>
        );

      case "scripts":
        return (
          <div className="grid grid-cols-1 lg:grid-cols-2 xl:grid-cols-3 gap-4">
            {loading ? (
              <div className="col-span-full flex items-center justify-center py-12">
                <Loader2 className="w-8 h-8 animate-spin text-muted-foreground" />
              </div>
            ) : scripts.length === 0 ? (
              <div className="col-span-full text-center py-12 text-muted-foreground">
                <FolderOpen className="w-12 h-12 mx-auto mb-4 opacity-50" />
                <p className="text-lg mb-2">No Playwright scripts found</p>
                <p className="text-sm">Create scripts in the Script Builder to run them here.</p>
              </div>
            ) : (
              scripts.map((script) => (
                <div
                  key={script.id}
                  className="p-4 border border-border rounded-lg bg-card hover:border-purple-500/50 transition-colors"
                >
                  <div className="flex items-start gap-3">
                    <TestTube className="w-5 h-5 text-purple-500 flex-shrink-0 mt-0.5" />
                    <div className="flex-1 min-w-0">
                      <h4 className="font-medium truncate">{script.name}</h4>
                      {script.description && (
                        <p className="text-sm text-muted-foreground line-clamp-3">
                          {script.description}
                        </p>
                      )}
                    </div>
                    <button
                      onClick={() => runScript(script)}
                      disabled={runningId === script.id}
                      className="flex-shrink-0 p-2 bg-purple-500 text-white rounded-lg hover:bg-purple-600 transition-colors disabled:opacity-50"
                      title="Run Script"
                    >
                      {runningId === script.id ? (
                        <Loader2 className="w-4 h-4 animate-spin" />
                      ) : (
                        <Play className="w-4 h-4" />
                      )}
                    </button>
                  </div>
                </div>
              ))
            )}
          </div>
        );

      case "verifications":
        return (
          <div className="space-y-6">
            {/* Config Panel for Verifications */}
            <div className="max-w-2xl">
              <CollapsiblePanel
                title="Configuration Required"
                icon={<Settings className="w-4 h-4" />}
                collapsible={false}
              >
                <div className="space-y-4">
                  <div className="p-3 bg-muted/30 rounded-lg">
                    <div className="flex items-center gap-2 text-sm">
                      <Settings className="w-4 h-4 text-muted-foreground" />
                      <span className="text-muted-foreground">Current Config:</span>
                      <span className="font-medium">
                        {execution.config?.name || "No config loaded"}
                      </span>
                    </div>
                    {!execution.config && (
                      <p className="text-xs text-amber-500 mt-2">
                        Load a configuration to run verifications against it.
                      </p>
                    )}
                  </div>
                  <div className="flex gap-2">
                    <button
                      onClick={execution.loadConfiguration}
                      className="flex-1 btn-secondary flex items-center justify-center gap-2"
                    >
                      <FolderOpen className="w-4 h-4" />
                      Load Config
                    </button>
                    <button
                      onClick={execution.loadLastConfiguration}
                      className="flex-1 btn-secondary flex items-center justify-center gap-2"
                    >
                      <RefreshCw className="w-4 h-4" />
                      Load Last
                    </button>
                  </div>
                </div>
              </CollapsiblePanel>
            </div>

            {/* Verification Cards */}
            <div className="grid grid-cols-1 lg:grid-cols-2 xl:grid-cols-3 gap-4">
              {loading ? (
                <div className="col-span-full flex items-center justify-center py-12">
                  <Loader2 className="w-8 h-8 animate-spin text-muted-foreground" />
                </div>
              ) : verifications.length === 0 ? (
                <div className="col-span-full text-center py-12 text-muted-foreground">
                  <FolderOpen className="w-12 h-12 mx-auto mb-4 opacity-50" />
                  <p className="text-lg mb-2">No verifications found</p>
                  <p className="text-sm">
                    Create verification configs in the Library to run them here.
                  </p>
                </div>
              ) : (
                verifications.map((verification) => (
                  <div
                    key={verification.id}
                    className={`p-4 border rounded-lg bg-card transition-colors ${
                      configLoaded
                        ? "border-border hover:border-emerald-500/50"
                        : "border-border/50 opacity-60"
                    }`}
                  >
                    <div className="flex items-start gap-3">
                      <ShieldCheck className="w-5 h-5 text-emerald-500 flex-shrink-0 mt-0.5" />
                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-2">
                          <h4 className="font-medium truncate">{verification.name}</h4>
                          <span className="text-xs bg-emerald-500/20 text-emerald-600 px-1.5 py-0.5 rounded">
                            {verification.config.strategy}
                          </span>
                        </div>
                        {verification.description && (
                          <p className="text-sm text-muted-foreground truncate">
                            {verification.description}
                          </p>
                        )}
                        <div className="flex gap-3 mt-2 text-xs text-muted-foreground">
                          <span>Max: {verification.config.max_states || "All"} states</span>
                          {verification.run_count > 0 && <span>{verification.run_count} runs</span>}
                        </div>
                      </div>
                    </div>
                    <button
                      onClick={() => runVerification(verification)}
                      disabled={
                        !configLoaded || runningId === verification.id || verificationRunning
                      }
                      className="w-full mt-3 flex items-center justify-center gap-2 py-2 bg-emerald-500 text-white rounded-lg hover:bg-emerald-600 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                    >
                      {runningId === verification.id || verificationRunning ? (
                        <Loader2 className="w-4 h-4 animate-spin" />
                      ) : (
                        <Play className="w-4 h-4" />
                      )}
                      {!configLoaded ? "Load Config First" : "Run Verification"}
                    </button>
                  </div>
                ))
              )}
            </div>
          </div>
        );
    }
  };

  return (
    <div className="p-6 space-y-6 overflow-y-auto h-full">
      {/* Task Type Selector */}
      <div className="flex flex-wrap gap-2 border-b border-border pb-4">
        {TASK_TYPES.map((taskType) => {
          const Icon = taskType.icon;
          const count =
            taskType.id === "gui"
              ? execution.workflows.length
              : taskType.id === "tasks"
                ? prompts.length
                : taskType.id === "workflows"
                  ? aiWorkflows.length
                  : taskType.id === "scripts"
                    ? scripts.length
                    : verifications.length;

          return (
            <button
              key={taskType.id}
              onClick={() => setSelectedTaskType(taskType.id)}
              className={`flex items-center gap-2 px-4 py-2 text-sm font-medium rounded-lg transition-colors ${
                selectedTaskType === taskType.id
                  ? "bg-primary text-primary-foreground"
                  : "bg-muted hover:bg-muted/80 text-muted-foreground"
              }`}
            >
              <Icon
                className={`w-4 h-4 ${selectedTaskType === taskType.id ? "" : taskType.color}`}
              />
              {taskType.label}
              {count > 0 && (
                <span
                  className={`text-xs px-1.5 py-0.5 rounded ${
                    selectedTaskType === taskType.id
                      ? "bg-primary-foreground/20 text-primary-foreground"
                      : "bg-background"
                  }`}
                >
                  {count}
                </span>
              )}
            </button>
          );
        })}

        {/* Refresh button */}
        <button
          onClick={fetchLibraryItems}
          disabled={loading}
          className="ml-auto p-2 text-muted-foreground hover:text-foreground transition-colors"
          title="Refresh library items"
        >
          <RefreshCw className={`w-4 h-4 ${loading ? "animate-spin" : ""}`} />
        </button>
      </div>

      {/* Task Type Description */}
      <div className="flex items-center gap-3 text-sm text-muted-foreground">
        <TaskIcon className={`w-5 h-5 ${currentTaskType.color}`} />
        <span>{currentTaskType.description}</span>
        {currentTaskType.requiresConfig && !execution.configLoaded && (
          <span className="text-amber-500">(Requires loaded configuration)</span>
        )}
      </div>

      {/* Task Content */}
      {renderTaskContent()}
    </div>
  );
}

export default ExecuteTab;
