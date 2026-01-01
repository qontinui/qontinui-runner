/**
 * constants.ts
 *
 * Constants for the AI Builder components.
 */

// Storage keys
export const CUSTOM_PROMPT_TEMPLATE_KEY = "qontinui-custom-developer-prompt-template";
export const EXECUTION_STEPS_KEY = "qontinui-ai-execution-steps";
export const GOAL_KEY = "qontinui-ai-goal";
export const MAX_ITERATIONS_KEY = "qontinui-ai-max-iterations";
export const CAPTURE_INPUT_VALIDATION_KEY = "qontinui-ai-capture-input-validation";
export const HISTORY_KEY = "qontinui-ai-builder-history";
export const SESSION_KEY = "qontinui-ai-developer-session";

// Default Developer Mode Prompt Template - Runner handles continuation deterministically
export const DEFAULT_DEVELOPER_PROMPT_TEMPLATE = `# AI Developer Loop

Execute automation steps, analyze results, fix issues. The runner handles session continuation.

## Runner-Managed Continuation

**You don't need to spawn new sessions or manage state files.** The runner will:
1. Check the checkpoint file after your session ends
2. If \`completed: false\` -> automatically spawn next session
3. If \`completed: true\` -> stop (goal achieved)

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
- \`completed: true\` -> Runner stops, workflow done
- \`completed: false\` -> Runner spawns next session automatically
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

/**
 * Get the current developer prompt template (custom or default)
 */
export const getDeveloperPromptTemplate = (): string => {
  if (typeof window !== "undefined") {
    const customTemplate = localStorage.getItem(CUSTOM_PROMPT_TEMPLATE_KEY);
    if (customTemplate) {
      return customTemplate;
    }
  }
  return DEFAULT_DEVELOPER_PROMPT_TEMPLATE;
};

/**
 * Save a custom prompt template
 */
export const saveCustomPromptTemplate = (template: string): void => {
  if (typeof window !== "undefined") {
    localStorage.setItem(CUSTOM_PROMPT_TEMPLATE_KEY, template);
  }
};

/**
 * Reset to default prompt template
 */
export const resetPromptTemplateToDefault = (): void => {
  if (typeof window !== "undefined") {
    localStorage.removeItem(CUSTOM_PROMPT_TEMPLATE_KEY);
  }
};

/**
 * Check if using custom template
 */
export const isUsingCustomTemplate = (): boolean => {
  if (typeof window !== "undefined") {
    return localStorage.getItem(CUSTOM_PROMPT_TEMPLATE_KEY) !== null;
  }
  return false;
};
