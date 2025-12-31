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
  Save,
  History,
  Workflow,
  Camera,
  Plus,
  Trash2,
  ChevronUp,
  GripVertical,
  Square,
  FileText,
  Settings,
  Activity,
  Code,
  ToggleLeft,
  ToggleRight,
  MousePointer2,
  TestTube,
  BookOpen,
  X,
  FilePlus2,
  AlertTriangle,
  RotateCcw,
  FolderOpen,
} from "lucide-react";
import { useExecution, useAutoContinue } from "../contexts";
import type { UseProjectLogsReturn } from "../hooks/useProjectLogs";
import CollapsiblePanel from "./CollapsiblePanel";

interface StateInfo {
  name: string;
  description?: string;
  images: string[];
}

interface ImageInfo {
  id: string;
  name: string;
  stateName: string;
}

interface WorkflowInfo {
  id: string;
  name: string;
  category?: string;
}

/** Playwright script info for the dropdown */
interface PlaywrightScriptInfo {
  id: string;
  name: string;
  target_url: string;
  description: string;
  script_content: string;
}

/** Saved prompt info for the dropdown */
interface SavedPromptInfo {
  id: string;
  name: string;
  description: string;
  content: string;
  category: string;
}

/** Saved AI Workflow from the AI Workflows library */
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

/** A single step in the execution sequence */
interface ExecutionStep {
  id: string;
  type: "workflow" | "state" | "playwright" | "prompt" | "action" | "screenshot";
  name: string;
  takeScreenshot: boolean;
  screenshotDelay?: number; // Delay in seconds before taking screenshot (default 0)
  playwrightScriptId?: string;
  playwrightScriptContent?: string;
  playwrightTargetUrl?: string;
  promptId?: string;
  promptContent?: string;
  // Action step fields
  actionType?: "click" | "double_click" | "right_click";
  targetImageId?: string;
  targetImageName?: string;
  // Screenshot step fields
  screenshotMonitor?: number | "all"; // Monitor index or "all" for all monitors
}

interface PromptHistoryEntry {
  id: string;
  timestamp: number;
  steps: ExecutionStep[];
  goal: string;
  success?: boolean;
}

// Default Developer Mode Prompt Template - Runner handles continuation deterministically
// This is the hardcoded default that can be restored if users modify the template
const DEFAULT_DEVELOPER_PROMPT_TEMPLATE = `# AI Developer Loop

Execute automation steps, analyze results, fix issues. The runner handles session continuation.

## Runner-Managed Continuation

**You don't need to spawn new sessions or manage state files.** The runner will:
1. Check the checkpoint file after your session ends
2. If \`completed: false\` → automatically spawn next session
3. If \`completed: true\` → stop (goal achieved)

Your job: Do the work, update checkpoint when done.

## Session Info

**Session ID:** {{SESSION_ID}}
**Iteration:** {{ITERATION}}/{{MAX_ITERATIONS}}
**Checkpoint:** {{DEV_LOGS_ESCAPED}}\\improve-all-checkpoint.json

## Configuration

**Execution Steps:** {{STEP_COUNT}} steps
**Goal:** {{GOAL}}

## Debugging Resources

### Quick Error Check (Use First)

Query the runner's debugging API for structured error summaries:

\`\`\`powershell
# Get all recent errors across services (backend, frontend, api, runner)
(Invoke-WebRequest -Uri "http://localhost:9876/debug/app/errors?limit=50" -UseBasicParsing).Content | ConvertFrom-Json | Select-Object -ExpandProperty data

# Filter by service
(Invoke-WebRequest -Uri "http://localhost:9876/debug/app/errors?service=backend&level=error" -UseBasicParsing).Content | ConvertFrom-Json | Select-Object -ExpandProperty data

# Check previous session findings
(Invoke-WebRequest -Uri "http://localhost:9876/findings/summary" -UseBasicParsing).Content | ConvertFrom-Json | Select-Object -ExpandProperty data
\`\`\`

### When to Use Which Resource

| Scenario | Use |
|----------|-----|
| "Are there any errors?" | MCP endpoint \`/debug/app/errors\` |
| "What errors occurred in the last run?" | MCP endpoint \`/debug/app/errors\` |
| "What issues were found previously?" | MCP endpoint \`/findings/summary\` |
| "What's the full stack trace?" | Log files directly |
| "Search for a specific error message" | Log files with Select-String |
| "Understand the sequence of events" | Log files directly |

### Detailed Investigation (Log Files)

For full context, stack traces, or searching specific patterns:

\`\`\`powershell
# Application logs
{{LOG_CHECK_COMMANDS}}

# Runner event logs
Get-Content "{{DEV_LOGS_ESCAPED}}\\runner-actions.jsonl" -Tail 50 | Select-String -Pattern '"error"|"failed"'
Get-Content "{{DEV_LOGS_ESCAPED}}\\runner-image-recognition.jsonl" -Tail 30 | Select-String -Pattern '"found":\\s*false'
\`\`\`

{{LOG_MONITORING_SECTION}}

## Step 1: Execute Automation Steps

{{EXECUTION_STEPS}}

## Step 2: Analyze Results

### 2.1 Quick Error Check (MCP API)

First, query the debugging API for a structured summary:

\`\`\`powershell
$errors = (Invoke-WebRequest -Uri "http://localhost:9876/debug/app/errors?limit=30" -UseBasicParsing).Content | ConvertFrom-Json
if ($errors.data.summary.total -gt 0) {
    Write-Host "Found $($errors.data.summary.total) errors/warnings:"
    Write-Host "  By service: $($errors.data.summary.by_service | ConvertTo-Json -Compress)"
    Write-Host "  By level: $($errors.data.summary.by_level | ConvertTo-Json -Compress)"
    $errors.data.errors | Select-Object timestamp, service, level, message | Format-Table -AutoSize
}
\`\`\`

### 2.2 Check Previous Findings

See what issues were already detected in previous sessions:

\`\`\`powershell
$findings = (Invoke-WebRequest -Uri "http://localhost:9876/findings/summary" -UseBasicParsing).Content | ConvertFrom-Json
if ($findings.data.total_findings -gt 0) {
    Write-Host "Previous sessions found $($findings.data.total_findings) issues:"
    Write-Host "  Code-related: $($findings.data.code_related_findings)"
    Write-Host "  By severity: $($findings.data.by_severity | ConvertTo-Json -Compress)"
}
\`\`\`

### 2.3 View Screenshots

Check for annotated screenshots from image recognition:

\`\`\`powershell
Get-ChildItem "{{DEV_LOGS_ESCAPED}}\\screenshots" -Filter "*.png" | Sort-Object LastWriteTime -Descending | Select-Object -First 5
\`\`\`

Use the Read tool to view the most recent screenshots.

## Step 3: Fix Issues

If any errors were found:
1. Identify the root cause from the API response or logs
2. Read the relevant source code
3. Make the fix
4. Restart affected services as needed:

\`\`\`powershell
# Restart any service - you are running independently
cd {{WORKSPACE_ESCAPED}}; .\\dev-start.ps1 -Backend   # Backend
cd {{WORKSPACE_ESCAPED}}; .\\dev-start.ps1 -Frontend  # Frontend
cd {{WORKSPACE_ESCAPED}}; .\\dev-start.ps1 -Api       # API

# You CAN restart the runner if needed:
Stop-Process -Name qontinui-runner -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 3
cd {{WORKSPACE_ESCAPED}}\\qontinui-runner; Start-Process npm -ArgumentList "run","tauri","dev"
\`\`\`

## Step 4: Update Checkpoint

**After completing work, update the checkpoint file. The runner reads this to decide what to do next.**

\`\`\`powershell
$checkpointPath = "{{DEV_LOGS_ESCAPED}}\\improve-all-checkpoint.json"
$checkpoint = if (Test-Path $checkpointPath) { Get-Content $checkpointPath | ConvertFrom-Json } else { @{} }

# Update progress
$checkpoint.current_phase = {{ITERATION}}
$checkpoint.last_update = (Get-Date -Format "o")

# Set completed based on whether goal is achieved
if ($goalAchieved) {
    $checkpoint.completed = $true
    $checkpoint.goal_achieved = $true
} else {
    $checkpoint.completed = $false
    $checkpoint.work_completed = @{
        fixes_applied = @("fix1", "fix2")  # List what you fixed
        issues_remaining = @("issue1")      # What's left
    }
}

$checkpoint | ConvertTo-Json -Depth 10 | Out-File -Encoding UTF8 $checkpointPath
\`\`\`

**Key points:**
- \`completed: true\` → Runner stops, workflow done
- \`completed: false\` → Runner spawns next session automatically
- You don't need to spawn sessions yourself

## Rules

- Execute steps IN THE EXACT ORDER specified
- **USE MCP ENDPOINTS FIRST** for quick error discovery
- Use log files for detailed investigation when needed
- You are INDEPENDENT - you CAN restart any service including the runner
- Work AUTONOMOUSLY - never ask the user
- **UPDATE CHECKPOINT** when done - runner reads it to decide continuation
- **Set completed: true** when goal is achieved
- **Don't spawn sessions yourself** - runner handles this deterministically

## Issue Tracking

When you detect an issue:
\`\`\`
[ISSUE:DETECTED] {"type":"error","severity":"high","title":"Brief title","file":"path/to/file.ts","line":42,"description":"Description"}
\`\`\`

When you fix an issue:
\`\`\`
[ISSUE:RESOLVED] {"title":"Brief title","resolution":"How you fixed it"}
\`\`\`
`;

// Storage key for custom prompt template
const CUSTOM_PROMPT_TEMPLATE_KEY = "qontinui-custom-developer-prompt-template";

// Get the current developer prompt template (custom or default)
const getDeveloperPromptTemplate = (): string => {
  if (typeof window !== "undefined") {
    const customTemplate = localStorage.getItem(CUSTOM_PROMPT_TEMPLATE_KEY);
    if (customTemplate) {
      return customTemplate;
    }
  }
  return DEFAULT_DEVELOPER_PROMPT_TEMPLATE;
};

// Save a custom prompt template
const saveCustomPromptTemplate = (template: string): void => {
  if (typeof window !== "undefined") {
    localStorage.setItem(CUSTOM_PROMPT_TEMPLATE_KEY, template);
  }
};

// Reset to default prompt template
const resetPromptTemplateToDefault = (): void => {
  if (typeof window !== "undefined") {
    localStorage.removeItem(CUSTOM_PROMPT_TEMPLATE_KEY);
  }
};

// Check if using custom template
const isUsingCustomTemplate = (): boolean => {
  if (typeof window !== "undefined") {
    return localStorage.getItem(CUSTOM_PROMPT_TEMPLATE_KEY) !== null;
  }
  return false;
};

interface WorkspacePaths {
  workspace_root: string;
  dev_logs_path: string;
  scripts_path: string;
  workspace_root_escaped: string;
  dev_logs_path_escaped: string;
  // Note: spawn_script fields removed - runner handles spawning deterministically
}

interface AiDeveloperState {
  session_id: string;
  iteration: number;
  max_iterations: number;
  status: string;
  started_at: string;
  stop_requested: boolean;
  current_action: string;
  errors_fixed: Array<{ file: string; line?: number; description: string; fixed_at: string }>;
  errors_remaining: Array<{ file: string; error_type: string; context: string }>;
}

// Types for unified sessions API
interface SessionCheckpoint {
  session_id: string;
  current_phase: number;
  total_phases: number;
  completed: boolean;
  status: string;
  started_at: string;
  last_activity: string;
  sessions_spawned: number;
  restart_permitted: boolean;
  error_message: string | null;
  custom_data: Record<string, unknown>;
  activity_log: string[];
}

interface SessionConfig {
  prompt: string;
  continuation_prompt: string | null;
  total_phases: number;
  uses_gui: boolean;
  timeout_seconds: number;
  stall_threshold_seconds: number;
  name: string;
  description: string;
  custom_config: Record<string, unknown>;
}

interface Session {
  id: string;
  config: SessionConfig;
  status:
    | "starting"
    | "running"
    | "completed"
    | "failed"
    | "stopped"
    | "waiting_for_continuation"
    | "stalled";
  checkpoint: SessionCheckpoint;
  active_subprocess_id: string | null;
  event_log: { timestamp: number; event_type: string; message: string }[];
}

// Convert Session to AiDeveloperState for UI compatibility
function sessionToAiDeveloperState(session: Session): AiDeveloperState {
  return {
    session_id: session.id,
    iteration: session.checkpoint.current_phase,
    max_iterations: session.config.total_phases,
    status: session.status === "waiting_for_continuation" ? "idle" : session.status,
    started_at: session.checkpoint.started_at,
    stop_requested: false,
    current_action:
      session.event_log.length > 0 ? session.event_log[session.event_log.length - 1].message : "",
    errors_fixed: [],
    errors_remaining: [],
  };
}

interface AiBuilderTabProps {
  /** Project logs hook from App.tsx (shared with Logs tab) */
  projectLogs: UseProjectLogsReturn;
  /** Navigate to Log Locations sub-tab */
  onNavigateToLogLocations: () => void;
}

export function AiBuilderTab({ projectLogs, onNavigateToLogLocations }: AiBuilderTabProps) {
  const execution = useExecution();

  // Input capture for coordinate validation - persisted to localStorage
  // When enabled, captures actual mouse/keyboard during automation to compare with reported positions
  const [captureInputValidation, setCaptureInputValidation] = useState<boolean>(() => {
    try {
      return localStorage.getItem("qontinui-ai-capture-input-validation") === "true";
    } catch {
      return false;
    }
  });

  // Persist input capture setting to localStorage
  useEffect(() => {
    localStorage.setItem(
      "qontinui-ai-capture-input-validation",
      captureInputValidation ? "true" : "false",
    );
  }, [captureInputValidation]);

  // Ordered execution steps - persisted to localStorage
  const [executionSteps, setExecutionSteps] = useState<ExecutionStep[]>(() => {
    try {
      const saved = localStorage.getItem("qontinui-ai-execution-steps");
      return saved ? JSON.parse(saved) : [];
    } catch {
      return [];
    }
  });

  // Goal - persisted to localStorage
  const [goal, setGoal] = useState(() => {
    try {
      return localStorage.getItem("qontinui-ai-goal") || "";
    } catch {
      return "";
    }
  });

  // Max iterations - persisted to localStorage
  const [maxIterations, setMaxIterations] = useState(() => {
    try {
      const saved = localStorage.getItem("qontinui-ai-max-iterations");
      return saved ? parseInt(saved, 10) : 10;
    } catch {
      return 10;
    }
  });

  // Persist execution steps to localStorage
  useEffect(() => {
    try {
      localStorage.setItem("qontinui-ai-execution-steps", JSON.stringify(executionSteps));
    } catch (e) {
      console.error("Failed to persist execution steps:", e);
    }
  }, [executionSteps]);

  // Persist goal to localStorage
  useEffect(() => {
    localStorage.setItem("qontinui-ai-goal", goal);
  }, [goal]);

  // Persist max iterations to localStorage
  useEffect(() => {
    localStorage.setItem("qontinui-ai-max-iterations", maxIterations.toString());
  }, [maxIterations]);

  // Workspace paths (fetched from backend for portability)
  const [workspacePaths, setWorkspacePaths] = useState<WorkspacePaths | null>(null);

  // Playwright scripts for the dropdown
  const [playwrightScripts, setPlaywrightScripts] = useState<PlaywrightScriptInfo[]>([]);

  // Saved prompts from the prompt library (used for adding prompt steps)
  const [savedPrompts, setSavedPrompts] = useState<SavedPromptInfo[]>([]);

  // Save workflow dialog state
  const [showSaveDialog, setShowSaveDialog] = useState(false);
  const [saveName, setSaveName] = useState("");
  const [saveDescription, setSaveDescription] = useState("");
  const [isSaving, setIsSaving] = useState(false);

  // Prompt template editing state
  const [isEditingPromptTemplate, setIsEditingPromptTemplate] = useState(false);
  const [editedPromptTemplate, setEditedPromptTemplate] = useState("");
  const [usingCustomTemplate, setUsingCustomTemplate] = useState(() => isUsingCustomTemplate());

  // New workflow confirmation dialog (for unsaved changes warning)
  const [showNewConfirmDialog, setShowNewConfirmDialog] = useState(false);

  // Track the currently loaded workflow (null = new unsaved workflow)
  const [currentWorkflowId, setCurrentWorkflowId] = useState<string | null>(null);
  const [currentWorkflowName, setCurrentWorkflowName] = useState<string>("");

  // Simple dirty flag for unsaved changes detection
  const [hasUnsavedChanges, setHasUnsavedChanges] = useState(false);

  // Saved AI Workflows from the AI Workflows library
  const [savedAiWorkflows, setSavedAiWorkflows] = useState<SavedAiWorkflow[]>([]);
  const [showWorkflowsPanel, setShowWorkflowsPanel] = useState(false);

  // Session state - persist to localStorage to survive tab switches
  const [currentSessionId, setCurrentSessionId] = useState<string | null>(() => {
    try {
      return localStorage.getItem("qontinui-ai-developer-session") || null;
    } catch {
      return null;
    }
  });
  const [sessionState, setSessionState] = useState<AiDeveloperState | null>(null);

  // UI state
  const [showAddDropdown, setShowAddDropdown] = useState(false);
  const [isRunning, setIsRunning] = useState(false);
  const [lastResult, setLastResult] = useState<{ success: boolean; message: string } | null>(null);

  // Resumable workflow state (for continuing after runner restart)
  const [resumableWorkflow, setResumableWorkflow] = useState<{
    name: string;
    currentPhase: number;
    totalPhases: number;
    status: string;
  } | null>(null);
  const [isResuming, setIsResuming] = useState(false);

  // Auto-continue settings (from context - syncs across all components)
  const {
    enabled: autoContinueEnabled,
    loading: autoContinueLoading,
    toggle: toggleAutoContinue,
  } = useAutoContinue();

  // Claude output log (real-time visibility)
  // Note: These values are polled but not displayed in UI yet
  const [, setClaudeLog] = useState<string>("");
  const [, setClaudeLogInfo] = useState<{
    totalLines: number;
    lastModified: number;
  } | null>(null);

  // Persist session ID to localStorage
  useEffect(() => {
    if (currentSessionId) {
      localStorage.setItem("qontinui-ai-developer-session", currentSessionId);
    } else {
      localStorage.removeItem("qontinui-ai-developer-session");
    }
  }, [currentSessionId]);

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

        const stateImgs = state.stateImages || state.images;
        if (stateImgs && Array.isArray(stateImgs)) {
          for (const img of stateImgs) {
            const imgId = img.id || "";
            const imgName = img.name || img.id || "";
            if (imgId && imgName) {
              stateImages.push(imgName);
              imageList.push({ id: imgId, name: imgName, stateName });
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

  // Fetch workspace paths on mount
  useEffect(() => {
    const fetchPaths = async () => {
      try {
        const response = await invoke<{ success: boolean; data?: WorkspacePaths }>(
          "get_workspace_paths",
        );
        if (response.success && response.data) {
          setWorkspacePaths(response.data);
        }
      } catch (error) {
        console.error("Failed to fetch workspace paths:", error);
      }
    };
    fetchPaths();
  }, []);

  // Fetch Playwright scripts for the dropdown
  useEffect(() => {
    const fetchPlaywrightScripts = async () => {
      try {
        const response = await fetch("http://localhost:9876/playwright/scripts");
        const result = await response.json();
        if (result.success && result.data) {
          setPlaywrightScripts(
            result.data.map(
              (s: {
                id: string;
                name: string;
                target_url: string;
                description: string;
                script_content: string;
              }) => ({
                id: s.id,
                name: s.name,
                target_url: s.target_url,
                description: s.description,
                script_content: s.script_content,
              }),
            ),
          );
        }
      } catch (error) {
        console.error("Failed to fetch Playwright scripts:", error);
      }
    };
    fetchPlaywrightScripts();
  }, []);

  // Fetch saved prompts from the prompt library
  useEffect(() => {
    const fetchSavedPrompts = async () => {
      try {
        const response = await fetch("http://localhost:9876/prompts");
        const result = await response.json();
        if (result.success && result.data) {
          setSavedPrompts(
            result.data.map(
              (p: {
                id: string;
                name: string;
                description: string;
                content: string;
                category: string;
              }) => ({
                id: p.id,
                name: p.name,
                description: p.description,
                content: p.content,
                category: p.category,
              }),
            ),
          );
        }
      } catch (error) {
        console.error("Failed to fetch saved prompts:", error);
      }
    };
    fetchSavedPrompts();
  }, []);

  // Check for resumable workflows on mount and periodically
  useEffect(() => {
    const checkResumableWorkflow = async () => {
      try {
        const response = await fetch("http://localhost:9876/workflow/resumable");
        const result = await response.json();
        if (result.success) {
          // Update isRunning from the API (more reliable than local state)
          if (result.data?.is_running !== undefined) {
            setIsRunning(result.data.is_running);
          }
          // Note: auto_continue_enabled is now managed by AutoContinueContext
          // Only show Continue if there's a resumable workflow AND nothing is running
          if (result.data?.has_resumable && !result.data?.is_running) {
            setResumableWorkflow({
              name: result.data.name || "Unknown Workflow",
              currentPhase: result.data.current_phase || 0,
              totalPhases: result.data.total_phases || 0,
              status: result.data.status || "unknown",
            });
          } else {
            setResumableWorkflow(null);
          }
        } else {
          setResumableWorkflow(null);
        }
      } catch (error) {
        console.error("Failed to check resumable workflow:", error);
        setResumableWorkflow(null);
      }
    };

    // Check on mount
    checkResumableWorkflow();

    // Check periodically (every 5 seconds for better responsiveness)
    const interval = setInterval(checkResumableWorkflow, 5000);

    return () => clearInterval(interval);
  }, []);

  // Handler to resume a workflow
  const handleResumeWorkflow = async () => {
    if (!resumableWorkflow || isResuming) return;

    setIsResuming(true);
    try {
      const response = await fetch("http://localhost:9876/workflow/resume", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
      });
      const result = await response.json();

      if (result.success) {
        // Clear the resumable state since we're now running
        setResumableWorkflow(null);
        setIsRunning(true);
        setLastResult({ success: true, message: `Resuming workflow: ${resumableWorkflow.name}` });
      } else {
        setLastResult({
          success: false,
          message: result.error || "Failed to resume workflow",
        });
      }
    } catch (error) {
      console.error("Failed to resume workflow:", error);
      setLastResult({
        success: false,
        message: `Failed to resume: ${error instanceof Error ? error.message : String(error)}`,
      });
    } finally {
      setIsResuming(false);
    }
  };

  // Fetch saved AI workflows from the AI Workflows library
  const fetchSavedAiWorkflows = async () => {
    try {
      const response = await fetch("http://localhost:9876/ai-workflows");
      const result = await response.json();
      if (result.success && result.data) {
        setSavedAiWorkflows(result.data);
      }
    } catch (error) {
      console.error("Failed to fetch saved AI workflows:", error);
    }
  };

  useEffect(() => {
    fetchSavedAiWorkflows();
  }, []);

  // Sync input capture setting with Python executor when toggle changes
  useEffect(() => {
    const syncInputCapture = async () => {
      try {
        await invoke("set_input_capture_enabled", { enabled: captureInputValidation });
        console.log("[AI_BUILDER] Input capture enabled:", captureInputValidation);
      } catch (error) {
        console.error("[AI_BUILDER] Failed to set input capture:", error);
      }
    };
    syncInputCapture();
  }, [captureInputValidation]);

  // Poll session state when there's an active session
  useEffect(() => {
    if (!currentSessionId) return;

    const pollState = async () => {
      try {
        // Use new unified sessions API
        const response = await fetch(`http://localhost:9876/sessions/${currentSessionId}`);
        const result = await response.json();

        if (result.success && result.data) {
          const session = result.data as Session;
          const state = sessionToAiDeveloperState(session);
          setSessionState(state);

          // Update isRunning based on session status
          const activeStatuses = ["starting", "running", "waiting_for_continuation"];
          const isActiveStatus = activeStatuses.includes(session.status);

          let isRecent = false;
          if (session.checkpoint.started_at) {
            const startedAt = new Date(session.checkpoint.started_at).getTime();
            const tenMinutesAgo = Date.now() - 10 * 60 * 1000;
            isRecent = startedAt > tenMinutesAgo;
          }

          // Only consider running if both active status AND recent start
          setIsRunning(isActiveStatus && isRecent);

          // Clear session if it's complete/stopped (but keep showing final state)
          if (
            session.status === "completed" ||
            session.status === "stopped" ||
            session.status === "failed"
          ) {
            // Don't clear immediately - let user see the final state
            // They can start a new session which will clear it
          }
        } else if (!result.success) {
          // Session not found - clear the session
          console.log("Session not found, clearing session");
          setCurrentSessionId(null);
          setSessionState(null);
          setIsRunning(false);
        }
      } catch (error) {
        console.error("Failed to read session state:", error);
      }
    };

    // Poll immediately and then every 2 seconds
    pollState();
    const interval = setInterval(pollState, 2000);

    return () => clearInterval(interval);
  }, [currentSessionId]);

  // Poll Claude log when there's an active session
  useEffect(() => {
    if (!currentSessionId) {
      setClaudeLog("");
      setClaudeLogInfo(null);
      return;
    }

    const pollLog = async () => {
      try {
        const response = await invoke<{
          success: boolean;
          data?: {
            content: string;
            total_lines: number;
            last_modified: number;
          };
        }>("read_claude_session_log", { sessionId: currentSessionId, tailLines: 100 });
        if (response.success && response.data) {
          setClaudeLog(response.data.content);
          setClaudeLogInfo({
            totalLines: response.data.total_lines,
            lastModified: response.data.last_modified,
          });
        }
      } catch (error) {
        console.error("Failed to read Claude log:", error);
      }
    };

    // Poll immediately and then every 3 seconds
    pollLog();
    const interval = setInterval(pollLog, 3000);

    return () => clearInterval(interval);
  }, [currentSessionId]);

  // Add a step to the execution list
  const addStep = (
    type: "workflow" | "state" | "playwright" | "prompt",
    name: string,
    scriptId?: string,
    scriptContent?: string,
    scriptTargetUrl?: string,
    promptId?: string,
    promptContent?: string,
  ) => {
    const newStep: ExecutionStep = {
      id: crypto.randomUUID(),
      type,
      name,
      takeScreenshot: type !== "prompt", // Screenshots don't apply to prompts
      screenshotDelay: 0, // Default delay in seconds
      playwrightScriptId: scriptId,
      playwrightScriptContent: scriptContent,
      playwrightTargetUrl: scriptTargetUrl,
      promptId,
      promptContent,
    };
    setExecutionSteps((prev) => [...prev, newStep]);
    setShowAddDropdown(false);
    setHasUnsavedChanges(true);
  };

  // Add an action step (e.g., click on image)
  const addActionStep = (
    actionType: "click" | "double_click" | "right_click",
    imageId: string,
    imageName: string,
  ) => {
    const actionNames = {
      click: "Click",
      double_click: "Double Click",
      right_click: "Right Click",
    };
    const newStep: ExecutionStep = {
      id: crypto.randomUUID(),
      type: "action",
      name: `${actionNames[actionType]}: ${imageName}`,
      takeScreenshot: true,
      screenshotDelay: 0,
      actionType,
      targetImageId: imageId,
      targetImageName: imageName,
    };
    setExecutionSteps((prev) => [...prev, newStep]);
    setShowAddDropdown(false);
    setHasUnsavedChanges(true);
  };

  // Add a screenshot step
  const addScreenshotStep = () => {
    const newStep: ExecutionStep = {
      id: crypto.randomUUID(),
      type: "screenshot",
      name: "Capture Screenshot",
      takeScreenshot: true, // Always true for screenshot steps
      screenshotDelay: 0,
      screenshotMonitor: "all",
    };
    setExecutionSteps((prev) => [...prev, newStep]);
    setShowAddDropdown(false);
    setHasUnsavedChanges(true);
  };

  // Update screenshot monitor for a step
  const updateScreenshotMonitor = (stepId: string, monitor: number | "all") => {
    setExecutionSteps((prev) =>
      prev.map((s) => (s.id === stepId ? { ...s, screenshotMonitor: monitor } : s)),
    );
    setHasUnsavedChanges(true);
  };

  // Remove a step
  const removeStep = (stepId: string) => {
    setExecutionSteps((prev) => prev.filter((s) => s.id !== stepId));
    setHasUnsavedChanges(true);
  };

  // Toggle screenshot for a step
  const toggleStepScreenshot = (stepId: string) => {
    setExecutionSteps((prev) =>
      prev.map((s) => (s.id === stepId ? { ...s, takeScreenshot: !s.takeScreenshot } : s)),
    );
    setHasUnsavedChanges(true);
  };

  // Update screenshot delay for a step
  const updateScreenshotDelay = (stepId: string, delay: number) => {
    setExecutionSteps((prev) =>
      prev.map((s) => (s.id === stepId ? { ...s, screenshotDelay: Math.max(0, delay) } : s)),
    );
    setHasUnsavedChanges(true);
  };

  // Move step up
  const moveStepUp = (index: number) => {
    if (index === 0) return;
    setExecutionSteps((prev) => {
      const newSteps = [...prev];
      [newSteps[index - 1], newSteps[index]] = [newSteps[index], newSteps[index - 1]];
      return newSteps;
    });
    setHasUnsavedChanges(true);
  };

  // Move step down
  const moveStepDown = (index: number) => {
    setExecutionSteps((prev) => {
      if (index === prev.length - 1) return prev;
      const newSteps = [...prev];
      [newSteps[index], newSteps[index + 1]] = [newSteps[index + 1], newSteps[index]];
      return newSteps;
    });
    setHasUnsavedChanges(true);
  };

  // Generate common prompt parts
  const generateExecutionInstructions = () => {
    if (executionSteps.length === 0) {
      return "No steps configured. Skip to log analysis.";
    }
    return executionSteps
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

After the workflow completes, capture a screenshot:
\`\`\`powershell
$body = @{ delay_seconds = ${step.screenshotDelay || 0}; task_id = "{{SESSION_ID}}"; step_index = ${stepNum} } | ConvertTo-Json
$response = Invoke-WebRequest -Uri "http://localhost:9876/capture-screenshot" -Method POST -ContentType "application/json" -Body $body -UseBasicParsing
($response.Content | ConvertFrom-Json).data.screenshot_path
\`\`\`${step.screenshotDelay && step.screenshotDelay > 0 ? `\n(Wait ${step.screenshotDelay}s before capture for UI to settle.)` : ""}`;
          }
          return instruction;
        } else if (step.type === "playwright") {
          let instruction = `### Step ${stepNum}: Run Playwright Test "${step.name}"

**⚡ ACTION: RUN THIS TEST IMMEDIATELY** - The script already exists on the server.

**Script ID:** \`${step.playwrightScriptId}\`
**Target URL:** ${step.playwrightTargetUrl || "(not specified)"}

**EXECUTE NOW** - Run this Playwright test via the runner API:
\`\`\`powershell
$response = Invoke-WebRequest -Uri "http://localhost:9876/playwright/scripts/${step.playwrightScriptId}/run" -Method POST -ContentType "application/json" -Body '{}' -UseBasicParsing
$response.Content | ConvertFrom-Json | ConvertTo-Json -Depth 10
\`\`\`

${
  step.playwrightScriptContent
    ? `**Script Content (for reference):**
\`\`\`typescript
${step.playwrightScriptContent}
\`\`\``
    : `*(Script content not cached - run the test anyway, it exists on the server)*`
}

After running, analyze the structured output:
- Check \`success\` field for overall result
- Check \`data.passed\` for test pass status
- Check \`data.tests_passed\` and \`data.tests_failed\` counts
- If tests fail, examine \`data.error\` and \`data.structured_output\` for details
- Screenshots are saved to \`data.screenshots\` paths

If tests fail, determine if the issue is:
- **Application bug** → fix application code
- **Test selector outdated** → update test script via PUT /playwright/scripts/${step.playwrightScriptId}
- **Timing issue** → add waits/retries to test script

**To update the script**, use:
\`\`\`powershell
$body = @{ script_content = @"
// Your updated TypeScript code here
"@ } | ConvertTo-Json
Invoke-WebRequest -Uri "http://localhost:9876/playwright/scripts/${step.playwrightScriptId}" -Method PUT -ContentType "application/json" -Body $body
\`\`\``;
          if (step.takeScreenshot) {
            instruction += `

After the test completes, capture an additional screenshot:
\`\`\`powershell
$body = @{ delay_seconds = ${step.screenshotDelay || 0}; task_id = "{{SESSION_ID}}"; step_index = ${stepNum} } | ConvertTo-Json
$response = Invoke-WebRequest -Uri "http://localhost:9876/capture-screenshot" -Method POST -ContentType "application/json" -Body $body -UseBasicParsing
($response.Content | ConvertFrom-Json).data.screenshot_path
\`\`\`${step.screenshotDelay && step.screenshotDelay > 0 ? `\n(Wait ${step.screenshotDelay}s before capture for UI to settle.)` : ""}`;
          }
          return instruction;
        } else if (step.type === "prompt") {
          // Prompt steps inject additional context/instructions into the AI prompt
          const instruction = `### Step ${stepNum}: Follow Prompt Instructions "${step.name}"

**Instructions from Prompt Library:**

${step.promptContent || "(No content)"}

---
*Execute the instructions above before proceeding to the next step.*`;
          return instruction;
        } else if (step.type === "action" && step.actionType === "click") {
          let instruction = `### Step ${stepNum}: Click on Image "${step.targetImageName}"

Execute click action via the runner API:
\`\`\`powershell
$body = '{"action_type": "click", "image_id": "${step.targetImageId}"}'
$response = Invoke-WebRequest -Uri "http://localhost:9876/execute-action" -Method POST -ContentType "application/json" -Body $body -UseBasicParsing
$response.Content | ConvertFrom-Json
\`\`\`

This will find the image on screen and click its center.`;
          if (step.takeScreenshot) {
            instruction += `

After the action completes, capture a screenshot:
\`\`\`powershell
$body = @{ delay_seconds = ${step.screenshotDelay || 0}; task_id = "{{SESSION_ID}}"; step_index = ${stepNum} } | ConvertTo-Json
$response = Invoke-WebRequest -Uri "http://localhost:9876/capture-screenshot" -Method POST -ContentType "application/json" -Body $body -UseBasicParsing
($response.Content | ConvertFrom-Json).data.screenshot_path
\`\`\`${step.screenshotDelay && step.screenshotDelay > 0 ? `\n(Wait ${step.screenshotDelay}s before capture for UI to settle.)` : ""}`;
          }
          return instruction;
        } else if (step.type === "screenshot") {
          // Screenshot step - uses the new unified /capture-screenshot endpoint
          const monitorParam = step.screenshotMonitor === "all" ? "null" : step.screenshotMonitor;
          const monitorDesc =
            step.screenshotMonitor === "all" ? "all monitors" : `monitor ${step.screenshotMonitor}`;
          const instruction = `### Step ${stepNum}: Capture Screenshot

**Action:** Take screenshot of ${monitorDesc}${step.screenshotDelay && step.screenshotDelay > 0 ? ` (after ${step.screenshotDelay}s delay)` : ""}

Execute via the runner API:
\`\`\`powershell
$body = @{
    monitor = ${monitorParam}
    delay_seconds = ${step.screenshotDelay || 0}
    task_id = "{{SESSION_ID}}"
    step_index = ${stepNum}
} | ConvertTo-Json
$response = Invoke-WebRequest -Uri "http://localhost:9876/capture-screenshot" -Method POST -ContentType "application/json" -Body $body -UseBasicParsing
$result = $response.Content | ConvertFrom-Json
Write-Host "Screenshot saved: $($result.data.screenshot_path)"
\`\`\`

The screenshot will be saved to \`.dev-logs/screenshots/\` and logged to \`ai-output.jsonl\` for your analysis.
After capture, use the Read tool to view the screenshot for visual verification.`;
          return instruction;
        } else {
          const instruction = `### Step ${stepNum}: Navigate to State "${step.name}"

Navigate to the state using the MCP tool:
\`\`\`
mcp__qontinui__go_to_state with state_names=["${step.name}"], take_screenshot=${step.takeScreenshot}
\`\`\``;
          return instruction;
        }
      })
      .join("\n\n");
  };

  const generateLogCheckCommands = () => {
    const enabledLogSources = (projectLogs.config?.logSources || []).filter((s) => s.enabled);
    if (enabledLogSources.length > 0) {
      return enabledLogSources
        .map(
          (s) =>
            `# ${s.name}\nGet-Content "${s.path.replace(/\\/g, "\\\\")}" -Tail ${s.tailLines || 200} | Select-String -Pattern "error|exception|failed" -CaseSensitive:$false | Select-Object -Last 30`,
        )
        .join("\n\n");
    }
    return "# No project logs configured - configure in the Logs tab";
  };

  // Generate input capture validation section (conditional)
  const _generateInputCaptureSection = () => {
    const devLogsEscaped = workspacePaths?.dev_logs_path_escaped || "{{DEV_LOGS_ESCAPED}}";

    if (!captureInputValidation) {
      return "*(Input capture validation disabled - enable in Advanced settings to compare actual vs reported clicks)*";
    }

    return `**Level 2: External Input Capture Validation**
Input capture is enabled. Compare actual mouse positions with reported positions:

\`\`\`powershell
$inputEventsDir = "${devLogsEscaped}\\\\input_events"
if (Test-Path $inputEventsDir) {
    $inputFile = Get-ChildItem $inputEventsDir -Filter "*_events.jsonl" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if ($inputFile) {
        Write-Host "Input capture file: $($inputFile.Name)"
        $actualClicks = Get-Content $inputFile.FullName | ForEach-Object { $_ | ConvertFrom-Json } | Where-Object { $_.event_type -eq "mouse_click" }
        Write-Host "Actual mouse clicks:"
        $actualClicks | Format-Table timestamp, x, y, button -AutoSize

        # Compare with reported positions
        $reported = Get-Content "${devLogsEscaped}\\\\runner-image-recognition.jsonl" | ForEach-Object { $_ | ConvertFrom-Json } | Where-Object { $_.found -eq $true }
        Write-Host "Reported find positions:"
        $reported | Format-Table timestamp, image_name, @{N='x';E={$_.location.x}}, @{N='y';E={$_.location.y}} -AutoSize

        # Flag coordinate discrepancies > 20px
        Write-Host "Check if actual clicks are within 20px of reported find locations!"
    }
}
\`\`\``;
  };

  // Generate developer mode prompt
  // Note: Runner handles session continuation deterministically via checkpoint file
  const generateDeveloperPrompt = (sessionId: string) => {
    const goalStr = goal.trim() || "Verify automation works correctly";

    // Get paths - use placeholders if not loaded yet (for preview before paths are fetched)
    const devLogsEscaped = workspacePaths?.dev_logs_path_escaped || "{{DEV_LOGS_ESCAPED}}";
    const workspaceEscaped = workspacePaths?.workspace_root_escaped || "{{WORKSPACE_ESCAPED}}";

    // Generate log monitoring section from Project Logs configuration
    let logMonitoringSection = "";
    const enabledLogSources = (projectLogs.config?.logSources || []).filter((s) => s.enabled);

    if (enabledLogSources.length > 0) {
      logMonitoringSection = `**Log Monitoring:** Enabled (${enabledLogSources.length} sources from Project Logs)
**Log Sources:**
${enabledLogSources.map((s) => `- ${s.name}: ${s.path}`).join("\n")}`;
    } else {
      logMonitoringSection =
        "**Log Monitoring:** Disabled (only runner logs will be checked)\nConfigure log sources in the Logs tab to enable application log monitoring.";
    }

    return getDeveloperPromptTemplate()
      .replace("{{SESSION_ID}}", sessionId)
      .replace(/\{\{SESSION_ID\}\}/g, sessionId)
      .replace("{{ITERATION}}", "1")
      .replace(/\{\{ITERATION\}\}/g, "1")
      .replace("{{MAX_ITERATIONS}}", String(maxIterations))
      .replace(/\{\{MAX_ITERATIONS\}\}/g, String(maxIterations))
      .replace("{{STEP_COUNT}}", String(executionSteps.length))
      .replace("{{EXECUTION_STEPS}}", generateExecutionInstructions())
      .replace("{{GOAL}}", goalStr)
      .replace("{{LOG_MONITORING_SECTION}}", logMonitoringSection)
      .replace("{{LOG_CHECK_COMMANDS}}", generateLogCheckCommands())
      .replace(/\{\{DEV_LOGS_ESCAPED\}\}/g, devLogsEscaped)
      .replace(/\{\{WORKSPACE_ESCAPED\}\}/g, workspaceEscaped);
  };

  // Generate prompt for preview
  const generatePrompt = (sessionId: string) => {
    return generateDeveloperPrompt(sessionId);
  };

  // Copy prompt to clipboard (preview mode - uses placeholder session ID)
  const copyPrompt = async () => {
    const previewSessionId = "preview-" + Date.now().toString(36);
    const prompt = generatePrompt(previewSessionId);
    await navigator.clipboard.writeText(prompt);
    setLastResult({ success: true, message: "Prompt copied to clipboard!" });
    setTimeout(() => setLastResult(null), 3000);
  };

  // Load an AI Workflow from the AI Workflows library
  const loadAiWorkflow = (workflow: SavedAiWorkflow) => {
    setExecutionSteps(workflow.steps);
    setGoal(workflow.goal);
    setMaxIterations(workflow.max_iterations);
    setCaptureInputValidation(workflow.capture_input_validation);
    setShowWorkflowsPanel(false);
    // Track the loaded workflow for "Save" vs "Save As" behavior
    setCurrentWorkflowId(workflow.id);
    setCurrentWorkflowName(workflow.name);
    setHasUnsavedChanges(false);
    setLastResult({ success: true, message: `Loaded workflow "${workflow.name}"` });
    setTimeout(() => setLastResult(null), 3000);
  };

  // Create a new workflow (reset to blank state)
  const handleNewWorkflow = () => {
    // Check for unsaved changes first
    if (hasUnsavedChanges) {
      setShowNewConfirmDialog(true);
      return;
    }
    // No unsaved changes, proceed directly
    resetToNewWorkflow();
  };

  // Actually reset to a new blank workflow
  const resetToNewWorkflow = () => {
    setExecutionSteps([]);
    setGoal("");
    setMaxIterations(5);
    setCaptureInputValidation(false);
    setCurrentWorkflowId(null);
    setCurrentWorkflowName("");
    setHasUnsavedChanges(false);
    setShowNewConfirmDialog(false);
    setLastResult({ success: true, message: "Created new workflow" });
    setTimeout(() => setLastResult(null), 3000);
  };

  // Delete an AI Workflow
  const deleteAiWorkflow = async (workflowId: string) => {
    try {
      const response = await fetch(`http://localhost:9876/ai-workflows/${workflowId}`, {
        method: "DELETE",
      });
      const result = await response.json();
      if (result.success) {
        await fetchSavedAiWorkflows();
        setLastResult({ success: true, message: "Workflow deleted" });
        setTimeout(() => setLastResult(null), 3000);
      } else {
        throw new Error(result.error || "Failed to delete workflow");
      }
    } catch (error) {
      setLastResult({
        success: false,
        message: `Error deleting: ${error instanceof Error ? error.message : String(error)}`,
      });
    }
  };

  // Save current workflow (update existing or prompt for new)
  const handleSaveWorkflow = async () => {
    if (currentWorkflowId) {
      // Update existing workflow
      await updateCurrentWorkflow();
    } else {
      // Show dialog for new workflow
      setSaveName(goal.trim() || "AI Builder Workflow");
      setShowSaveDialog(true);
    }
  };

  // Update the currently loaded workflow
  const updateCurrentWorkflow = async () => {
    if (!currentWorkflowId) return;

    setIsSaving(true);
    try {
      const response = await fetch(`http://localhost:9876/ai-workflows/${currentWorkflowId}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: currentWorkflowName,
          description: "",
          steps: executionSteps,
          goal: goal.trim(),
          max_iterations: maxIterations,
          persistent_session: true, // Always use persistent/task mode
          capture_input_validation: captureInputValidation,
          category: "",
          tags: [],
        }),
      });

      const result = await response.json();
      if (result.success) {
        setLastResult({ success: true, message: `Workflow "${currentWorkflowName}" saved!` });
        setHasUnsavedChanges(false);
        await fetchSavedAiWorkflows();
      } else {
        throw new Error(result.error || "Failed to save workflow");
      }
    } catch (error) {
      setLastResult({
        success: false,
        message: `Error saving: ${error instanceof Error ? error.message : String(error)}`,
      });
    } finally {
      setIsSaving(false);
    }
    setTimeout(() => setLastResult(null), 3000);
  };

  // Save as new workflow (from dialog)
  const saveAsNewWorkflow = async () => {
    if (!saveName.trim()) {
      setLastResult({ success: false, message: "Please enter a name for the workflow" });
      return;
    }

    setIsSaving(true);
    try {
      const response = await fetch("http://localhost:9876/ai-workflows", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: saveName.trim(),
          description: saveDescription.trim(),
          steps: executionSteps,
          goal: goal.trim(),
          max_iterations: maxIterations,
          persistent_session: true, // Always use persistent/task mode
          capture_input_validation: captureInputValidation,
          category: "",
          tags: [],
        }),
      });

      const result = await response.json();
      if (result.success) {
        setLastResult({ success: true, message: `Workflow "${saveName}" saved!` });
        setShowSaveDialog(false);
        // Track the new workflow as current
        if (result.data?.id) {
          setCurrentWorkflowId(result.data.id);
          setCurrentWorkflowName(saveName.trim());
        }
        setHasUnsavedChanges(false);
        setSaveName("");
        setSaveDescription("");
        await fetchSavedAiWorkflows();
      } else {
        throw new Error(result.error || "Failed to save workflow");
      }
    } catch (error) {
      setLastResult({
        success: false,
        message: `Error saving: ${error instanceof Error ? error.message : String(error)}`,
      });
    } finally {
      setIsSaving(false);
    }
  };

  // Run the automation using task-based execution
  const runAutomation = async () => {
    console.log("[AI_BUILDER] Starting AI session...");
    setIsRunning(true);
    setLastResult(null);

    try {
      // Generate unique session ID
      const sessionId = Date.now().toString(36) + Math.random().toString(36).substring(2, 7);
      console.log("[AI_BUILDER] Generated session ID:", sessionId);

      // Note: Input capture is now automatically started/stopped with workflow execution
      // in Python when captureInputValidation is enabled (synced via useEffect)

      // Generate prompt
      const prompt = generateDeveloperPrompt(sessionId);
      console.log("[AI_BUILDER] Generated prompt length:", prompt.length);

      // Add to history
      const historyEntry: PromptHistoryEntry = {
        id: sessionId,
        timestamp: Date.now(),
        steps: [...executionSteps],
        goal: goal.trim() || "Verify automation works correctly",
      };
      setHistory((prev) => [historyEntry, ...prev]);

      // Start session via unified sessions API
      console.log("[AI_BUILDER] Starting session via unified sessions API...");
      const response = await fetch("http://localhost:9876/sessions/start", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: goal.trim() || "AI Builder Session",
          prompt: prompt,
          continuation_prompt: null,
          total_phases: maxIterations,
          uses_gui: false,
          timeout_seconds: 1800,
        }),
      });
      const result = await response.json();
      console.log("[AI_BUILDER] sessions/start response:", result);

      if (result.success && result.data?.session) {
        const session = result.data.session as Session;
        setCurrentSessionId(session.id);
        setLastResult({
          success: true,
          message: `AI Builder session ${session.id} started!`,
        });
        setHistory((prev) => prev.map((h) => (h.id === sessionId ? { ...h, success: true } : h)));
      } else {
        throw new Error(result.error || "Failed to start AI Builder session");
      }
    } catch (error) {
      setLastResult({
        success: false,
        message: `Error: ${error instanceof Error ? error.message : String(error)}`,
      });
      setHistory((prev) => {
        const lastEntry = prev[0];
        if (lastEntry) {
          return [{ ...lastEntry, success: false }, ...prev.slice(1)];
        }
        return prev;
      });
      setIsRunning(false);
    }
  };

  // Stop the current session
  const stopSession = async () => {
    if (!currentSessionId) return;

    try {
      // Use new unified sessions API
      const response = await fetch(`http://localhost:9876/sessions/${currentSessionId}/stop`, {
        method: "POST",
      });
      const result = await response.json();

      if (result.success) {
        setLastResult({ success: true, message: "Stop requested. Session will exit gracefully." });
        setIsRunning(false);
      } else {
        setLastResult({
          success: false,
          message: result.error || "Failed to stop session",
        });
      }
    } catch (error) {
      setLastResult({
        success: false,
        message: `Error: ${error instanceof Error ? error.message : String(error)}`,
      });
    }
  };

  const loadFromHistory = (entry: PromptHistoryEntry) => {
    setExecutionSteps(entry.steps);
    setGoal(entry.goal);
  };

  const getHistorySummary = (entry: PromptHistoryEntry): string => {
    const workflowCount = entry.steps.filter((s) => s.type === "workflow").length;
    const stateCount = entry.steps.filter((s) => s.type === "state").length;
    const playwrightCount = entry.steps.filter((s) => s.type === "playwright").length;
    const promptCount = entry.steps.filter((s) => s.type === "prompt").length;
    const parts: string[] = [];
    if (workflowCount > 0) parts.push(`${workflowCount} workflow${workflowCount > 1 ? "s" : ""}`);
    if (stateCount > 0) parts.push(`${stateCount} state${stateCount > 1 ? "s" : ""}`);
    if (playwrightCount > 0) parts.push(`${playwrightCount} test${playwrightCount > 1 ? "s" : ""}`);
    if (promptCount > 0) parts.push(`${promptCount} prompt${promptCount > 1 ? "s" : ""}`);
    return parts.join(" • ") || "No steps";
  };

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
            Run automation sequences, capture screenshots, and let AI analyze/fix issues in a loop
          </p>
        </div>
      </div>

      {/* AI Builder always shows the UI - works without a config (Playwright tests and prompts are independent) */}
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
                    {/* No config loaded message */}
                    {!execution.configLoaded && (
                      <div className="px-3 py-3 border-b border-border">
                        <div className="flex items-center gap-2 text-amber-500 mb-2">
                          <AlertTriangle className="w-4 h-4" />
                          <span className="text-sm font-medium">No Config Loaded</span>
                        </div>
                        <p className="text-xs text-muted-foreground mb-2">
                          Qontinui automation workflows, states, and click actions are only
                          available when a configuration is loaded.
                        </p>
                        <button
                          onClick={() => {
                            setShowAddDropdown(false);
                            execution.loadConfiguration();
                          }}
                          className="w-full flex items-center justify-center gap-2 px-3 py-2 text-sm bg-primary/10 text-primary rounded hover:bg-primary/20 transition-colors"
                        >
                          <FolderOpen className="w-4 h-4" />
                          Load Configuration
                        </button>
                      </div>
                    )}

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

                    {/* Playwright Scripts section */}
                    {playwrightScripts.length > 0 && (
                      <>
                        <div className="px-3 py-2 text-xs font-semibold text-muted-foreground bg-muted/30 border-b border-border flex items-center gap-2">
                          <TestTube className="w-3 h-3" />
                          Playwright Tests
                        </div>
                        {playwrightScripts.map((script) => (
                          <button
                            key={script.id}
                            onClick={() =>
                              addStep(
                                "playwright",
                                script.name,
                                script.id,
                                script.script_content,
                                script.target_url,
                              )
                            }
                            className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors"
                          >
                            <TestTube className="w-4 h-4 text-green-500" />
                            <span>{script.name}</span>
                            {script.target_url && (
                              <span className="text-xs text-muted-foreground ml-auto truncate max-w-20">
                                {script.target_url}
                              </span>
                            )}
                          </button>
                        ))}
                      </>
                    )}

                    {/* Prompts section */}
                    {savedPrompts.length > 0 && (
                      <>
                        <div className="px-3 py-2 text-xs font-semibold text-muted-foreground bg-muted/30 border-b border-border flex items-center gap-2">
                          <FileText className="w-3 h-3" />
                          Prompts
                        </div>
                        {savedPrompts.map((prompt) => (
                          <button
                            key={prompt.id}
                            onClick={() =>
                              addStep(
                                "prompt",
                                prompt.name,
                                undefined,
                                undefined,
                                undefined,
                                prompt.id,
                                prompt.content,
                              )
                            }
                            className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors"
                          >
                            <FileText className="w-4 h-4 text-amber-500" />
                            <span className="truncate">{prompt.name}</span>
                            {prompt.category && (
                              <span className="text-xs text-muted-foreground ml-auto">
                                {prompt.category}
                              </span>
                            )}
                          </button>
                        ))}
                      </>
                    )}

                    {/* Click Image section */}
                    {images.length > 0 && (
                      <>
                        <div className="px-3 py-2 text-xs font-semibold text-muted-foreground bg-muted/30 border-b border-border flex items-center gap-2">
                          <MousePointer2 className="w-3 h-3" />
                          Click Image
                        </div>
                        {images.map((image) => (
                          <button
                            key={`${image.stateName}-${image.id}`}
                            onClick={() => addActionStep("click", image.id, image.name)}
                            className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors"
                          >
                            <MousePointer2 className="w-4 h-4 text-blue-500" />
                            <span className="truncate">{image.name}</span>
                            <span className="text-xs text-muted-foreground ml-auto">
                              {image.stateName}
                            </span>
                          </button>
                        ))}
                      </>
                    )}

                    {/* Capture section */}
                    <div className="px-3 py-2 text-xs font-semibold text-muted-foreground bg-muted/30 border-b border-border flex items-center gap-2">
                      <Camera className="w-3 h-3" />
                      Capture
                    </div>
                    <button
                      onClick={() => addScreenshotStep()}
                      className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-muted/30 transition-colors"
                    >
                      <Camera className="w-4 h-4 text-cyan-500" />
                      <span>Screenshot</span>
                      <span className="text-xs text-muted-foreground ml-auto">for AI analysis</span>
                    </button>
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
                        : step.type === "playwright"
                          ? "bg-green-500/5 border-green-500/20"
                          : step.type === "prompt"
                            ? "bg-amber-500/5 border-amber-500/20"
                            : step.type === "action"
                              ? "bg-blue-500/5 border-blue-500/20"
                              : step.type === "screenshot"
                                ? "bg-cyan-500/5 border-cyan-500/20"
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
                    ) : step.type === "playwright" ? (
                      <TestTube className="w-4 h-4 text-green-500 flex-shrink-0" />
                    ) : step.type === "prompt" ? (
                      <FileText className="w-4 h-4 text-amber-500 flex-shrink-0" />
                    ) : step.type === "action" ? (
                      <MousePointer2 className="w-4 h-4 text-blue-500 flex-shrink-0" />
                    ) : step.type === "screenshot" ? (
                      <Camera className="w-4 h-4 text-cyan-500 flex-shrink-0" />
                    ) : (
                      <Target className="w-4 h-4 text-primary flex-shrink-0" />
                    )}

                    {/* Name */}
                    <span className="flex-1 text-sm truncate">{step.name}</span>

                    {/* Screenshot step: monitor selector and delay */}
                    {step.type === "screenshot" && (
                      <div className="flex items-center gap-2">
                        <select
                          value={
                            step.screenshotMonitor === "all"
                              ? "all"
                              : (step.screenshotMonitor ?? "all")
                          }
                          onChange={(e) => {
                            const val = e.target.value;
                            updateScreenshotMonitor(
                              step.id,
                              val === "all" ? "all" : parseInt(val, 10),
                            );
                          }}
                          className="px-1 py-0.5 text-xs bg-background border border-border rounded"
                          title="Monitor to capture"
                          onClick={(e) => e.stopPropagation()}
                        >
                          <option value="all">All monitors</option>
                          <option value="0">Monitor 0</option>
                          <option value="1">Monitor 1</option>
                          <option value="2">Monitor 2</option>
                        </select>
                        <div
                          className="flex items-center gap-0.5"
                          title="Delay before capture (seconds)"
                        >
                          <input
                            type="number"
                            min={0}
                            max={30}
                            step={0.5}
                            value={step.screenshotDelay ?? 0}
                            onChange={(e) =>
                              updateScreenshotDelay(step.id, parseFloat(e.target.value) || 0)
                            }
                            className="w-12 px-1 py-0.5 text-xs text-center bg-background border border-border rounded"
                            onClick={(e) => e.stopPropagation()}
                          />
                          <span className="text-xs text-muted-foreground">s</span>
                        </div>
                      </div>
                    )}

                    {/* Screenshot toggle for other step types (not applicable to prompts or screenshot steps) */}
                    {step.type !== "prompt" && step.type !== "screenshot" && (
                      <div className="flex items-center gap-1">
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
                        {step.takeScreenshot && (
                          <div
                            className="flex items-center gap-0.5"
                            title="Screenshot delay (seconds)"
                          >
                            <input
                              type="number"
                              min={0}
                              max={30}
                              step={0.5}
                              value={step.screenshotDelay ?? 0}
                              onChange={(e) =>
                                updateScreenshotDelay(step.id, parseFloat(e.target.value) || 0)
                              }
                              className="w-12 px-1 py-0.5 text-xs text-center bg-background border border-border rounded"
                              onClick={(e) => e.stopPropagation()}
                            />
                            <span className="text-xs text-muted-foreground">s</span>
                          </div>
                        )}
                      </div>
                    )}

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

          {/* Saved Workflows */}
          <div className="card p-4 space-y-3">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <BookOpen className="w-4 h-4 text-green-500" />
                <span className="font-medium">Saved Workflows</span>
                <span className="text-xs text-muted-foreground">({savedAiWorkflows.length})</span>
              </div>
              <button
                onClick={() => setShowWorkflowsPanel(!showWorkflowsPanel)}
                className="flex items-center gap-1 px-2 py-1 text-xs bg-green-500/10 text-green-600 rounded hover:bg-green-500/20 transition-colors"
              >
                {showWorkflowsPanel ? "Hide" : "Show"}
                <ChevronDown
                  className={`w-3 h-3 transition-transform ${showWorkflowsPanel ? "rotate-180" : ""}`}
                />
              </button>
            </div>

            {showWorkflowsPanel && (
              <div className="space-y-2 max-h-48 overflow-y-auto">
                {savedAiWorkflows.length === 0 ? (
                  <p className="text-sm text-muted-foreground text-center py-4">
                    No saved workflows yet. Use the Save button to save your current workflow.
                  </p>
                ) : (
                  savedAiWorkflows.map((workflow) => (
                    <div
                      key={workflow.id}
                      className="flex items-center gap-2 p-2 bg-background rounded-md border border-border/50 hover:border-green-500/30 transition-colors"
                    >
                      <Sparkles className="w-4 h-4 text-green-500 flex-shrink-0" />
                      <div className="flex-1 min-w-0">
                        <p className="text-sm font-medium truncate">{workflow.name}</p>
                        <p className="text-xs text-muted-foreground truncate">
                          {workflow.steps.length} step{workflow.steps.length !== 1 ? "s" : ""}
                        </p>
                      </div>
                      <button
                        onClick={() => loadAiWorkflow(workflow)}
                        className="px-2 py-1 text-xs bg-green-500/10 text-green-600 rounded hover:bg-green-500/20 transition-colors"
                      >
                        Load
                      </button>
                      <button
                        onClick={() => deleteAiWorkflow(workflow.id)}
                        className="p-1 text-muted-foreground hover:text-red-500 transition-colors"
                        title="Delete workflow"
                      >
                        <Trash2 className="w-4 h-4" />
                      </button>
                    </div>
                  ))
                )}
              </div>
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
              onChange={(e) => {
                setGoal(e.target.value);
                setHasUnsavedChanges(true);
              }}
              placeholder="Describe what you want to verify or achieve..."
              className="w-full h-24 px-3 py-2 bg-background border border-border rounded-md text-sm resize-none focus:outline-none focus:ring-2 focus:ring-primary"
            />
          </div>

          {/* Settings */}
          <div className="card p-4 space-y-3">
            <div className="flex items-center gap-2">
              <Settings className="w-4 h-4 text-muted-foreground" />
              <span className="font-medium">Settings</span>
            </div>

            {/* Project Logs Status */}
            <div className="text-xs space-y-1">
              {projectLogs.config?.logSources &&
              projectLogs.config.logSources.filter((s) => s.enabled).length > 0 ? (
                <>
                  <div className="flex items-center gap-2 text-green-600">
                    <CheckCircle className="w-3 h-3" />
                    <span>
                      {projectLogs.config.logSources.filter((s) => s.enabled).length} log source(s)
                      will be monitored
                    </span>
                  </div>
                  <div className="text-muted-foreground pl-5">
                    {projectLogs.config.logSources
                      .filter((s) => s.enabled)
                      .map((s) => s.name)
                      .join(", ")}
                  </div>
                </>
              ) : (
                <div className="flex items-center gap-2 text-yellow-600">
                  <Info className="w-3 h-3" />
                  <span>
                    No log sources configured.{" "}
                    <button
                      onClick={onNavigateToLogLocations}
                      className="underline hover:text-yellow-500 transition-colors"
                    >
                      Configure in Log Locations
                    </button>
                  </span>
                </div>
              )}
            </div>

            {/* Auto-Continue Toggle */}
            <div className="flex items-center justify-between p-2 bg-muted/30 rounded-md">
              <div className="flex items-center gap-2">
                <RotateCcw className="w-4 h-4 text-orange-500" />
                <div>
                  <span className="text-sm font-medium">Auto-Continue on Restart</span>
                  <p className="text-xs text-muted-foreground">
                    {autoContinueEnabled
                      ? "Workflows resume automatically after runner restart"
                      : "Use the Continue button to resume after restart"}
                  </p>
                </div>
              </div>
              <button
                onClick={toggleAutoContinue}
                disabled={autoContinueLoading}
                className={`flex items-center transition-colors ${autoContinueEnabled ? "text-orange-500" : "text-muted-foreground"} ${autoContinueLoading ? "opacity-50" : ""}`}
                title={autoContinueEnabled ? "Auto-continue enabled" : "Auto-continue disabled"}
              >
                {autoContinueLoading ? (
                  <Loader2 className="w-8 h-8 animate-spin" />
                ) : autoContinueEnabled ? (
                  <ToggleRight className="w-8 h-8" />
                ) : (
                  <ToggleLeft className="w-8 h-8" />
                )}
              </button>
            </div>
          </div>

          {/* Advanced Settings - Collapsible */}
          <CollapsiblePanel
            title="Advanced"
            icon={<Code className="w-4 h-4" />}
            defaultCollapsed={true}
            storageKey="ai-builder-advanced"
          >
            <div className="space-y-3">
              {/* Execution Info with hover tooltip */}
              <div className="flex items-center gap-4 flex-wrap">
                <label className="flex items-center gap-2 text-sm">
                  <span className="text-muted-foreground">Max Iterations:</span>
                  <input
                    type="number"
                    min={1}
                    max={50}
                    value={maxIterations}
                    onChange={(e) => {
                      setMaxIterations(Math.max(1, Math.min(50, parseInt(e.target.value) || 10)));
                      setHasUnsavedChanges(true);
                    }}
                    className="w-16 px-2 py-1 bg-background border border-border rounded text-sm"
                  />
                </label>
                <div className="relative group">
                  <Info className="w-4 h-4 text-muted-foreground hover:text-foreground cursor-help" />
                  <div className="absolute left-0 bottom-full mb-2 w-72 p-3 bg-popover border border-border rounded-lg shadow-lg opacity-0 invisible group-hover:opacity-100 group-hover:visible transition-all duration-200 z-50">
                    <p className="text-xs font-medium text-foreground mb-2">How it works</p>
                    <div className="text-xs text-muted-foreground space-y-1">
                      <p>
                        Each iteration spawns a new AI session. AI runs automation, analyzes
                        results, and fixes issues.
                      </p>
                      <p>Loop continues until all checks pass or max iterations reached.</p>
                    </div>
                  </div>
                </div>
              </div>

              {/* Input Capture for Coordinate Validation Toggle */}
              <div className="flex items-center justify-between p-2 bg-muted/30 rounded-md">
                <div className="flex items-center gap-2">
                  <MousePointer2 className="w-4 h-4 text-purple-500" />
                  <div>
                    <div className="flex items-center gap-1.5">
                      <span className="text-sm font-medium">Capture Input for Validation</span>
                      <div className="relative group">
                        <Info className="w-3.5 h-3.5 text-muted-foreground hover:text-foreground cursor-help" />
                        <div className="absolute left-0 bottom-full mb-2 w-72 p-3 bg-popover border border-border rounded-lg shadow-lg opacity-0 invisible group-hover:opacity-100 group-hover:visible transition-all duration-200 z-50">
                          <p className="text-xs font-medium text-foreground mb-2">
                            Debugging/Validation Feature
                          </p>
                          <div className="space-y-2 text-xs text-muted-foreground">
                            <p>
                              <strong>When Disabled:</strong> Only reported positions from the
                              automation engine are logged (where clicks <em>should</em> happen).
                            </p>
                            <p>
                              <strong>When Enabled:</strong> Captures actual mouse/keyboard events
                              during execution and compares them to reported positions.
                            </p>
                            <p className="pt-1 border-t border-border mt-2">
                              <strong>Use Case:</strong> Debug clicks missing targets due to
                              multi-monitor offsets, DPI scaling, or coordinate calculation bugs.
                            </p>
                          </div>
                        </div>
                      </div>
                    </div>
                    <p className="text-xs text-muted-foreground">
                      {captureInputValidation
                        ? "Records actual mouse/keyboard to compare with reported positions"
                        : "Disabled - only reported positions are logged"}
                    </p>
                  </div>
                </div>
                <button
                  onClick={() => {
                    setCaptureInputValidation(!captureInputValidation);
                    setHasUnsavedChanges(true);
                  }}
                  className={`flex items-center transition-colors ${captureInputValidation ? "text-purple-500" : "text-muted-foreground"}`}
                  title={
                    captureInputValidation ? "Input capture enabled" : "Input capture disabled"
                  }
                >
                  {captureInputValidation ? (
                    <ToggleRight className="w-8 h-8" />
                  ) : (
                    <ToggleLeft className="w-8 h-8" />
                  )}
                </button>
              </div>

              {/* When Input Capture is enabled, show explanation */}
              {captureInputValidation && (
                <div className="space-y-2 pl-2 border-l-2 border-purple-500/30">
                  <p className="text-xs text-muted-foreground">
                    <strong>For Qontinui development:</strong> Captures actual mouse clicks during
                    automation to detect coordinate calculation bugs (when reported click position
                    differs from actual).
                  </p>
                  <p className="text-xs text-muted-foreground">
                    Captured input will be logged to{" "}
                    <code className="bg-muted px-1 rounded">.dev-logs/input_events/</code>
                  </p>
                </div>
              )}
            </div>
          </CollapsiblePanel>

          {/* Continue Workflow Button - Shows when there's a resumable workflow */}
          {resumableWorkflow && !isRunning && (
            <div className="card p-4 border-2 border-orange-500/50 bg-orange-500/5">
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-3">
                  <div className="p-2 bg-orange-500/20 rounded-lg">
                    <RotateCcw className="w-5 h-5 text-orange-500" />
                  </div>
                  <div>
                    <h4 className="font-medium text-foreground">Continue Previous Workflow</h4>
                    <p className="text-sm text-muted-foreground">
                      <span className="font-medium">{resumableWorkflow.name}</span>
                      {resumableWorkflow.totalPhases > 0 && (
                        <span className="ml-2">
                          • Step {resumableWorkflow.currentPhase} of {resumableWorkflow.totalPhases}
                        </span>
                      )}
                      <span className="ml-2 capitalize">• {resumableWorkflow.status}</span>
                    </p>
                  </div>
                </div>
                <button
                  onClick={handleResumeWorkflow}
                  disabled={isResuming}
                  className="flex items-center gap-2 px-4 py-2 bg-orange-500 text-white rounded-md font-medium hover:bg-orange-600 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                >
                  {isResuming ? (
                    <>
                      <Loader2 className="w-4 h-4 animate-spin" />
                      Resuming...
                    </>
                  ) : (
                    <>
                      <Play className="w-4 h-4" />
                      Continue
                    </>
                  )}
                </button>
              </div>
            </div>
          )}

          {/* Actions */}
          <div className="flex gap-3">
            {isRunning ? (
              // Stop button when running
              <button
                onClick={stopSession}
                className="flex-1 flex items-center justify-center gap-2 px-4 py-3 bg-red-500 text-white rounded-md font-medium hover:bg-red-600 transition-colors"
              >
                <Square className="w-4 h-4" />
                Stop Session
              </button>
            ) : sessionState &&
              (sessionState.status === "complete" || sessionState.status === "stopped") ? (
              // New session button when previous session completed
              <button
                onClick={() => {
                  setCurrentSessionId(null);
                  setSessionState(null);
                  setClaudeLog("");
                  setClaudeLogInfo(null);
                  setLastResult(null);
                }}
                className="flex-1 flex items-center justify-center gap-2 px-4 py-3 bg-primary text-primary-foreground rounded-md font-medium hover:bg-primary/90 transition-colors"
              >
                <RefreshCw className="w-4 h-4" />
                New Session
              </button>
            ) : (
              // Start button
              <button
                onClick={runAutomation}
                disabled={isRunning}
                className="flex-1 flex items-center justify-center gap-2 px-4 py-3 rounded-md font-medium disabled:opacity-50 disabled:cursor-not-allowed transition-colors bg-primary text-primary-foreground hover:bg-primary/90"
              >
                <Play className="w-4 h-4" />
                Start AI Analysis
              </button>
            )}

            <button
              onClick={handleNewWorkflow}
              className="flex items-center gap-2 px-4 py-3 bg-blue-500/10 text-blue-600 rounded-md font-medium hover:bg-blue-500/20 transition-colors"
              title="Create new workflow"
            >
              <FilePlus2 className="w-4 h-4" />
            </button>

            <button
              onClick={copyPrompt}
              className="flex items-center gap-2 px-4 py-3 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 transition-colors"
              title="Copy prompt to clipboard"
            >
              <Copy className="w-4 h-4" />
            </button>

            <button
              onClick={handleSaveWorkflow}
              disabled={isSaving}
              className="flex items-center gap-2 px-4 py-3 bg-green-500/10 text-green-600 rounded-md font-medium hover:bg-green-500/20 disabled:opacity-50 transition-colors"
              title={currentWorkflowId ? `Save "${currentWorkflowName}"` : "Save workflow"}
            >
              {isSaving ? (
                <Loader2 className="w-4 h-4 animate-spin" />
              ) : (
                <Save className="w-4 h-4" />
              )}
            </button>
          </div>

          {/* Session Status */}
          {sessionState && (
            <div className="card p-4 space-y-3">
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2">
                  <Activity className="w-4 h-4 text-primary" />
                  <span className="font-medium">Session Status</span>
                </div>
                <span
                  className={`text-xs px-2 py-1 rounded ${
                    sessionState.status === "running"
                      ? "bg-green-500/20 text-green-500"
                      : sessionState.status === "complete"
                        ? "bg-blue-500/20 text-blue-500"
                        : sessionState.status === "stopped"
                          ? "bg-gray-500/20 text-gray-500"
                          : "bg-yellow-500/20 text-yellow-500"
                  }`}
                >
                  {sessionState.status}
                </span>
              </div>

              <div className="grid grid-cols-2 gap-2 text-sm">
                <div>
                  <span className="text-muted-foreground">Iteration:</span>{" "}
                  <span className="font-medium">
                    {sessionState.iteration}/{sessionState.max_iterations}
                  </span>
                </div>
                <div>
                  <span className="text-muted-foreground">Errors Fixed:</span>{" "}
                  <span className="font-medium text-green-500">
                    {sessionState.errors_fixed.length}
                  </span>
                </div>
              </div>

              {sessionState.current_action && (
                <div className="text-xs text-muted-foreground truncate">
                  {sessionState.current_action}
                </div>
              )}

              {sessionState.errors_fixed.length > 0 && (
                <div className="space-y-1 max-h-24 overflow-y-auto">
                  <span className="text-xs text-muted-foreground">Recent fixes:</span>
                  {sessionState.errors_fixed.slice(-3).map((fix, i) => (
                    <div
                      key={i}
                      className="text-xs bg-green-500/10 text-green-600 p-1 rounded truncate"
                    >
                      {fix.file}: {fix.description}
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}

          {/* Result message */}
          {lastResult && (
            <div
              className={`flex items-center gap-2 p-3 rounded-md ${
                lastResult.success ? "bg-green-500/10 text-green-500" : "bg-red-500/10 text-red-500"
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
          {/* Running indicator - Direct user to AI Output sub-tab */}
          {isRunning && (
            <div className="card p-4 space-y-3 border-primary/50">
              <div className="flex items-center gap-2">
                <Loader2 className="w-4 h-4 text-primary animate-spin" />
                <span className="font-medium">Analysis in Progress</span>
              </div>
              <p className="text-sm text-muted-foreground">
                View live output in the <strong>AI Output</strong> sub-tab.
              </p>
              <p className="text-xs text-muted-foreground">
                The AI is executing your automation steps and analyzing results.
              </p>
            </div>
          )}

          {/* Prompt Preview */}
          <CollapsiblePanel
            title="Prompt Preview"
            icon={<Sparkles className="w-4 h-4" />}
            defaultCollapsed={currentSessionId !== null}
            storageKey="ai-builder-preview"
            headerExtra={
              <div className="flex items-center gap-2">
                {usingCustomTemplate && (
                  <span className="text-xs bg-blue-500/20 text-blue-400 px-2 py-0.5 rounded">
                    Custom
                  </span>
                )}
                {!isEditingPromptTemplate ? (
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      setEditedPromptTemplate(getDeveloperPromptTemplate());
                      setIsEditingPromptTemplate(true);
                    }}
                    className="text-xs text-muted-foreground hover:text-foreground px-2 py-1 rounded hover:bg-muted/50"
                    title="Edit prompt template"
                  >
                    Edit Template
                  </button>
                ) : null}
              </div>
            }
          >
            {isEditingPromptTemplate ? (
              <div className="space-y-3">
                <div className="text-xs text-muted-foreground mb-2">
                  Edit the developer prompt template. Use placeholders like{" "}
                  <code className="bg-muted px-1 rounded">{"{{GOAL}}"}</code>,{" "}
                  <code className="bg-muted px-1 rounded">{"{{EXECUTION_STEPS}}"}</code>,{" "}
                  <code className="bg-muted px-1 rounded">{"{{DEV_LOGS_ESCAPED}}"}</code>, etc.
                </div>
                <textarea
                  value={editedPromptTemplate}
                  onChange={(e) => setEditedPromptTemplate(e.target.value)}
                  className="w-full h-96 text-xs font-mono bg-background border border-border rounded-md p-3 resize-y"
                  placeholder="Enter custom prompt template..."
                />
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-2">
                    <button
                      onClick={() => {
                        saveCustomPromptTemplate(editedPromptTemplate);
                        setUsingCustomTemplate(true);
                        setIsEditingPromptTemplate(false);
                        setLastResult({ success: true, message: "Custom template saved!" });
                        setTimeout(() => setLastResult(null), 3000);
                      }}
                      className="px-3 py-1.5 text-xs bg-primary text-primary-foreground rounded hover:bg-primary/90"
                    >
                      Save Template
                    </button>
                    <button
                      onClick={() => {
                        setIsEditingPromptTemplate(false);
                        setEditedPromptTemplate("");
                      }}
                      className="px-3 py-1.5 text-xs bg-muted text-muted-foreground rounded hover:bg-muted/80"
                    >
                      Cancel
                    </button>
                  </div>
                  <button
                    onClick={() => {
                      resetPromptTemplateToDefault();
                      setUsingCustomTemplate(false);
                      setEditedPromptTemplate(DEFAULT_DEVELOPER_PROMPT_TEMPLATE);
                      setLastResult({ success: true, message: "Reset to default template" });
                      setTimeout(() => setLastResult(null), 3000);
                    }}
                    className="px-3 py-1.5 text-xs text-orange-400 hover:text-orange-300 hover:bg-orange-500/10 rounded"
                    title="Reset to the hardcoded default template"
                  >
                    Reset to Default
                  </button>
                </div>
              </div>
            ) : (
              <pre className="text-xs bg-background p-3 rounded-md overflow-auto max-h-64 whitespace-pre-wrap">
                {generatePrompt("preview-session")}
              </pre>
            )}
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
                      {entry.success === true && <CheckCircle className="w-4 h-4 text-green-500" />}
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

      {/* Save Workflow Dialog */}
      {showSaveDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          {/* Backdrop */}
          <div className="absolute inset-0 bg-black/50" onClick={() => setShowSaveDialog(false)} />

          {/* Dialog */}
          <div className="relative bg-card border border-border rounded-lg shadow-xl w-full max-w-md mx-4 p-6 space-y-4">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <Save className="w-5 h-5 text-green-500" />
                <h3 className="text-lg font-semibold">Save Workflow</h3>
              </div>
              <button
                onClick={() => setShowSaveDialog(false)}
                className="p-1 text-muted-foreground hover:text-foreground transition-colors"
              >
                <X className="w-5 h-5" />
              </button>
            </div>

            <p className="text-sm text-muted-foreground">
              Save this workflow to the Prompt Library for easy reuse.
            </p>

            <div className="space-y-3">
              <div>
                <label className="block text-sm font-medium mb-1">Name</label>
                <input
                  type="text"
                  value={saveName}
                  onChange={(e) => setSaveName(e.target.value)}
                  placeholder="Enter workflow name..."
                  className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                  autoFocus
                />
              </div>

              <div>
                <label className="block text-sm font-medium mb-1">Description (optional)</label>
                <textarea
                  value={saveDescription}
                  onChange={(e) => setSaveDescription(e.target.value)}
                  placeholder="Describe what this workflow does..."
                  className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm resize-none h-20 focus:outline-none focus:ring-2 focus:ring-primary"
                />
              </div>

              <div className="text-xs text-muted-foreground bg-muted/30 p-2 rounded">
                <p>
                  <strong>Includes:</strong>
                </p>
                <ul className="list-disc list-inside mt-1 space-y-0.5">
                  <li>
                    {executionSteps.length} execution step{executionSteps.length !== 1 ? "s" : ""}
                  </li>
                  <li>Goal: {goal.trim() || "(none set)"}</li>
                  <li>Max iterations: {maxIterations}</li>
                </ul>
              </div>
            </div>

            <div className="flex gap-3 pt-2">
              <button
                onClick={() => setShowSaveDialog(false)}
                className="flex-1 px-4 py-2 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 transition-colors"
              >
                Cancel
              </button>
              <button
                onClick={saveAsNewWorkflow}
                disabled={isSaving || !saveName.trim()}
                className="flex-1 flex items-center justify-center gap-2 px-4 py-2 bg-green-500 text-white rounded-md font-medium hover:bg-green-600 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
              >
                {isSaving ? (
                  <>
                    <Loader2 className="w-4 h-4 animate-spin" />
                    Saving...
                  </>
                ) : (
                  <>
                    <Save className="w-4 h-4" />
                    Save to Library
                  </>
                )}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* New Workflow Confirmation Dialog (Unsaved Changes Warning) */}
      {showNewConfirmDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          {/* Backdrop */}
          <div
            className="absolute inset-0 bg-black/50"
            onClick={() => setShowNewConfirmDialog(false)}
          />

          {/* Dialog */}
          <div className="relative bg-card border border-border rounded-lg shadow-xl w-full max-w-md mx-4 p-6 space-y-4">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <AlertTriangle className="w-5 h-5 text-yellow-500" />
                <h3 className="text-lg font-semibold">Unsaved Changes</h3>
              </div>
              <button
                onClick={() => setShowNewConfirmDialog(false)}
                className="p-1 text-muted-foreground hover:text-foreground transition-colors"
              >
                <X className="w-5 h-5" />
              </button>
            </div>

            <p className="text-sm text-muted-foreground">
              You have unsaved changes to your current workflow. Creating a new workflow will
              discard these changes.
            </p>

            <div className="text-xs text-muted-foreground bg-yellow-500/10 border border-yellow-500/20 p-3 rounded">
              <p>
                <strong>Current workflow includes:</strong>
              </p>
              <ul className="list-disc list-inside mt-1 space-y-0.5">
                <li>
                  {executionSteps.length} execution step{executionSteps.length !== 1 ? "s" : ""}
                </li>
                {goal.trim() && (
                  <li>
                    Goal: {goal.trim().slice(0, 50)}
                    {goal.trim().length > 50 ? "..." : ""}
                  </li>
                )}
              </ul>
            </div>

            <div className="flex gap-3 pt-2">
              <button
                onClick={() => setShowNewConfirmDialog(false)}
                className="flex-1 px-4 py-2 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 transition-colors"
              >
                Cancel
              </button>
              <button
                onClick={resetToNewWorkflow}
                className="flex-1 flex items-center justify-center gap-2 px-4 py-2 bg-red-500 text-white rounded-md font-medium hover:bg-red-600 transition-colors"
              >
                <Trash2 className="w-4 h-4" />
                Discard & Create New
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
