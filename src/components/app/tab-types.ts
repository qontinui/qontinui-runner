export type LogSubTab = "general" | "image" | "actions";

export type MainTabId =
  | "prompt-home"
  | "gui-automation"
  | "workflow-queue"
  | "active"
  | "runs"
  | "history"
  | "error-monitor"
  | "processes"
  | "reflection"
  | "observations"
  | "architecture"
  | "generator-eval"
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
  | "settings-world-state-verifier"
  | "settings-playwright"
  | "settings-mobile"
  | "settings-cloud-relay"
  | "settings-discovery"
  | "settings-web-integration"
  | "settings-mcp"
  | "settings-log-sources"
  | "settings-execution-variables"
  | "settings-general"
  | "settings-storage"
  | "settings-backup"
  | "settings-instances"
  | "settings-debug"
  | "settings-security"
  | "accessibility-explorer"
  | "settings-updates"
  | "orchestration-loop"
  | "image-quality-tests"
  | "terminal"
  | "llm-analytics"
  | "cost-control"
  | "evaluation"
  | "skills"
  | "help"
  | "automation-health"
  | "activity-timeline"
  | "watchers"
  | "knowledge-explorer"
  | "event-history"
  | "development-intelligence"
  | "demo-video"
  | "product-tours"
  | "session-recap"
  | "api-surface"
  | "decision-trail"
  | "memory-search"
  | "online-learning"
  | "dag-workflow-editor"
  | "project-explainer";

const VALID_TAB_IDS: MainTabId[] = [
  "prompt-home",
  "gui-automation",
  "workflow-queue",
  "active",
  "runs",
  "history",
  "error-monitor",
  "processes",
  "reflection",
  "observations",
  "architecture",
  "generator-eval",
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
  "settings-world-state-verifier",
  "settings-playwright",
  "settings-mobile",
  "settings-cloud-relay",
  "settings-discovery",
  "settings-web-integration",
  "settings-mcp",
  "settings-log-sources",
  "settings-execution-variables",
  "settings-general",
  "settings-storage",
  "settings-backup",
  "settings-instances",
  "settings-debug",
  "settings-security",
  "accessibility-explorer",
  "settings-updates",
  "orchestration-loop",
  "image-quality-tests",
  "terminal",
  "llm-analytics",
  "cost-control",
  "evaluation",
  "skills",
  "help",
  "automation-health",
  "activity-timeline",
  "watchers",
  "knowledge-explorer",
  "event-history",
  "development-intelligence",
  "demo-video",
  "product-tours",
  "session-recap",
  "api-surface",
  "decision-trail",
  "memory-search",
  "online-learning",
  "dag-workflow-editor",
  "project-explainer",
];

export const SIDEBAR_COLLAPSED_KEY = "qontinui-sidebar-collapsed";

/**
 * Human-readable labels for every `MainTabId`. Authoritative source for the
 * UI Bridge `GET /control/tabs` endpoint and any other consumer that needs
 * to present a tab by a display name rather than its raw id.
 *
 * Keep in sync with `VALID_TAB_IDS` above: every id in `VALID_TAB_IDS` must
 * have an entry here (the runtime test in `page.rs::tabs_list` asserts this).
 */
export const TAB_LABELS: Record<MainTabId, string> = {
  "prompt-home": "Home",
  "gui-automation": "Execute",
  "workflow-queue": "Workflow Queue",
  active: "Active Dashboard",
  runs: "History",
  history: "History",
  "error-monitor": "Error Monitor",
  processes: "Process Manager",
  reflection: "Reflection",
  observations: "Observations",
  architecture: "Architecture",
  "generator-eval": "Generator Eval",
  "meta-optimizer": "Meta-Optimizer",
  "run-recap": "Run Recap",
  "run-actions": "Run Actions",
  "run-image": "Image Recognition",
  "run-findings": "Findings",
  "run-state-explorer": "State Explorer",
  "run-tests": "Test Results",
  "run-ai-output": "AI Output",
  "run-statistics": "Statistics",
  "run-ai-data": "AI Data Viewer",
  "run-traces": "Traces",
  ai: "AI Output",
  logs: "Logs",
  "run-summary": "Run Summary",
  "monitor-summary": "Monitor Summary",
  "monitor-findings": "Monitor Findings",
  "monitor-issues": "Monitor Issues",
  "monitor-learnings": "Monitor Learnings",
  "monitor-state-explorer": "Monitor State Explorer",
  "monitor-statistics": "Monitor Statistics",
  "monitor-discoveries": "Monitor Discoveries",
  library: "Library",
  "step-builders": "Step Builders",
  "check-builder": "Check Builder",
  "check-group-builder": "Check Group Builder",
  "shell-command-builder": "Shell Command Builder",
  "task-builder": "Task Builder",
  "context-builder": "Context Builder",
  "playwright-test-builder": "Playwright Test Builder",
  "unified-workflow-builder": "Workflow Builder",
  "state-machine": "State Machines",
  specs: "Specs",
  capture: "Capture",
  "config-log-sources": "Log Sources",
  "config-findings": "Findings Config",
  "config-hooks": "Hooks",
  "config-ui-bridge": "UI Bridge",
  triggers: "Triggers",
  tasks: "Scheduler",
  settings: "Settings",
  "settings-account": "Account Settings",
  "settings-ai": "AI Settings",
  "settings-agentic": "Agentic Settings",
  "settings-self-healing": "Self-Healing Settings",
  "settings-world-state-verifier": "World State Verifier Settings",
  "settings-playwright": "Playwright Settings",
  "settings-mobile": "Mobile Settings",
  "settings-cloud-relay": "Cloud Relay Settings",
  "settings-discovery": "Discovery Settings",
  "settings-web-integration": "Web Integration Settings",
  "settings-mcp": "MCP Settings",
  "settings-log-sources": "Log Sources Settings",
  "settings-execution-variables": "Execution Variables",
  "settings-general": "General Settings",
  "settings-storage": "Storage Settings",
  "settings-backup": "Backup Settings",
  "settings-instances": "Instances Settings",
  "settings-debug": "Debug Settings",
  "settings-security": "Security Settings",
  "accessibility-explorer": "Accessibility Explorer",
  "settings-updates": "Updates Settings",
  "orchestration-loop": "Orchestration Loop",
  "image-quality-tests": "Image Quality Tests",
  terminal: "Terminal",
  "llm-analytics": "LLM Analytics",
  "cost-control": "Cost Control",
  evaluation: "Evaluation",
  skills: "Skills",
  help: "Help",
  "automation-health": "Automation Health",
  "activity-timeline": "Activity Timeline",
  watchers: "Watchers",
  "knowledge-explorer": "Knowledge Explorer",
  "event-history": "Event History",
  "development-intelligence": "Development Intelligence",
  "demo-video": "Demo Video",
  "product-tours": "Product Tours",
  "session-recap": "Session Recap",
  "api-surface": "API Surface",
  "decision-trail": "Decision Trail",
  "memory-search": "Memory Search",
  "online-learning": "Online Learning",
  "dag-workflow-editor": "DAG Workflow Editor",
  "project-explainer": "Project Explainer",
};

/** Ordered list of all tab ids with their human-readable labels. */
export const TAB_LIST: ReadonlyArray<{ id: MainTabId; label: string }> = VALID_TAB_IDS.map(
  (id) => ({ id, label: TAB_LABELS[id] }),
);

/** Type guard. */
export function isValidTabId(candidate: string): candidate is MainTabId {
  return VALID_TAB_IDS.includes(candidate as MainTabId);
}

/** Read the runner's last-persisted active tab from instance-scoped storage. */
export const ACTIVE_TAB_STORAGE_KEY = "qontinui-main-active-tab";

export function migrateTabId(stored: string | null): MainTabId {
  if (!stored) return "prompt-home";

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

  return "prompt-home";
}
