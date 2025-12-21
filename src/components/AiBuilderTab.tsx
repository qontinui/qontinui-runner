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
  Square,
  // FileText, // unused
  Settings,
  Activity,
  Code,
  ToggleLeft,
  ToggleRight,
  MousePointer2,
} from "lucide-react";
import { useExecution } from "../contexts";
import type { UseProjectLogsReturn } from "../hooks/useProjectLogs";
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

// Standard Mode Prompt Template - ONE iteration per session, spawns new session to continue
const STANDARD_PROMPT_TEMPLATE = `# AI Automation Analysis (Single Iteration)

Execute automation, analyze results, fix issues. If fixes applied, spawn a NEW session to re-run.

**IMPORTANT**: This session handles ONE iteration only. Fresh sessions prevent context overflow.

## Goal
{{GOAL}}

## Iteration Info
- **Iteration**: {{ITERATION}} of {{MAX_ITERATIONS}}
- **Workflow**: {{WORKFLOW_NAME}}

## Execution Steps
{{STEP_COUNT}} steps to execute:

{{EXECUTION_STEPS}}

---

## Phase 1: Execute Automation

Run each step in order using the MCP tools available.

---

## Phase 2: Analyze Results (CRITICAL - LOOK FOR ANOMALIES)

### 2.1 Check Action Logs
\`\`\`powershell
Get-Content "{{DEV_LOGS_ESCAPED}}\\runner-actions.jsonl" -Tail 100 | Select-String -Pattern '"error"|"failed"|"status":"error"'
\`\`\`

### 2.2 Check Image Recognition Logs (ANOMALY DETECTION)
\`\`\`powershell
# Get ALL image recognition results
$allLogs = Get-Content "{{DEV_LOGS_ESCAPED}}\\runner-image-recognition.jsonl" | ForEach-Object { $_ | ConvertFrom-Json }
$allLogs | Format-Table timestamp, image_name, found, confidence, location, annotated_screenshot_path -AutoSize
\`\`\`

**ANOMALY DETECTION - Look for these bugs:**

1. **REDUNDANT SEARCHES**: If the same image appears multiple times:
   - **Bug**: Image should only be searched once if found - why is it searching again?
   - Check: Group by image_name and count entries

2. **MISSING DATA in logs**: Each log entry MUST have:
   - \`annotated_screenshot_path\` - path to screenshot with match visualization
   - \`template_path\` - path to the template image being matched
   - \`location\` - MUST be a valid coordinate object {x, y} if found=true
   - **Bug**: Missing any of these fields indicates corrupt/incomplete log entry

3. **INCONSISTENT LOCATIONS**: For multiple entries of same image:
   - All found=true entries should have SAME location (within ~5px)
   - **Bug**: Different locations for same image = coordinate calculation bug

4. **IMPOSSIBLE COORDINATES**: Check for:
   - Negative x or y values (can't click negative coordinates)
   - x > 5000 or y > 3000 (beyond reasonable screen bounds)
   - **Bug**: These indicate coordinate transformation errors

\`\`\`powershell
# ANOMALY CHECK: Find duplicate image searches
$grouped = $allLogs | Group-Object image_name
$grouped | Where-Object { $_.Count -gt 1 } | ForEach-Object {
    Write-Host "ANOMALY: Image '$($_.Name)' searched $($_.Count) times - should be 1 if found!"
    $_.Group | ForEach-Object { Write-Host "  - found=$($_.found) loc=$($_.location) screenshot=$($_.annotated_screenshot_path)" }
}

# ANOMALY CHECK: Find entries with missing data
$allLogs | Where-Object {
    -not $_.annotated_screenshot_path -or
    -not $_.template_path -or
    ($_.found -eq $true -and (-not $_.location -or -not $_.location.x -or -not $_.location.y))
} | ForEach-Object {
    Write-Host "ANOMALY: Incomplete log entry for '$($_.image_name)' - missing required fields!"
    Write-Host "  screenshot_path: $($_.annotated_screenshot_path)"
    Write-Host "  template_path: $($_.template_path)"
    Write-Host "  location: $($_.location)"
}

# ANOMALY CHECK: Find impossible coordinates
$allLogs | Where-Object {
    $_.found -eq $true -and $_.location -and (
        $_.location.x -lt 0 -or $_.location.y -lt 0 -or
        $_.location.x -gt 5000 -or $_.location.y -gt 3000
    )
} | ForEach-Object {
    Write-Host "ANOMALY: Impossible coordinates for '$($_.image_name)': ($($_.location.x), $($_.location.y))"
}
\`\`\`

### 2.3 Coordinate Validation (CRITICAL)
For EVERY CLICK action, validate the click actually went where intended:

\`\`\`powershell
# Get CLICK actions with their clicked_location
Get-Content "{{DEV_LOGS_ESCAPED}}\\runner-actions.jsonl" | Where-Object { $_ -match '"type":"CLICK"' -and $_ -match 'action_completed' } | ForEach-Object { $_ | ConvertFrom-Json } | Select-Object -ExpandProperty node | Select-Object id, name, status, @{N='clicked_x';E={$_.metadata.execution_record.metadata.runtime.clicked_location.x}}, @{N='clicked_y';E={$_.metadata.execution_record.metadata.runtime.clicked_location.y}}
\`\`\`

**Compare FIND location vs CLICK location:**
- The FIND action records where the image was found
- The CLICK action should click at (or near) that same location
- **BUG**: If clicked_location differs from found location by >20px, coordinate transformation is broken

{{INPUT_CAPTURE_SECTION}}

### 2.4 View Annotated Screenshots
**CRITICAL**: Use the Read tool to view the annotated screenshots listed in the logs. These show:
- Green boxes around matched regions
- Confidence scores overlaid
- Why a match succeeded or failed

\`\`\`powershell
# List annotated screenshots
Get-Content "{{DEV_LOGS_ESCAPED}}\\runner-image-recognition.jsonl" | ForEach-Object { ($_ | ConvertFrom-Json).annotated_screenshot_path } | Where-Object { $_ } | Select-Object -Unique
\`\`\`

### 2.5 Check Application Logs
\`\`\`powershell
{{LOG_CHECK_COMMANDS}}
\`\`\`

---

## Phase 3: Decision Point

Based on analysis, choose ONE path:

### Path A: ALL PASS (No issues found)
If automation succeeded with no anomalies:
1. Report success
2. Exit - no further action needed

### Path B: ISSUES FOUND
If any bugs or anomalies were detected:
1. **Fix the issues** (see Phase 4)
2. **Spawn new session** to re-run (see Phase 5)
3. Exit this session

### Path C: MAX ITERATIONS REACHED
If iteration {{ITERATION}} >= {{MAX_ITERATIONS}}:
1. Report failure with remaining issues
2. Exit - let user review

---

## Phase 4: Fix Issues (Path B only)

**If ANY anomalies or bugs were detected:**

1. **Identify the root cause** by reading the relevant source code
2. **Implement the fix** in the qontinui-runner codebase:
   - Image recognition issues: Check \`python-bridge/\` and \`src-tauri/src/\`
   - Coordinate bugs: Check coordinate transformation logic
   - Action execution: Check \`src-tauri/src/executor/\` or action handlers
3. **Verify the fix compiles**: Run \`npm run typecheck\` or \`cargo check\`

**Common bug patterns:**
- Searching for already-found images → fix early return in find logic
- Missing log fields → fix logger to include all required fields
- Coordinate bugs → fix monitor offset calculations, DPI scaling

---

## Phase 5: Spawn New Session to Continue (Path B only)

**CRITICAL**: After fixing, spawn a FRESH Claude session to re-run automation.

Write the continuation prompt and spawn:
\`\`\`powershell
# Write continuation prompt to file
$nextIteration = {{ITERATION}} + 1
$prompt = @"
Continue AI automation analysis loop.

Previous iteration: {{ITERATION}}
Current iteration: $nextIteration
Max iterations: {{MAX_ITERATIONS}}

Fixes applied in previous iteration:
- [List what you fixed here]

INSTRUCTIONS:
1. Re-run the workflow: mcp__qontinui__run_workflow with workflow_name="{{WORKFLOW_NAME}}" {{MONITOR_PARAM}}
2. Analyze results for remaining issues
3. Fix any new issues found
4. If still failing, spawn another session (up to max iterations)
"@
$prompt | Set-Content "{{DEV_LOGS_ESCAPED}}\\ai-builder-continuation.txt"

# Spawn fresh Claude session
python "{{SPAWN_SCRIPT_ESCAPED}}" --file "{{DEV_LOGS_ESCAPED}}\\ai-builder-continuation.txt"
\`\`\`

**Then EXIT this session.** The new session will continue with fresh context.

---

## Phase 6: Report (Before Exiting)

Always end with a summary before exiting:

\`\`\`
## Iteration {{ITERATION}} Summary

### Execution Result
- **Steps Executed:** X/Y successful
- **Overall Status:** [PASS/FAIL/CONTINUING]

### Anomalies Detected
- Redundant searches: [list or "None"]
- Missing log data: [list or "None"]
- Coordinate bugs: [list or "None"]
- Impossible coordinates: [list or "None"]

### Fixes Applied This Iteration
1. {file}:{line} - {description}
2. ...

### Next Action
[SUCCESS - done / SPAWNED new session / MAX_ITERATIONS reached]
\`\`\`

---

## Rules

- **Work AUTONOMOUSLY** - never ask the user questions
- **ONE ITERATION per session** - spawn new session to continue
- **FIX issues found** - don't just report them
- **Look for ANOMALIES** - not just explicit errors
- **View screenshots** - they show visual match quality
- **EXIT after spawning** - don't loop in same session
`;

// Developer Mode Prompt Template - uses placeholders that get replaced with dynamic paths
const DEVELOPER_PROMPT_TEMPLATE = `# AI Developer Loop

Execute automation steps, analyze results, fix issues, and continue until success.

## Session Info

**Session ID:** {{SESSION_ID}}
**Iteration:** {{ITERATION}}/{{MAX_ITERATIONS}}
**State File:** {{STATE_FILE}}

## Configuration

**Execution Steps:** {{STEP_COUNT}} steps
**Goal:** {{GOAL}}
{{LOG_MONITORING_SECTION}}

## Step 1: Check for Stop Request

Before doing any work, check if stop was requested:
\`\`\`powershell
$state = Get-Content "{{STATE_FILE}}" -Raw | ConvertFrom-Json
if ($state.stop_requested) { Write-Host "Stop requested. Exiting."; exit 0 }
\`\`\`

## Step 2: Update State to Running

\`\`\`powershell
$state = Get-Content "{{STATE_FILE}}" -Raw | ConvertFrom-Json
$state.status = "running"
$state.current_action = "Starting iteration {{ITERATION}}"
$state | ConvertTo-Json -Depth 10 | Set-Content "{{STATE_FILE}}"
\`\`\`

## Step 3: Execute Automation Steps

{{EXECUTION_STEPS}}

## Step 4: Analyze Results

### Check Logs for Errors

\`\`\`powershell
# Runner event logs
Get-Content "{{DEV_LOGS_ESCAPED}}\\runner-actions.jsonl" -Tail 50 | Select-String -Pattern '"error"|"failed"'
Get-Content "{{DEV_LOGS_ESCAPED}}\\runner-image-recognition.jsonl" -Tail 30 | Select-String -Pattern '"found":\\s*false'

# Application logs (if configured)
{{LOG_CHECK_COMMANDS}}
\`\`\`

### View Screenshots

Check for annotated screenshots from image recognition:
\`\`\`powershell
Get-ChildItem "{{DEV_LOGS_ESCAPED}}\\screenshots" -Filter "*.png" | Sort-Object LastWriteTime -Descending | Select-Object -First 5
\`\`\`

Use the Read tool to view the most recent screenshots.

## Step 5: Fix Issues

If any errors were found:
1. Identify the root cause from logs and screenshots
2. Read the relevant source code
3. Make the fix
4. Update state with what you fixed:

\`\`\`powershell
$state = Get-Content "{{STATE_FILE}}" -Raw | ConvertFrom-Json
$state.errors_fixed += @{
    file = "path/to/file.ts"
    line = 42
    description = "Fixed the issue"
    fixed_at = (Get-Date -Format "o")
}
$state | ConvertTo-Json -Depth 10 | Set-Content "{{STATE_FILE}}"
\`\`\`

5. Restart affected services as needed:

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

## Step 6: Decide Next Action

**Check for stop request again:**
\`\`\`powershell
$state = Get-Content "{{STATE_FILE}}" -Raw | ConvertFrom-Json
if ($state.stop_requested) {
    $state.status = "stopped"
    $state | ConvertTo-Json -Depth 10 | Set-Content "{{STATE_FILE}}"
    Write-Host "Stop requested. Exiting."
    exit 0
}
\`\`\`

**If all issues fixed and automation passes:** Update state to complete and exit:
\`\`\`powershell
$state = Get-Content "{{STATE_FILE}}" -Raw | ConvertFrom-Json
$state.status = "complete"
$state.completed_at = (Get-Date -Format "o")
$state | ConvertTo-Json -Depth 10 | Set-Content "{{STATE_FILE}}"
\`\`\`

**If more work needed:** Spawn fresh session and exit:
\`\`\`powershell
$state = Get-Content "{{STATE_FILE}}" -Raw | ConvertFrom-Json
$state.iteration = $state.iteration + 1
$state.status = "spawning_next"
$state | ConvertTo-Json -Depth 10 | Set-Content "{{STATE_FILE}}"

# Write continuation prompt
@"
Continue AI Developer session {{SESSION_ID}}.
Read state file first: Get-Content "{{STATE_FILE}}" | ConvertFrom-Json
Previous fixes: [list what you fixed]
Remaining: [what's left]
"@ | Set-Content "{{DEV_LOGS_ESCAPED}}\\ai-developer-continuation.txt"

# Spawn fresh Claude
python {{SPAWN_SCRIPT_ESCAPED}} --file "{{DEV_LOGS_ESCAPED}}\\ai-developer-continuation.txt"
\`\`\`

## Rules

- Execute steps IN THE EXACT ORDER specified
- ALWAYS check for stop request before and after main work
- ALWAYS update state file with progress
- ALWAYS analyze logs and screenshots after execution
- You are INDEPENDENT - you CAN restart any service including the runner
- STOP when iteration reaches {{MAX_ITERATIONS}}
- Work AUTONOMOUSLY - never ask the user

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

interface WorkspacePaths {
  workspace_root: string;
  dev_logs_path: string;
  scripts_path: string;
  spawn_script: string;
  workspace_root_escaped: string;
  dev_logs_path_escaped: string;
  spawn_script_escaped: string;
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

interface AiBuilderTabProps {
  /** Project logs hook from App.tsx (shared with Logs tab) */
  projectLogs: UseProjectLogsReturn;
}

export function AiBuilderTab({ projectLogs }: AiBuilderTabProps) {
  const execution = useExecution();

  // Session Mode - persisted to localStorage
  // Inline (false): Uses /trigger-ai-analysis, output in AI Output tab, ends if runner restarts
  // Persistent (true): Independent process, survives restarts, has state file tracking
  const [persistentSession, setPersistentSession] = useState<boolean>(() => {
    try {
      return localStorage.getItem("qontinui-ai-persistent-session") === "true";
    } catch {
      return false;
    }
  });

  // Persist session mode to localStorage
  useEffect(() => {
    localStorage.setItem("qontinui-ai-persistent-session", persistentSession ? "true" : "false");
  }, [persistentSession]);

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

  // Ordered execution steps
  const [executionSteps, setExecutionSteps] = useState<ExecutionStep[]>([]);
  const [goal, setGoal] = useState("");
  const [maxIterations, setMaxIterations] = useState(10);

  // Workspace paths (fetched from backend for portability)
  const [workspacePaths, setWorkspacePaths] = useState<WorkspacePaths | null>(null);

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

  // Claude output log (real-time visibility)
  const [claudeLog, setClaudeLog] = useState<string>("");
  const [claudeLogInfo, setClaudeLogInfo] = useState<{
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

  // Poll session state when there's an active session AND in persistent mode
  // This useEffect should only manage isRunning for persistent sessions
  useEffect(() => {
    if (!currentSessionId) return;
    // Only poll and manage isRunning state if we're in persistent session mode
    // This prevents interfering with standard mode button state
    if (!persistentSession) return;

    const pollState = async () => {
      try {
        const response = await invoke<{ success: boolean; data?: AiDeveloperState }>(
          "read_ai_developer_state",
          { sessionId: currentSessionId },
        );
        if (response.success && response.data) {
          setSessionState(response.data);

          // Update isRunning based on status AND recency
          // A session is only considered running if it has an active status
          // AND was started within the last 10 minutes (to handle stale state files)
          const activeStatuses = ["starting", "running", "spawning_next"];
          const isActiveStatus = activeStatuses.includes(response.data.status);

          let isRecent = false;
          if (response.data.started_at) {
            const startedAt = new Date(response.data.started_at).getTime();
            const tenMinutesAgo = Date.now() - 10 * 60 * 1000;
            isRecent = startedAt > tenMinutesAgo;
          }

          // Only consider running if both active status AND recent start
          setIsRunning(isActiveStatus && isRecent);

          // Clear session if it's complete/stopped (but keep showing final state)
          if (response.data.status === "complete" || response.data.status === "stopped") {
            // Don't clear immediately - let user see the final state
            // They can start a new session which will clear it
          }
        } else if (!response.success) {
          // Session file not found - clear the session
          console.log("Session file not found, clearing session");
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
  }, [currentSessionId, persistentSession]);

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
  const generateInputCaptureSection = () => {
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

  // Generate standard mode prompt (for inline execution via MCP API)
  // iteration parameter allows continuation prompts to pass current iteration
  const generateStandardPrompt = (iteration: number = 1) => {
    const goalStr = goal.trim() || "Verify automation works correctly";
    const devLogsEscaped = workspacePaths?.dev_logs_path_escaped || "{{DEV_LOGS_ESCAPED}}";
    const spawnScriptEscaped = workspacePaths?.spawn_script_escaped || "{{SPAWN_SCRIPT_ESCAPED}}";

    // Get workflow name for re-run (use first workflow step, or placeholder)
    const workflowStep = executionSteps.find((s) => s.type === "workflow");
    const workflowName = workflowStep?.name || "{{WORKFLOW_NAME}}";
    const monitorParam = ""; // Could be extracted from execution context if needed

    return STANDARD_PROMPT_TEMPLATE.replace("{{GOAL}}", goalStr)
      .replace(/\{\{ITERATION\}\}/g, String(iteration))
      .replace(/\{\{MAX_ITERATIONS\}\}/g, String(maxIterations))
      .replace("{{STEP_COUNT}}", String(executionSteps.length))
      .replace("{{EXECUTION_STEPS}}", generateExecutionInstructions())
      .replace(/\{\{DEV_LOGS_ESCAPED\}\}/g, devLogsEscaped)
      .replace(/\{\{SPAWN_SCRIPT_ESCAPED\}\}/g, spawnScriptEscaped)
      .replace("{{LOG_CHECK_COMMANDS}}", generateLogCheckCommands())
      .replace("{{INPUT_CAPTURE_SECTION}}", generateInputCaptureSection())
      .replace(/\{\{WORKFLOW_NAME\}\}/g, workflowName)
      .replace(/\{\{MONITOR_PARAM\}\}/g, monitorParam);
  };

  // Generate developer mode prompt (for spawn mechanism)
  const generateDeveloperPrompt = (sessionId: string) => {
    const goalStr = goal.trim() || "Verify automation works correctly";

    // Get paths - use placeholders if not loaded yet (for preview before paths are fetched)
    const devLogsEscaped = workspacePaths?.dev_logs_path_escaped || "{{DEV_LOGS_ESCAPED}}";
    const spawnScriptEscaped = workspacePaths?.spawn_script_escaped || "{{SPAWN_SCRIPT_ESCAPED}}";
    const workspaceEscaped = workspacePaths?.workspace_root_escaped || "{{WORKSPACE_ESCAPED}}";
    const devLogsPath = workspacePaths?.dev_logs_path || ".dev-logs";

    // State file path
    const stateFile = `${devLogsPath}\\\\ai-developer-${sessionId}.json`;

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

    return DEVELOPER_PROMPT_TEMPLATE.replace("{{SESSION_ID}}", sessionId)
      .replace(/\{\{SESSION_ID\}\}/g, sessionId)
      .replace("{{ITERATION}}", "1")
      .replace(/\{\{ITERATION\}\}/g, "1")
      .replace("{{MAX_ITERATIONS}}", String(maxIterations))
      .replace(/\{\{MAX_ITERATIONS\}\}/g, String(maxIterations))
      .replace(/\{\{STATE_FILE\}\}/g, stateFile)
      .replace("{{STEP_COUNT}}", String(executionSteps.length))
      .replace("{{EXECUTION_STEPS}}", generateExecutionInstructions())
      .replace("{{GOAL}}", goalStr)
      .replace("{{LOG_MONITORING_SECTION}}", logMonitoringSection)
      .replace("{{LOG_CHECK_COMMANDS}}", generateLogCheckCommands())
      .replace(/\{\{DEV_LOGS_ESCAPED\}\}/g, devLogsEscaped)
      .replace(/\{\{SPAWN_SCRIPT_ESCAPED\}\}/g, spawnScriptEscaped)
      .replace(/\{\{WORKSPACE_ESCAPED\}\}/g, workspaceEscaped);
  };

  // Generate prompt based on current mode (for preview)
  const generatePrompt = (sessionId: string) => {
    return persistentSession ? generateDeveloperPrompt(sessionId) : generateStandardPrompt();
  };

  // Copy prompt to clipboard (preview mode - uses placeholder session ID)
  const copyPrompt = async () => {
    const previewSessionId = "preview-" + Date.now().toString(36);
    const prompt = generatePrompt(previewSessionId);
    await navigator.clipboard.writeText(prompt);
    setLastResult({ success: true, message: "Prompt copied to clipboard!" });
    setTimeout(() => setLastResult(null), 3000);
  };

  // Run automation in standard mode (inline via MCP API, output goes to AI Output tab)
  const runStandardMode = async (sessionId: string, prompt: string) => {
    console.log("[AI_BUILDER] Running in STANDARD mode via MCP API...");

    const response = await fetch("http://localhost:9876/trigger-ai-analysis", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        prompt,
        display_prompt: `AI Builder: ${goal.trim() || "Automation analysis"}`,
        timeout_seconds: 600,
      }),
    });

    const result = await response.json();
    console.log("[AI_BUILDER] trigger-ai-analysis response:", result);

    if (result.success) {
      setLastResult({
        success: true,
        message: "AI analysis started! Check AI Output tab for results.",
      });
      setHistory((prev) => prev.map((h) => (h.id === sessionId ? { ...h, success: true } : h)));
      // Standard mode doesn't track session ID since it uses AI Output tab
      // Keep button disabled for longer to prevent rapid re-clicking while analysis runs
      // Analysis typically takes 30-60+ seconds, but we enable after 10s for responsiveness
      setTimeout(() => setIsRunning(false), 10000);
      // Note: Input capture is now automatically started/stopped with workflow execution
      // in Python when captureInputValidation is enabled
    } else {
      throw new Error(result.error || "Failed to trigger AI analysis");
    }
  };

  // Run automation in persistent mode (spawn independent Claude, survives runner restarts)
  const runPersistentMode = async (sessionId: string, prompt: string) => {
    console.log("[AI_BUILDER] Running in PERSISTENT mode via spawn mechanism...");

    const response = await invoke<{
      success: boolean;
      message?: string;
      data?: { session_id: string };
    }>("spawn_ai_developer", {
      prompt,
      sessionId,
      maxIterations: maxIterations,
    });
    console.log("[AI_BUILDER] spawn_ai_developer response:", response);

    if (response.success) {
      setCurrentSessionId(sessionId);
      setLastResult({
        success: true,
        message: `Persistent session ${sessionId} started! (survives runner restarts)`,
      });
      setHistory((prev) => prev.map((h) => (h.id === sessionId ? { ...h, success: true } : h)));
    } else {
      throw new Error(response.message || "Failed to start developer session");
    }
  };

  // Run the automation
  const runAutomation = async () => {
    console.log(
      "[AI_BUILDER] Starting AI session...",
      persistentSession ? "(Persistent)" : "(Inline)",
    );
    setIsRunning(true);
    setLastResult(null);

    try {
      // Generate unique session ID
      const sessionId = Date.now().toString(36) + Math.random().toString(36).substring(2, 7);
      console.log("[AI_BUILDER] Generated session ID:", sessionId);

      // Note: Input capture is now automatically started/stopped with workflow execution
      // in Python when captureInputValidation is enabled (synced via useEffect)

      // Generate appropriate prompt based on mode
      const prompt = persistentSession
        ? generateDeveloperPrompt(sessionId)
        : generateStandardPrompt();
      console.log("[AI_BUILDER] Generated prompt length:", prompt.length);

      // Add to history
      const historyEntry: PromptHistoryEntry = {
        id: sessionId,
        timestamp: Date.now(),
        steps: [...executionSteps],
        goal: goal.trim() || "Verify automation works correctly",
      };
      setHistory((prev) => [historyEntry, ...prev]);

      // Run in appropriate mode
      if (persistentSession) {
        await runPersistentMode(sessionId, prompt);
      } else {
        await runStandardMode(sessionId, prompt);
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
      const response = await invoke<{ success: boolean; message?: string }>("stop_ai_developer", {
        sessionId: currentSessionId,
      });

      if (response.success) {
        setLastResult({ success: true, message: "Stop requested. Session will exit gracefully." });
      } else {
        setLastResult({
          success: false,
          message: response.message || "Failed to stop session",
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
            Run automation sequences, capture screenshots, and let AI analyze/fix issues in a loop
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
                        {projectLogs.config.logSources.filter((s) => s.enabled).length} log
                        source(s) will be monitored
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
                    <span>No log sources configured. Configure in Logs → Project Logs.</span>
                  </div>
                )}
              </div>
            </div>

            {/* Advanced Settings - Collapsible */}
            <CollapsiblePanel
              title="Advanced"
              icon={<Code className="w-4 h-4" />}
              defaultCollapsed={!persistentSession}
              storageKey="ai-builder-advanced"
            >
              <div className="space-y-3">
                {/* Persistent Session Toggle */}
                <div className="flex items-center justify-between p-2 bg-muted/30 rounded-md">
                  <div className="flex items-center gap-2">
                    <Activity className="w-4 h-4 text-orange-500" />
                    <div>
                      <span className="text-sm font-medium">Persistent Session Mode</span>
                      <p className="text-xs text-muted-foreground">
                        {persistentSession
                          ? "Multi-iteration loop with fresh AI sessions per iteration"
                          : "Single iteration - AI runs once, output in AI Output tab"}
                      </p>
                    </div>
                  </div>
                  <button
                    onClick={() => setPersistentSession(!persistentSession)}
                    className={`flex items-center transition-colors ${persistentSession ? "text-orange-500" : "text-muted-foreground"}`}
                    title={persistentSession ? "Persistent Session enabled" : "Inline Session"}
                  >
                    {persistentSession ? (
                      <ToggleRight className="w-8 h-8" />
                    ) : (
                      <ToggleLeft className="w-8 h-8" />
                    )}
                  </button>
                </div>

                {/* When Persistent Session is enabled, show additional options */}
                {persistentSession && (
                  <div className="space-y-3 pl-2 border-l-2 border-orange-500/30">
                    <div className="space-y-1">
                      <p className="text-xs font-medium text-foreground">How it works:</p>
                      <ul className="text-xs text-muted-foreground space-y-0.5 list-disc list-inside">
                        <li>Each iteration spawns a <strong>new AI session</strong> (fresh context)</li>
                        <li>AI runs automation → analyzes logs/screenshots → fixes issues</li>
                        <li>If issues found: spawns next iteration automatically</li>
                        <li>Loop continues until all checks pass or max iterations reached</li>
                        <li>Sessions survive runner restarts (independent process)</li>
                      </ul>
                    </div>
                    <div className="flex items-center gap-4 flex-wrap">
                      <label className="flex items-center gap-2 text-sm">
                        <span className="text-muted-foreground">Max Iterations:</span>
                        <input
                          type="number"
                          min={1}
                          max={50}
                          value={maxIterations}
                          onChange={(e) =>
                            setMaxIterations(
                              Math.max(1, Math.min(50, parseInt(e.target.value) || 10)),
                            )
                          }
                          className="w-16 px-2 py-1 bg-background border border-border rounded text-sm"
                        />
                      </label>
                    </div>
                  </div>
                )}

                {/* When Persistent Session is disabled, show explanation */}
                {!persistentSession && (
                  <div className="space-y-1 pl-2 border-l-2 border-muted-foreground/30">
                    <p className="text-xs font-medium text-foreground">How it works:</p>
                    <ul className="text-xs text-muted-foreground space-y-0.5 list-disc list-inside">
                      <li>Single AI session runs one iteration</li>
                      <li>AI analyzes automation results and suggests/applies fixes</li>
                      <li>If fixes applied, AI will instruct you to re-run manually</li>
                      <li>Output appears in the "AI Output" tab</li>
                    </ul>
                  </div>
                )}

                {/* Input Capture for Coordinate Validation Toggle */}
                <div className="flex items-center justify-between p-2 bg-muted/30 rounded-md">
                  <div className="flex items-center gap-2">
                    <MousePointer2 className="w-4 h-4 text-purple-500" />
                    <div>
                      <span className="text-sm font-medium">Capture Input for Validation</span>
                      <p className="text-xs text-muted-foreground">
                        {captureInputValidation
                          ? "Records actual mouse/keyboard to compare with reported positions"
                          : "Disabled - only reported positions are logged"}
                      </p>
                    </div>
                  </div>
                  <button
                    onClick={() => setCaptureInputValidation(!captureInputValidation)}
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

            {/* Actions */}
            <div className="flex gap-3">
              {isRunning && persistentSession ? (
                // Persistent Session: Stop button for spawn sessions
                <button
                  onClick={stopSession}
                  className="flex-1 flex items-center justify-center gap-2 px-4 py-3 bg-red-500 text-white rounded-md font-medium hover:bg-red-600 transition-colors"
                >
                  <Square className="w-4 h-4" />
                  Stop Session
                </button>
              ) : isRunning && !persistentSession ? (
                // Standard Mode: Running indicator (uses AI Output tab)
                <button
                  disabled
                  className="flex-1 flex items-center justify-center gap-2 px-4 py-3 bg-primary/50 text-primary-foreground rounded-md font-medium cursor-not-allowed"
                >
                  <Loader2 className="w-4 h-4 animate-spin" />
                  Analysis Running...
                </button>
              ) : persistentSession &&
                sessionState &&
                (sessionState.status === "complete" || sessionState.status === "stopped") ? (
                // Persistent Session: New session button
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
                // Start button - different labels for each mode
                <button
                  onClick={runAutomation}
                  disabled={isRunning}
                  className={`flex-1 flex items-center justify-center gap-2 px-4 py-3 rounded-md font-medium disabled:opacity-50 disabled:cursor-not-allowed transition-colors ${
                    persistentSession
                      ? "bg-orange-500 text-white hover:bg-orange-600"
                      : "bg-primary text-primary-foreground hover:bg-primary/90"
                  }`}
                >
                  <Play className="w-4 h-4" />
                  {persistentSession ? "Start Persistent Session" : "Start AI Analysis"}
                </button>
              )}

              <button
                onClick={copyPrompt}
                className="flex items-center gap-2 px-4 py-3 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 transition-colors"
                title="Copy prompt to clipboard"
              >
                <Copy className="w-4 h-4" />
              </button>
            </div>

            {/* Session Status - Persistent Session only */}
            {persistentSession && sessionState && (
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
            {/* Standard Mode: Direct user to AI Output tab */}
            {!persistentSession && isRunning && (
              <div className="card p-4 space-y-3 border-primary/50">
                <div className="flex items-center gap-2">
                  <Loader2 className="w-4 h-4 text-primary animate-spin" />
                  <span className="font-medium">Analysis in Progress</span>
                </div>
                <p className="text-sm text-muted-foreground">
                  View live output in the <strong>Logs → AI Output</strong> tab.
                </p>
                <p className="text-xs text-muted-foreground">
                  The AI is executing your automation steps and analyzing results.
                </p>
              </div>
            )}

            {/* Claude Output - Persistent Session only (shown when spawn session is active) */}
            {persistentSession && currentSessionId && (
              <div className="card p-4 space-y-3">
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-2">
                    <Activity className="w-4 h-4 text-primary animate-pulse" />
                    <span className="font-medium">Claude Output</span>
                    {isRunning && (
                      <span className="text-xs bg-green-500/20 text-green-500 px-2 py-0.5 rounded-full flex items-center gap-1">
                        <Loader2 className="w-3 h-3 animate-spin" />
                        Running
                      </span>
                    )}
                  </div>
                  {claudeLogInfo && (
                    <span className="text-xs text-muted-foreground">
                      {claudeLogInfo.totalLines} lines
                    </span>
                  )}
                </div>

                {claudeLog ? (
                  <pre className="text-xs bg-background p-3 rounded-md overflow-auto max-h-80 whitespace-pre-wrap font-mono">
                    {claudeLog}
                  </pre>
                ) : (
                  <div className="text-center py-6 text-muted-foreground">
                    <Loader2 className="w-6 h-6 mx-auto mb-2 animate-spin" />
                    <p className="text-sm">Waiting for Claude output...</p>
                    <p className="text-xs mt-1">Claude is initializing in the background</p>
                  </div>
                )}
              </div>
            )}

            {/* Prompt Preview */}
            <CollapsiblePanel
              title="Prompt Preview"
              icon={<Sparkles className="w-4 h-4" />}
              defaultCollapsed={currentSessionId !== null}
              storageKey="ai-builder-preview"
            >
              <pre className="text-xs bg-background p-3 rounded-md overflow-auto max-h-64 whitespace-pre-wrap">
                {generatePrompt("preview-session")}
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
