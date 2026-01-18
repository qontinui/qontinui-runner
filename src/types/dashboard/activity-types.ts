/**
 * Dashboard Activity Types
 *
 * Type definitions for activity tracking in the Active Dashboard.
 * Activities represent the different types of work that can occur during task execution.
 */

/**
 * All supported activity types in the dashboard.
 *
 * - gui_automation: Setup/navigation via GUI automation (screenshot, actions, image recognition)
 * - playwright: Setup/execution via Playwright scripts
 * - ai_conversation: AI analysis, thinking, and code changes
 * - verification: Tests verifying if the fix worked (uses repo tests, Playwright, or GUI)
 * - findings: AI-detected issues (bugs, security, performance)
 * - execution_status: Real-time status of agentic features (routing, retry, compression, hooks)
 * - shell_command: Shell/terminal command execution with output
 * - api_request: HTTP API request/response details
 * - script: Script execution (Python, JS, etc.) with output
 * - workflow_ref: Referenced sub-workflow execution status
 */
export type ActivityType =
  | "gui_automation"
  | "playwright"
  | "ai_conversation"
  | "verification"
  | "findings"
  | "execution_status"
  | "shell_command"
  | "api_request"
  | "script"
  | "workflow_ref";

/**
 * Status of an individual activity.
 */
export type ActivityStatus =
  | "idle" // Not active, no data
  | "loading" // Fetching initial data
  | "running" // Currently executing
  | "paused" // Paused mid-execution
  | "completed" // Finished successfully
  | "failed" // Finished with error
  | "stopped"; // User stopped

/**
 * Phase in the task iteration cycle.
 *
 * Tasks follow this order: setup -> verification -> ai_work
 * Not all tasks use all phases (e.g., AI-only tasks skip setup/verification).
 */
export type TaskPhase = "setup" | "verification" | "ai_work" | "idle";

/**
 * State of an individual activity for tracking in the dashboard.
 */
export interface ActivityState {
  /** The type of activity */
  type: ActivityType;
  /** Current status */
  status: ActivityStatus;
  /** When the activity started (Unix timestamp ms) */
  startedAt?: number;
  /** When the activity completed (Unix timestamp ms) */
  completedAt?: number;
  /** Error message if failed */
  error?: string;
  /** Number of items (actions, tests, findings, messages, etc.) */
  itemCount: number;
  /** Number of completed items for progress tracking */
  completedCount?: number;
  /** Priority for display order (lower = higher priority) */
  priority: number;
}

/**
 * Summary statistics for an activity.
 */
export interface ActivityStats {
  /** Total items processed */
  total: number;
  /** Successful items */
  success: number;
  /** Failed items */
  failed: number;
  /** Skipped items */
  skipped?: number;
  /** Success rate percentage (0-100) */
  successRate: number;
  /** Average duration per item in ms */
  avgDurationMs?: number;
}

/**
 * Display configuration for an activity type.
 */
export interface ActivityDisplayConfig {
  /** Display name shown in UI */
  displayName: string;
  /** Icon name from lucide-react */
  icon: string;
  /** Accent color for borders/highlights (Tailwind color name) */
  accentColor: string;
  /** Route path for detail page */
  detailRoute: string;
}

/**
 * Default display configurations for each activity type.
 */
export const ACTIVITY_DISPLAY_CONFIG: Record<ActivityType, ActivityDisplayConfig> = {
  gui_automation: {
    displayName: "GUI Automation",
    icon: "Monitor",
    accentColor: "blue",
    detailRoute: "/logs/actions",
  },
  playwright: {
    displayName: "Playwright",
    icon: "Globe",
    accentColor: "purple",
    detailRoute: "/logs/playwright",
  },
  ai_conversation: {
    displayName: "AI Conversation",
    icon: "Bot",
    accentColor: "green",
    detailRoute: "/ai-output",
  },
  verification: {
    displayName: "Verification",
    icon: "CheckCircle",
    accentColor: "teal",
    detailRoute: "/verification",
  },
  findings: {
    displayName: "Findings",
    icon: "AlertTriangle",
    accentColor: "amber",
    detailRoute: "/findings",
  },
  execution_status: {
    displayName: "Execution Status",
    icon: "Activity",
    accentColor: "cyan",
    detailRoute: "/settings/agentic",
  },
  shell_command: {
    displayName: "Shell Command",
    icon: "Terminal",
    accentColor: "slate",
    detailRoute: "/logs/shell",
  },
  api_request: {
    displayName: "API Request",
    icon: "Globe2",
    accentColor: "orange",
    detailRoute: "/logs/api",
  },
  script: {
    displayName: "Script",
    icon: "FileCode",
    accentColor: "indigo",
    detailRoute: "/logs/scripts",
  },
  workflow_ref: {
    displayName: "Sub-Workflow",
    icon: "GitBranch",
    accentColor: "pink",
    detailRoute: "/logs/workflows",
  },
};

/**
 * Default priorities for activity types (lower = shown first).
 */
export const ACTIVITY_PRIORITIES: Record<ActivityType, number> = {
  gui_automation: 10,
  playwright: 15,
  shell_command: 18,
  script: 19,
  ai_conversation: 20,
  api_request: 22,
  workflow_ref: 23,
  verification: 25,
  findings: 30,
  execution_status: 35,
};

/**
 * Phase display configurations.
 */
export const PHASE_DISPLAY_CONFIG: Record<TaskPhase, { label: string; color: string }> = {
  setup: { label: "SETUP", color: "blue" },
  verification: { label: "VERIFY", color: "purple" },
  ai_work: { label: "AI", color: "green" },
  idle: { label: "", color: "gray" },
};
