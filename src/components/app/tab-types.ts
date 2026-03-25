export type LogSubTab = "general" | "image" | "actions";

export type MainTabId =
  | "gui-automation"
  | "workflow-queue"
  | "active"
  | "runs"
  | "history"
  | "error-monitor"
  | "processes"
  | "reflection"
  | "architecture"
  | "generator-eval"
  | "autoresearch"
  | "meta-optimizer"
  | "run-recap"
  | "run-actions"
  | "run-image"
  | "run-findings"
  | "run-state-explorer"
  | "run-tests"
  | "run-ai-output"
  | "run-statistics"
  | "run-ai-data"
  | "run-traces"
  | "ai"
  | "logs"
  | "run-summary"
  | "monitor-summary"
  | "monitor-findings"
  | "monitor-issues"
  | "monitor-learnings"
  | "monitor-state-explorer"
  | "monitor-statistics"
  | "monitor-discoveries"
  | "library"
  | "step-builders"
  | "check-builder"
  | "check-group-builder"
  | "shell-command-builder"
  | "task-builder"
  | "context-builder"
  | "playwright-test-builder"
  | "unified-workflow-builder"
  | "state-machine"
  | "specs"
  | "capture"
  | "config-log-sources"
  | "config-findings"
  | "config-hooks"
  | "config-ui-bridge"
  | "triggers"
  | "tasks"
  | "settings"
  | "settings-account"
  | "settings-ai"
  | "settings-agentic"
  | "settings-self-healing"
  | "settings-playwright"
  | "settings-mobile"
  | "settings-cloud-relay"
  | "settings-mcp"
  | "settings-log-sources"
  | "settings-execution-variables"
  | "settings-general"
  | "settings-storage"
  | "settings-backup"
  | "settings-instances"
  | "settings-debug"
  | "settings-updates"
  | "orchestration-loop"
  | "image-quality-tests"
  | "terminal"
  | "llm-analytics"
  | "help";

const VALID_TAB_IDS: MainTabId[] = [
  "gui-automation",
  "workflow-queue",
  "active",
  "runs",
  "history",
  "error-monitor",
  "processes",
  "reflection",
  "architecture",
  "generator-eval",
  "autoresearch",
  "meta-optimizer",
  "run-recap",
  "run-actions",
  "run-image",
  "run-findings",
  "run-state-explorer",
  "run-tests",
  "run-ai-output",
  "run-statistics",
  "run-ai-data",
  "run-traces",
  "ai",
  "logs",
  "run-summary",
  "monitor-summary",
  "monitor-findings",
  "monitor-issues",
  "monitor-learnings",
  "monitor-state-explorer",
  "monitor-statistics",
  "monitor-discoveries",
  "library",
  "step-builders",
  "check-builder",
  "check-group-builder",
  "shell-command-builder",
  "task-builder",
  "context-builder",
  "playwright-test-builder",
  "unified-workflow-builder",
  "state-machine",
  "specs",
  "capture",
  "config-log-sources",
  "config-findings",
  "config-hooks",
  "config-ui-bridge",
  "triggers",
  "tasks",
  "settings",
  "settings-account",
  "settings-ai",
  "settings-agentic",
  "settings-self-healing",
  "settings-playwright",
  "settings-mobile",
  "settings-cloud-relay",
  "settings-mcp",
  "settings-log-sources",
  "settings-execution-variables",
  "settings-general",
  "settings-storage",
  "settings-backup",
  "settings-instances",
  "settings-debug",
  "settings-updates",
  "orchestration-loop",
  "image-quality-tests",
  "terminal",
  "llm-analytics",
  "help",
];

export const SIDEBAR_COLLAPSED_KEY = "qontinui-sidebar-collapsed";

export function migrateTabId(stored: string | null): MainTabId {
  if (!stored) return "gui-automation";

  const migrations: Record<string, MainTabId> = {
    run: "gui-automation",
    history: "runs",
    "ai-workflows": "run-ai-output",
    "ai-builder": "unified-workflow-builder",
    builder: "unified-workflow-builder",
    prompts: "library",
    scripts: "library",
    "script-builder": "library",
    contexts: "library",
    scheduler: "tasks",
    dataset: "capture",
    extract: "capture",
    "live-page-generator": "unified-workflow-builder",
    "spec-discovery": "unified-workflow-builder",
    "page-sweep": "unified-workflow-builder",
    "run-plan": "terminal",
    logs: "run-recap",
    "run-dashboard": "run-recap",
    ai: "run-ai-output",
    "run-summary": "run-recap",
    "monitor-summary": "run-recap",
    "monitor-findings": "run-findings",
    "monitor-issues": "run-findings",
    "monitor-learnings": "run-recap",
    "monitor-verification": "run-state-explorer",
    "monitor-state-explorer": "run-state-explorer",
    "monitor-statistics": "run-statistics",
    "monitor-discoveries": "run-recap",
    monitor: "run-recap",
    issues: "run-findings",
    "run-issues": "run-findings",
    learnings: "run-recap",
    verification: "run-state-explorer",
    "run-verification": "run-state-explorer",
    "run-exploration": "run-state-explorer",
    "verification-builder": "library",
    statistics: "run-statistics",
    "check-builder": "step-builders",
    "check-group-builder": "step-builders",
    "shell-command-builder": "step-builders",
    "task-builder": "step-builders",
    "context-builder": "step-builders",
    "playwright-test-builder": "step-builders",
    "log-sources": "config-log-sources",
    "log-locations": "config-log-sources",
  };

  if (stored in migrations) {
    return migrations[stored];
  }

  if (VALID_TAB_IDS.includes(stored as MainTabId)) {
    return stored as MainTabId;
  }

  return "gui-automation";
}
