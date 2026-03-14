// FILE: src/lib/ui-bridge/intents.ts
/**
 * App-specific intent definitions for the UI Bridge.
 *
 * Intents represent composite user actions that span multiple steps.
 * They are registered with the backend via POST /ai/intents/register
 * at startup (not as React hooks).
 */

export interface IntentParam {
  type: string;
  required?: boolean;
  description?: string;
  default?: unknown;
}

export interface Intent {
  id: string;
  name: string;
  description: string;
  tags?: string[];
  params?: Record<string, IntentParam>;
  handler?: string;
}

export const APP_INTENTS: Intent[] = [
  // -------------------------------------------------------------------------
  // Workflow execution
  // -------------------------------------------------------------------------
  {
    id: 'run-workflow',
    name: 'Run Workflow',
    description:
      'Navigate to Execute tab, select a workflow by name, choose run mode (GUI or headless), and start execution. Optionally force GUI lock acquisition.',
    tags: ['execution', 'workflow', 'run'],
    params: {
      workflowName: { type: 'string', required: true, description: 'Name of the workflow to execute' },
      mode: { type: 'string', required: false, description: '"gui" or "headless"', default: 'headless' },
      forceGuiLock: { type: 'boolean', required: false, description: 'Force GUI lock acquisition', default: false },
    },
  },
  {
    id: 'rerun-last-workflow',
    name: 'Re-run Last Workflow',
    description:
      'Re-execute the most recently completed workflow using its saved configuration. Navigates to active dashboard after starting.',
    tags: ['execution', 'workflow', 'rerun'],
  },

  // -------------------------------------------------------------------------
  // Workflow building
  // -------------------------------------------------------------------------
  {
    id: 'create-workflow',
    name: 'Create New Workflow',
    description:
      'Navigate to the Workflow Builder tab to create a new automation workflow. Optionally provide a natural-language spec for AI generation.',
    tags: ['build', 'workflow', 'create'],
    params: {
      spec: { type: 'string', required: false, description: 'Natural-language workflow specification for AI generation' },
    },
  },
  {
    id: 'edit-workflow',
    name: 'Edit Existing Workflow',
    description:
      'Open a saved workflow in the Workflow Builder for editing. Navigate to Build > Workflows and load the workflow by name.',
    tags: ['build', 'workflow', 'edit'],
    params: {
      workflowName: { type: 'string', required: true, description: 'Name of the workflow to edit' },
    },
  },

  // -------------------------------------------------------------------------
  // Run inspection
  // -------------------------------------------------------------------------
  {
    id: 'inspect-run',
    name: 'Inspect Run Results',
    description:
      'Select a specific run from History, then navigate through its sub-tabs (Recap, Findings, Test Results, Traces, AI Output, Statistics) to inspect results.',
    tags: ['observe', 'run', 'inspect'],
    params: {
      runId: { type: 'string', required: false, description: 'Task run ID to select. If omitted, uses the most recent run.' },
      tab: {
        type: 'string',
        required: false,
        description: 'Sub-tab to open: "run-recap", "run-findings", "run-tests", "run-traces", "run-ai-output", "run-statistics"',
        default: 'run-recap',
      },
    },
  },
  {
    id: 'view-run-traces',
    name: 'View Run Traces',
    description:
      'Navigate to the Traces sub-tab for a selected run to inspect the execution trace waterfall, span details, and performance insights.',
    tags: ['observe', 'traces', 'performance'],
    params: {
      runId: { type: 'string', required: false, description: 'Task run ID. If omitted, uses the currently selected run.' },
    },
  },

  // -------------------------------------------------------------------------
  // Terminal management
  // -------------------------------------------------------------------------
  {
    id: 'open-terminal',
    name: 'Open Terminal Session',
    description:
      'Navigate to the Terminal tab and create a new terminal session. Optionally set a specific zone layout.',
    tags: ['terminal', 'session'],
    params: {
      layout: { type: 'number', required: false, description: 'Zone layout preset (1-8)', default: 1 },
    },
  },
  {
    id: 'approve-all-sessions',
    name: 'Approve All Waiting Sessions',
    description:
      'In the Terminal tab, approve all Claude Code sessions that are waiting for user input. Equivalent to Ctrl+Shift+Enter.',
    tags: ['terminal', 'approval', 'batch'],
  },
  {
    id: 'broadcast-command',
    name: 'Broadcast Command to All Sessions',
    description:
      'In the Terminal tab, send a shell command to all active terminal sessions simultaneously via the batch operations bar.',
    tags: ['terminal', 'batch', 'command'],
    params: {
      command: { type: 'string', required: true, description: 'Shell command to broadcast' },
    },
  },
  {
    id: 'switch-terminal-layout',
    name: 'Switch Terminal Zone Layout',
    description:
      'Change the terminal zone layout preset. Supports 1 (single) through 8 (eight zones) presets, accessible via Ctrl+Shift+1-8.',
    tags: ['terminal', 'layout', 'zones'],
    params: {
      preset: { type: 'number', required: true, description: 'Layout preset number (1-8)' },
    },
  },

  // -------------------------------------------------------------------------
  // Configuration
  // -------------------------------------------------------------------------
  {
    id: 'configure-ai-provider',
    name: 'Configure AI Provider',
    description:
      'Navigate to Settings > AI Providers to configure API keys and model selection for AI-powered features.',
    tags: ['settings', 'ai', 'configuration'],
  },
  {
    id: 'configure-log-sources',
    name: 'Configure Log Sources',
    description:
      'Navigate to Settings > Log Sources to add, remove, or modify global log source configurations for error monitoring.',
    tags: ['settings', 'logs', 'configuration'],
  },

  // -------------------------------------------------------------------------
  // Error monitoring
  // -------------------------------------------------------------------------
  {
    id: 'investigate-errors',
    name: 'Investigate Application Errors',
    description:
      'Navigate to the Error Monitor tab to review recent application errors from configured log sources. Errors can be auto-fixed or used to generate workflows.',
    tags: ['errors', 'monitoring', 'debug'],
  },

  // -------------------------------------------------------------------------
  // Finding management
  // -------------------------------------------------------------------------
  {
    id: 'manage-finding-categories',
    name: 'Manage Finding Categories',
    description:
      'Navigate to Configure > Finding Categories to create, reorder, show/hide, or reset finding category definitions used for workflow result classification.',
    tags: ['configure', 'findings', 'categories'],
  },

  // -------------------------------------------------------------------------
  // UI Bridge integration
  // -------------------------------------------------------------------------
  {
    id: 'discover-ui-elements',
    name: 'Discover UI Elements',
    description:
      'Navigate to Configure > UI Bridge to connect to an application instance and discover its interactive UI elements, states, and action targets for automation.',
    tags: ['configure', 'ui-bridge', 'discovery'],
    params: {
      appUrl: { type: 'string', required: false, description: 'URL of the application to connect to for element discovery' },
    },
  },

  // -------------------------------------------------------------------------
  // Specs and AI chat
  // -------------------------------------------------------------------------
  {
    id: 'create-spec',
    name: 'Create or Edit Spec',
    description:
      'Navigate to the Specs tab to view, create, or edit workflow specifications. Includes an AI chat panel for iterating on specs via natural language.',
    tags: ['build', 'specs', 'ai'],
    params: {
      specName: { type: 'string', required: false, description: 'Name of the spec to open. If omitted, shows the spec list.' },
    },
  },

  // -------------------------------------------------------------------------
  // Setup
  // -------------------------------------------------------------------------
  {
    id: 'initial-setup',
    name: 'Run Initial Setup',
    description:
      'Walk through the setup wizard: Welcome, Projects, Processes, Dev Services, AI Provider, and Claude Sessions configuration.',
    tags: ['setup', 'onboarding', 'wizard'],
  },
];
