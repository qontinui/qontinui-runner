// FILE: src/lib/ui-bridge/intents.ts
import type { Intent } from "@qontinui/ui-bridge";

/**
 * Application-level intents — named composite user actions that AI agents
 * can discover and orchestrate. Registered at startup via the UI Bridge
 * control API (POST /ai/intents/register).
 */
export const APP_INTENTS: Intent[] = [
  {
    id: "execute-workflow",
    name: "Execute Workflow",
    description:
      "Run a workflow end-to-end: navigate to the Execute tab (gui-automation), " +
      "select a workflow from the dropdown, optionally configure monitors and initial " +
      "states, then click Start. The app navigates to the Active Dashboard to monitor " +
      "execution in real-time. On completion it auto-navigates to Run Recap.",
    tags: ["workflow", "execution", "run", "automation"],
    params: {
      workflowName: {
        type: "string",
        required: true,
        description: "Name of the saved workflow to execute",
      },
      navigateToActive: {
        type: "boolean",
        required: false,
        default: true,
        description: "Whether to auto-navigate to the Active Dashboard after starting",
      },
    },
  },
  {
    id: "rerun-last-workflow",
    name: "Re-run Last Workflow",
    description:
      "Re-execute the most recently run workflow. Looks up the workflow by name " +
      "in the database, or falls back to the cached inline workflow definition. " +
      "Triggered via the 'Run Last Workflow' button on the Active Dashboard.",
    tags: ["workflow", "execution", "rerun", "quick-action"],
    params: {},
  },
  {
    id: "build-workflow",
    name: "Build New Workflow",
    description:
      "Create a new unified workflow: navigate to the Workflow Builder tab, " +
      "add setup, verification, agentic, and completion steps using the step " +
      "configuration panel. Optionally use AI generation (AiGenerateWorkflowModal) " +
      "to scaffold from a natural language prompt. Save the workflow via the save dialog.",
    tags: ["workflow", "builder", "create", "build"],
    params: {
      workflowName: {
        type: "string",
        required: false,
        description: "Name for the new workflow",
      },
      prompt: {
        type: "string",
        required: false,
        description: "Natural language prompt for AI workflow generation",
      },
    },
  },
  {
    id: "generate-workflow-from-prompt",
    name: "Generate Workflow from Prompt",
    description:
      "Open the Workflow Builder, click the AI Generate button to open " +
      "AiGenerateWorkflowModal, enter a natural language description of the " +
      "desired automation, configure generation settings (model, verification type), " +
      "and generate a complete workflow definition automatically.",
    tags: ["workflow", "ai", "generation", "prompt"],
    params: {
      prompt: {
        type: "string",
        required: true,
        description: "Natural language description of the workflow to generate",
      },
    },
  },
  {
    id: "view-run-results",
    name: "View Run Results",
    description:
      "Navigate to the Observe section to review a completed workflow run. " +
      "Start at Run Recap for a summary, then drill into specific tabs: " +
      "Actions (execution log), Image Recognition (visual matches), " +
      "Findings (AI-detected issues), AI Output (full conversation), " +
      "Statistics (performance metrics), or Traces (execution traces).",
    tags: ["observe", "results", "recap", "review"],
    params: {
      taskRunId: {
        type: "number",
        required: false,
        description: "Specific task run ID to view. If omitted, shows the most recent run.",
      },
    },
  },
  {
    id: "configure-log-sources",
    name: "Configure Log Sources",
    description:
      "Set up external log file monitoring: navigate to Settings > Log Sources " +
      "to add, enable/disable, or remove log source entries. Each source specifies " +
      "a file path, category, color, and tail line count. Sources are monitored " +
      "during workflow execution for error detection.",
    tags: ["configure", "logs", "monitoring", "settings"],
    params: {
      sourcePath: {
        type: "string",
        required: false,
        description: "File path of the log source to add",
      },
      category: {
        type: "string",
        required: false,
        description: "Category for the log source (e.g., 'application', 'server')",
      },
    },
  },
  {
    id: "create-verification-check",
    name: "Create Verification Check",
    description:
      "Create a new verification check in the Library: navigate to the Library " +
      "or Step Builders > Checks page, click Create, configure the check type " +
      "(visual, text, DOM, API, etc.), set assertion conditions, and save. " +
      "Checks can then be added to workflow verification steps.",
    tags: ["check", "verification", "build", "library"],
    params: {
      checkName: {
        type: "string",
        required: false,
        description: "Name for the new check",
      },
      checkType: {
        type: "string",
        required: false,
        description: "Type of check (e.g., 'visual', 'text', 'dom', 'api')",
      },
    },
  },
  {
    id: "queue-workflows",
    name: "Queue Multiple Workflows",
    description:
      "Navigate to the Workflow Queue tab to set up a sequence of workflows " +
      "to run one after another. Add workflows from the saved library, " +
      "reorder them, and start the queue. Each workflow runs to completion " +
      "before the next begins.",
    tags: ["workflow", "queue", "batch", "sequence"],
    params: {
      workflowNames: {
        type: "string",
        required: false,
        description: "Comma-separated list of workflow names to queue",
      },
    },
  },
  {
    id: "approve-workflow-gate",
    name: "Approve Workflow Gate",
    description:
      "When a running workflow pauses at an approval gate, the Active Dashboard " +
      "shows an ApprovalDialog. Review the gate context, optionally add a comment, " +
      "and click Approve (or press Enter) to continue execution. " +
      "Press Escape to dismiss the dialog temporarily (shows floating badge).",
    tags: ["approval", "gate", "execution", "active-dashboard"],
    params: {
      comment: {
        type: "string",
        required: false,
        description: "Optional comment to include with the approval",
      },
    },
  },
  {
    id: "generate-ui-bridge-hooks",
    name: "Generate UI Bridge Hooks",
    description:
      "Navigate to Configure > UI Bridge, open the Hook Generation panel, " +
      "select which hook categories to generate (route awareness, state machine, " +
      "components, keyboard shortcuts, annotations, intents), optionally enable " +
      "architecture spec generation, then click Generate. The AI analyzes the " +
      "target app's source code and produces hook files. Preview the generated " +
      "files and click Apply to write them to the project.",
    tags: ["ui-bridge", "hooks", "generation", "ai", "configure"],
    params: {
      categories: {
        type: "string",
        required: false,
        description:
          "Comma-separated hook categories to generate (e.g., 'route-awareness,state-machine,keyboard-shortcuts')",
      },
      includeArchitectureSpec: {
        type: "boolean",
        required: false,
        default: false,
        description: "Whether to also generate an architecture specification JSON",
      },
    },
  },
  {
    id: "inspect-errors",
    name: "Inspect Error Monitor",
    description:
      "Navigate to the Error Monitor tab in the Observe section to review " +
      "application errors detected from configured log sources. Errors are " +
      "grouped by pattern and source, with severity levels, timestamps, and " +
      "occurrence counts. Use this to diagnose issues found during or after " +
      "workflow execution.",
    tags: ["errors", "monitor", "observe", "debugging", "logs"],
    params: {
      severity: {
        type: "string",
        required: false,
        description: "Filter by severity level (e.g., 'error', 'warning', 'critical')",
      },
    },
  },
  {
    id: "manage-processes",
    name: "Manage Spawned Processes",
    description:
      "Navigate to the Process Manager tab to view, monitor, and control " +
      "child processes spawned by the runner. Shows process status, resource " +
      "usage, and provides controls to stop or restart processes. Useful for " +
      "managing long-running automation tasks and debugging process issues.",
    tags: ["processes", "system", "management", "monitor"],
    params: {},
  },
];
