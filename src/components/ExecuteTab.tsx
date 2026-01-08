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
  ChevronDown,
  Settings,
  FolderOpen,
  MousePointer2,
} from "lucide-react";
import { ConfigurationPanel } from "./ConfigurationPanel";
import { ExecutionControlPanel } from "./ExecutionControlPanel";
import { MonitorSelector as _MonitorSelector } from "./MonitorSelector";
import { QuickActionsPanel } from "./QuickActionsPanel";
import { useExecution } from "../contexts/ExecutionContext";
import type { SavedVerification, VerificationTaskConfig, SavedGuiWorkflow } from "../types";
import { useVerificationAgent } from "../hooks/useVerificationAgent";

type LogLevel = "info" | "warning" | "error" | "debug" | "success";

type TaskType = "gui" | "gui-workflows" | "tasks" | "workflows" | "scripts" | "verifications";

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
    id: "gui-workflows",
    label: "GUI Workflows",
    icon: MousePointer2,
    color: "text-orange-500",
    description: "Sequential GUI action workflows",
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
  const [guiWorkflows, setGuiWorkflows] = useState<SavedGuiWorkflow[]>([]);
  const [scripts, setScripts] = useState<PlaywrightScript[]>([]);
  const [verifications, setVerifications] = useState<SavedVerification[]>([]);
  const [loading, setLoading] = useState(true);
  const [runningId, setRunningId] = useState<string | null>(null);

  // UI state for workflow dropdown
  const [showWorkflowDropdown, setShowWorkflowDropdown] = useState(false);

  // UI state for GUI workflow selection
  const [selectedGuiWorkflowId, setSelectedGuiWorkflowId] = useState<string | null>(null);
  const [showGuiWorkflowDropdown, setShowGuiWorkflowDropdown] = useState(false);

  // Get selected GUI workflow
  const selectedGuiWorkflow = guiWorkflows.find((w) => w.id === selectedGuiWorkflowId) || null;

  // Fetch library items
  const fetchLibraryItems = useCallback(async () => {
    setLoading(true);
    try {
      const [promptsRes, workflowsRes, guiWorkflowsRes, scriptsRes, verificationsRes] =
        await Promise.all([
          fetch(`${API_BASE}/prompts`).catch(() => ({ ok: false })),
          fetch(`${API_BASE}/ai-workflows`).catch(() => ({ ok: false })),
          fetch(`${API_BASE}/gui-workflows`).catch(() => ({ ok: false })),
          fetch(`${API_BASE}/playwright/scripts`).catch(() => ({ ok: false })),
          fetch(`${API_BASE}/verifications`).catch(() => ({ ok: false })),
        ]);

      const [promptsData, workflowsData, guiWorkflowsData, scriptsData, verificationsData] =
        await Promise.all([
          promptsRes.ok ? (promptsRes as Response).json() : { success: false },
          workflowsRes.ok ? (workflowsRes as Response).json() : { success: false },
          guiWorkflowsRes.ok ? (guiWorkflowsRes as Response).json() : { success: false },
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
      if (guiWorkflowsData.success) setGuiWorkflows(sortByDate(guiWorkflowsData.data || []));
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

  const runGuiWorkflow = async (workflow: SavedGuiWorkflow) => {
    setRunningId(workflow.id);
    try {
      const response = await fetch(`${API_BASE}/gui-workflows/${workflow.id}/run`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({}),
      });

      const result = await response.json();
      if (result.success) {
        const data = result.data;
        if (data.failed_steps === 0) {
          onLog(
            "success",
            `Completed GUI workflow: ${workflow.name} (${data.successful_steps}/${data.total_steps} steps in ${data.duration_ms}ms)`,
          );
        } else {
          onLog(
            "warning",
            `GUI workflow completed with errors: ${data.failed_steps}/${data.total_steps} steps failed`,
          );
        }
        onNavigateToActive();
      } else {
        throw new Error(result.error || "Failed to run GUI workflow");
      }
    } catch (error) {
      onLog("error", `Failed to run GUI workflow: ${error}`);
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
              automationEnabledCategories={execution.automationEnabledCategories}
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

      case "gui-workflows":
        return (
          <div className="space-y-4">
            {/* Config Panel for GUI Workflows */}
            <div className="panel max-w-2xl">
              <div className="panel-header">
                <div className="panel-title">
                  <Settings className="w-4 h-4 text-muted-foreground" />
                  <span>Configuration</span>
                </div>
              </div>
              <div className="panel-content space-y-3">
                <div className="flex items-center gap-2 text-sm">
                  <span className="text-muted-foreground">Current:</span>
                  <span className="font-medium">
                    {execution.config?.name || "No config loaded"}
                  </span>
                  {!execution.config && (
                    <span className="badge badge-warning text-[10px]">Required for targeting</span>
                  )}
                </div>
                <div className="flex gap-2">
                  <button
                    onClick={execution.loadConfiguration}
                    className="flex-1 flex items-center justify-center gap-2 px-3 py-1.5 text-xs rounded-lg bg-muted/50 hover:bg-muted transition-colors"
                  >
                    <FolderOpen className="w-3.5 h-3.5" />
                    Load Config
                  </button>
                  <button
                    onClick={execution.loadLastConfiguration}
                    className="flex-1 flex items-center justify-center gap-2 px-3 py-1.5 text-xs rounded-lg bg-muted/50 hover:bg-muted transition-colors"
                  >
                    <RefreshCw className="w-3.5 h-3.5" />
                    Load Last
                  </button>
                </div>
              </div>
            </div>

            {/* Run Workflow Panel */}
            <div className="panel max-w-2xl">
              <div className="panel-header">
                <div className="panel-title">
                  <MousePointer2 className="w-4 h-4 text-orange-500" />
                  <span>Run GUI Workflow</span>
                </div>
              </div>
              <div className="panel-content space-y-3">
                {/* Workflow Selector */}
                <div className="relative">
                  <button
                    onClick={() => setShowGuiWorkflowDropdown(!showGuiWorkflowDropdown)}
                    disabled={guiWorkflows.length === 0}
                    className="w-full flex items-center justify-between px-3 py-2 text-sm bg-muted/50 border border-border rounded-lg hover:bg-muted transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    <span
                      className={selectedGuiWorkflow ? "text-foreground" : "text-muted-foreground"}
                    >
                      {selectedGuiWorkflow
                        ? `${selectedGuiWorkflow.name} (${selectedGuiWorkflow.steps.length} steps)`
                        : guiWorkflows.length === 0
                          ? "No workflows available"
                          : "Select a workflow..."}
                    </span>
                    <ChevronDown
                      className={`w-4 h-4 transition-transform ${showGuiWorkflowDropdown ? "rotate-180" : ""}`}
                    />
                  </button>

                  {/* Dropdown */}
                  {showGuiWorkflowDropdown && guiWorkflows.length > 0 && (
                    <div className="absolute z-20 w-full mt-1 bg-card border border-border rounded-lg shadow-lg max-h-64 overflow-y-auto">
                      {guiWorkflows.map((workflow) => (
                        <button
                          key={workflow.id}
                          onClick={() => {
                            setSelectedGuiWorkflowId(workflow.id);
                            setShowGuiWorkflowDropdown(false);
                          }}
                          className={`w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/50 transition-colors ${
                            selectedGuiWorkflowId === workflow.id ? "bg-orange-500/10" : ""
                          }`}
                        >
                          <MousePointer2 className="w-4 h-4 text-orange-500 flex-shrink-0" />
                          <div className="flex-1 min-w-0">
                            <div className="flex items-center gap-2">
                              <span className="font-medium truncate">{workflow.name}</span>
                              <span className="text-[10px] bg-orange-500/20 text-orange-400 px-1.5 py-0.5 rounded flex-shrink-0">
                                {workflow.steps.length} steps
                              </span>
                            </div>
                            {workflow.description && (
                              <p className="text-xs text-muted-foreground truncate">
                                {workflow.description}
                              </p>
                            )}
                          </div>
                        </button>
                      ))}
                    </div>
                  )}
                </div>

                {/* Run Button */}
                <button
                  onClick={() => selectedGuiWorkflow && runGuiWorkflow(selectedGuiWorkflow)}
                  disabled={!selectedGuiWorkflow || !execution.configLoaded || runningId !== null}
                  className="w-full flex items-center justify-center gap-2 px-4 py-2.5 text-sm font-medium bg-orange-500 text-white rounded-lg hover:bg-orange-600 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                >
                  {runningId !== null && runningId === selectedGuiWorkflowId ? (
                    <Loader2 className="w-4 h-4 animate-spin" />
                  ) : (
                    <Play className="w-4 h-4" />
                  )}
                  {!execution.configLoaded
                    ? "Load Config First"
                    : !selectedGuiWorkflow
                      ? "Select a Workflow"
                      : "Run Workflow"}
                </button>
              </div>
            </div>

            {/* Quick Actions Panel */}
            <div className="max-w-2xl">
              <QuickActionsPanel onLog={onLog} />
            </div>

            {/* GUI Workflow Cards */}
            <div className="grid grid-cols-1 lg:grid-cols-2 xl:grid-cols-3 gap-3">
              {loading ? (
                <div className="col-span-full empty-state">
                  <Loader2 className="empty-state-icon animate-spin" />
                </div>
              ) : guiWorkflows.length === 0 ? (
                <div className="col-span-full empty-state">
                  <FolderOpen className="empty-state-icon" />
                  <p className="empty-state-title">No GUI workflows found</p>
                  <p className="empty-state-desc">
                    Create GUI workflows in the GUI Workflow Builder to run them here.
                  </p>
                </div>
              ) : (
                guiWorkflows.map((workflow) => (
                  <div
                    key={workflow.id}
                    className={`item-card ${!execution.configLoaded ? "opacity-60" : ""}`}
                  >
                    <div className="item-card-header">
                      <MousePointer2 className="item-card-icon text-orange-500" />
                      <div className="item-card-content">
                        <div className="flex items-center gap-2">
                          <h4 className="item-card-title">{workflow.name}</h4>
                          <span className="text-[10px] bg-orange-500/20 text-orange-400 px-1.5 py-0.5 rounded">
                            {workflow.steps.length} steps
                          </span>
                        </div>
                        {workflow.description && (
                          <p className="item-card-desc">{workflow.description}</p>
                        )}
                        {workflow.run_count > 0 && (
                          <div className="flex gap-3 mt-1.5 text-[10px] text-muted-foreground">
                            <span>{workflow.run_count} runs</span>
                          </div>
                        )}
                      </div>
                    </div>
                    <button
                      onClick={() => runGuiWorkflow(workflow)}
                      disabled={!execution.configLoaded || runningId === workflow.id}
                      className="w-full mt-3 flex items-center justify-center gap-2 py-2 text-xs rounded-lg bg-orange-500/20 text-orange-400 hover:bg-orange-500/30 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                    >
                      {runningId === workflow.id ? (
                        <Loader2 className="w-3.5 h-3.5 animate-spin" />
                      ) : (
                        <Play className="w-3.5 h-3.5" />
                      )}
                      {!execution.configLoaded ? "Load Config First" : "Run Workflow"}
                    </button>
                  </div>
                ))
              )}
            </div>
          </div>
        );

      case "tasks":
        return (
          <div className="grid grid-cols-1 lg:grid-cols-2 xl:grid-cols-3 gap-3">
            {loading ? (
              <div className="col-span-full empty-state">
                <Loader2 className="empty-state-icon animate-spin" />
              </div>
            ) : prompts.length === 0 ? (
              <div className="col-span-full empty-state">
                <FolderOpen className="empty-state-icon" />
                <p className="empty-state-title">No AI tasks found</p>
                <p className="empty-state-desc">Create tasks in the Library to run them here.</p>
              </div>
            ) : (
              prompts.map((task) => (
                <div key={task.id} className="item-card">
                  <div className="item-card-header">
                    <FileText className="item-card-icon text-amber-500" />
                    <div className="item-card-content">
                      <h4 className="item-card-title">{task.name}</h4>
                      {task.description && <p className="item-card-desc">{task.description}</p>}
                    </div>
                    <button
                      onClick={() => runTask(task)}
                      disabled={runningId === task.id}
                      className="item-card-action bg-amber-500/20 text-amber-400 hover:bg-amber-500/30"
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
          <div className="grid grid-cols-1 lg:grid-cols-2 xl:grid-cols-3 gap-3">
            {loading ? (
              <div className="col-span-full empty-state">
                <Loader2 className="empty-state-icon animate-spin" />
              </div>
            ) : aiWorkflows.length === 0 ? (
              <div className="col-span-full empty-state">
                <FolderOpen className="empty-state-icon" />
                <p className="empty-state-title">No AI workflows found</p>
                <p className="empty-state-desc">
                  Create workflows in the Workflow Builder to run them here.
                </p>
              </div>
            ) : (
              aiWorkflows.map((workflow) => (
                <div key={workflow.id} className="item-card">
                  <div className="item-card-header">
                    <Sparkles className="item-card-icon text-green-500" />
                    <div className="item-card-content">
                      <div className="flex items-center gap-2">
                        <h4 className="item-card-title">{workflow.name}</h4>
                        <span className="text-[10px] bg-green-500/20 text-green-400 px-1.5 py-0.5 rounded">
                          {workflow.steps.length} steps
                        </span>
                      </div>
                      {workflow.goal && <p className="item-card-desc">{workflow.goal}</p>}
                    </div>
                    <button
                      onClick={() => runAiWorkflow(workflow)}
                      disabled={runningId === workflow.id}
                      className="item-card-action bg-green-500/20 text-green-400 hover:bg-green-500/30"
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
          <div className="grid grid-cols-1 lg:grid-cols-2 xl:grid-cols-3 gap-3">
            {loading ? (
              <div className="col-span-full empty-state">
                <Loader2 className="empty-state-icon animate-spin" />
              </div>
            ) : scripts.length === 0 ? (
              <div className="col-span-full empty-state">
                <FolderOpen className="empty-state-icon" />
                <p className="empty-state-title">No Playwright scripts found</p>
                <p className="empty-state-desc">
                  Create scripts in the Script Builder to run them here.
                </p>
              </div>
            ) : (
              scripts.map((script) => (
                <div key={script.id} className="item-card">
                  <div className="item-card-header">
                    <TestTube className="item-card-icon text-purple-500" />
                    <div className="item-card-content">
                      <h4 className="item-card-title">{script.name}</h4>
                      {script.description && <p className="item-card-desc">{script.description}</p>}
                    </div>
                    <button
                      onClick={() => runScript(script)}
                      disabled={runningId === script.id}
                      className="item-card-action bg-purple-500/20 text-purple-400 hover:bg-purple-500/30"
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
          <div className="space-y-4">
            {/* Config Panel for Verifications */}
            <div className="panel max-w-2xl">
              <div className="panel-header">
                <div className="panel-title">
                  <Settings className="w-4 h-4 text-muted-foreground" />
                  <span>Configuration</span>
                </div>
              </div>
              <div className="panel-content space-y-3">
                <div className="flex items-center gap-2 text-sm">
                  <span className="text-muted-foreground">Current:</span>
                  <span className="font-medium">
                    {execution.config?.name || "No config loaded"}
                  </span>
                  {!execution.config && (
                    <span className="badge badge-warning text-[10px]">Required</span>
                  )}
                </div>
                <div className="flex gap-2">
                  <button
                    onClick={execution.loadConfiguration}
                    className="flex-1 flex items-center justify-center gap-2 px-3 py-1.5 text-xs rounded-lg bg-muted/50 hover:bg-muted transition-colors"
                  >
                    <FolderOpen className="w-3.5 h-3.5" />
                    Load Config
                  </button>
                  <button
                    onClick={execution.loadLastConfiguration}
                    className="flex-1 flex items-center justify-center gap-2 px-3 py-1.5 text-xs rounded-lg bg-muted/50 hover:bg-muted transition-colors"
                  >
                    <RefreshCw className="w-3.5 h-3.5" />
                    Load Last
                  </button>
                </div>
              </div>
            </div>

            {/* Verification Cards */}
            <div className="grid grid-cols-1 lg:grid-cols-2 xl:grid-cols-3 gap-3">
              {loading ? (
                <div className="col-span-full empty-state">
                  <Loader2 className="empty-state-icon animate-spin" />
                </div>
              ) : verifications.length === 0 ? (
                <div className="col-span-full empty-state">
                  <FolderOpen className="empty-state-icon" />
                  <p className="empty-state-title">No verifications found</p>
                  <p className="empty-state-desc">
                    Create verification configs in the Library to run them here.
                  </p>
                </div>
              ) : (
                verifications.map((verification) => (
                  <div
                    key={verification.id}
                    className={`item-card ${!configLoaded ? "opacity-60" : ""}`}
                  >
                    <div className="item-card-header">
                      <ShieldCheck className="item-card-icon text-emerald-500" />
                      <div className="item-card-content">
                        <div className="flex items-center gap-2">
                          <h4 className="item-card-title">{verification.name}</h4>
                          <span className="text-[10px] bg-emerald-500/20 text-emerald-400 px-1.5 py-0.5 rounded">
                            {verification.config.strategy}
                          </span>
                        </div>
                        {verification.description && (
                          <p className="item-card-desc">{verification.description}</p>
                        )}
                        <div className="flex gap-3 mt-1.5 text-[10px] text-muted-foreground">
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
                      className="w-full mt-3 flex items-center justify-center gap-2 py-2 text-xs rounded-lg bg-emerald-500/20 text-emerald-400 hover:bg-emerald-500/30 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                    >
                      {runningId === verification.id || verificationRunning ? (
                        <Loader2 className="w-3.5 h-3.5 animate-spin" />
                      ) : (
                        <Play className="w-3.5 h-3.5" />
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

  // Get accent class for task type
  const getTabAccentClass = (taskType: TaskType, isActive: boolean) => {
    if (!isActive) return "tab-inactive";
    switch (taskType) {
      case "gui":
        return "tab-accent-blue";
      case "gui-workflows":
        return "tab-accent-orange";
      case "tasks":
        return "tab-accent-amber";
      case "workflows":
        return "tab-accent-green";
      case "scripts":
        return "tab-accent-purple";
      case "verifications":
        return "tab-accent-emerald";
      default:
        return "tab-active";
    }
  };

  return (
    <div className="h-full flex flex-col p-4 gap-4 overflow-hidden">
      {/* Task Type Selector - horizontal tabs */}
      <div className="flex-shrink-0 flex items-center gap-3">
        <div className="tab-container flex-1">
          {TASK_TYPES.map((taskType) => {
            const Icon = taskType.icon;
            const count =
              taskType.id === "gui"
                ? execution.workflows.length
                : taskType.id === "gui-workflows"
                  ? guiWorkflows.length
                  : taskType.id === "tasks"
                    ? prompts.length
                    : taskType.id === "workflows"
                      ? aiWorkflows.length
                      : taskType.id === "scripts"
                        ? scripts.length
                        : verifications.length;
            const isActive = selectedTaskType === taskType.id;

            return (
              <button
                key={taskType.id}
                onClick={() => setSelectedTaskType(taskType.id)}
                className={`tab-item ${getTabAccentClass(taskType.id, isActive)}`}
              >
                <Icon className="w-3.5 h-3.5" />
                <span>{taskType.label}</span>
                {count > 0 && (
                  <span className="px-1.5 py-0.5 text-[10px] rounded bg-black/20">{count}</span>
                )}
              </button>
            );
          })}
        </div>

        {/* Refresh button */}
        <button
          onClick={fetchLibraryItems}
          disabled={loading}
          className="p-2 text-muted-foreground hover:text-foreground rounded-lg hover:bg-muted/50 transition-colors"
          title="Refresh library items"
        >
          <RefreshCw className={`w-4 h-4 ${loading ? "animate-spin" : ""}`} />
        </button>
      </div>

      {/* Task Type Description */}
      <div className="flex-shrink-0 flex items-center gap-2 text-xs text-muted-foreground px-1">
        <TaskIcon className={`w-4 h-4 ${currentTaskType.color}`} />
        <span>{currentTaskType.description}</span>
        {currentTaskType.requiresConfig && !execution.configLoaded && (
          <span className="badge badge-warning">Requires config</span>
        )}
      </div>

      {/* Task Content - scrollable */}
      <div className="flex-1 min-h-0 overflow-auto scrollbar-dark">{renderTaskContent()}</div>
    </div>
  );
}

export default ExecuteTab;
