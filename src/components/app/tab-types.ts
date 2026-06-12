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
  | "settings-discovery"
  | "settings-backend-connection"
  | "settings-dev-loop"
  | "settings-mcp"
  | "settings-log-sources"
  | "settings-execution-variables"
  | "settings-general"
  | "settings-storage"
  | "settings-backup"
  | "settings-instances"
  | "settings-debug"
  | "settings-security"
  | "settings-notifications"
  | "settings-otel"
  | "settings-containers"
  | "settings-ci-runner"
  | "settings-lock-yield"
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
  | "project-explainer"
  | "wrappers"
  | "productivity"
  | "regression"
  | "memory-federation";

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
  "settings-discovery",
  "settings-backend-connection",
  "settings-dev-loop",
  "settings-mcp",
  "settings-log-sources",
  "settings-execution-variables",
  "settings-general",
  "settings-storage",
  "settings-backup",
  "settings-instances",
  "settings-debug",
  "settings-security",
  "settings-notifications",
  "settings-otel",
  "settings-containers",
  "settings-ci-runner",
  "settings-lock-yield",
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
  "wrappers",
  "productivity",
  "regression",
  "memory-federation",
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
  runs: "Runs",
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
  ai: "AI Output (Legacy)",
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
  "settings-discovery": "Discovery Settings",
  "settings-backend-connection": "Backend Connection Settings",
  "settings-dev-loop": "Test My Change",
  "settings-mcp": "MCP Settings",
  "settings-log-sources": "Log Sources Settings",
  "settings-execution-variables": "Execution Variables",
  "settings-general": "General Settings",
  "settings-storage": "Storage Settings",
  "settings-backup": "Backup Settings",
  "settings-instances": "Instances Settings",
  "settings-debug": "Debug Settings",
  "settings-security": "Security Settings",
  "settings-notifications": "Notification Settings",
  "settings-otel": "OpenTelemetry Settings",
  "settings-containers": "Container Isolation Settings",
  "settings-ci-runner": "CI Runner Settings",
  "settings-lock-yield": "Lock-yield Policy Settings",
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
  wrappers: "Wrappers",
  productivity: "Productivity",
  regression: "Regression",
  "memory-federation": "Memory Federation",
};

/**
 * Logical section a tab belongs to — surfaces in `GET /control/tabs` so
 * agents can navigate the 100+ tab catalog by grouping. The vocabulary
 * mirrors the terminal-centric sidebar IA (see `@qontinui/navigation`
 * `groups.ts`) so the agent-facing catalog and the human sidebar agree:
 *   - "workspace": daily entry points (Terminal, Home, Active, Productivity, …)
 *   - "review":    runs, run-detail sub-tabs, memory, knowledge
 *   - "spend":     token cost (LLM analytics, cost control)
 *   - "automate":  scheduled / reactive agents (triggers, tasks, watchers)
 *   - "build":     workflow / spec / state-machine builders + library
 *   - "insights":  live monitoring + accumulated analysis (errors, monitor-* …)
 *   - "configure": configuration tabs (config-*, event history)
 *   - "dev":       dev-only tooling (generator eval, meta-optimizer, skills, …)
 *   - "system":    settings panel + help + anything unclassified
 */
export type TabSection =
  | "workspace"
  | "review"
  | "spend"
  | "automate"
  | "build"
  | "insights"
  | "configure"
  | "dev"
  | "system";

/** WORKSPACE — daily entry points. */
const WORKSPACE_TAB_IDS: ReadonlySet<MainTabId> = new Set<MainTabId>([
  "prompt-home",
  "gui-automation",
  "workflow-queue",
  "active",
  "terminal",
  "productivity",
]);

/**
 * REVIEW — runs and the intelligence reviewed afterwards. The `run-*`
 * detail sub-tabs are covered by the prefix rule below.
 */
const REVIEW_TAB_IDS: ReadonlySet<MainTabId> = new Set<MainTabId>([
  "runs",
  "history",
  "ai",
  "memory-search",
  "knowledge-explorer",
]);

/** SPEND — token cost surfaces. */
const SPEND_TAB_IDS: ReadonlySet<MainTabId> = new Set<MainTabId>([
  "llm-analytics",
  "cost-control",
]);

/** AUTOMATE — scheduled / reactive agents. */
const AUTOMATE_TAB_IDS: ReadonlySet<MainTabId> = new Set<MainTabId>([
  "triggers",
  "tasks",
  "watchers",
]);

/**
 * BUILD — workflow / spec / state-machine builders and assets. Mixed
 * prefixes (state-*, spec*, capture, library, *-builder, …), so explicit.
 */
const BUILD_TAB_IDS: ReadonlySet<MainTabId> = new Set<MainTabId>([
  "unified-workflow-builder",
  "dag-workflow-editor",
  "step-builders",
  "check-builder",
  "check-group-builder",
  "shell-command-builder",
  "task-builder",
  "context-builder",
  "playwright-test-builder",
  "library",
  "state-machine",
  "specs",
  "capture",
  "regression",
  "demo-video",
  "product-tours",
  "orchestration-loop",
  "wrappers",
]);

/**
 * INSIGHTS — live monitoring + accumulated analysis. The `monitor-*`
 * family is covered by the prefix rule below.
 */
const INSIGHTS_TAB_IDS: ReadonlySet<MainTabId> = new Set<MainTabId>([
  "error-monitor",
  "processes",
  "automation-health",
  "activity-timeline",
  "reflection",
  "observations",
  "architecture",
  "api-surface",
  "development-intelligence",
  "project-explainer",
  "decision-trail",
  "session-recap",
  "logs",
  "evaluation",
  "memory-federation",
]);

/** DEV — dev-only tooling (mostly `hiddenInProd` in the sidebar). */
const DEV_TAB_IDS: ReadonlySet<MainTabId> = new Set<MainTabId>([
  "generator-eval",
  "meta-optimizer",
  "online-learning",
  "skills",
  "image-quality-tests",
  "accessibility-explorer",
]);

/** CONFIGURE — misc config tabs without the `config-` prefix. */
const CONFIGURE_TAB_IDS: ReadonlySet<MainTabId> = new Set<MainTabId>(["event-history"]);

/**
 * Derive the section for a tab id. Explicit sets mirror the sidebar IA
 * groups; prefix rules cover the large sub-tab families (run-* → review,
 * monitor-* → insights, settings-* → system, config-* → configure).
 * Unknown ids fall back to "system".
 */
export function deriveSection(id: MainTabId): TabSection {
  if (WORKSPACE_TAB_IDS.has(id)) return "workspace";
  if (REVIEW_TAB_IDS.has(id)) return "review";
  if (SPEND_TAB_IDS.has(id)) return "spend";
  if (AUTOMATE_TAB_IDS.has(id)) return "automate";
  if (BUILD_TAB_IDS.has(id)) return "build";
  if (INSIGHTS_TAB_IDS.has(id)) return "insights";
  if (DEV_TAB_IDS.has(id)) return "dev";
  if (CONFIGURE_TAB_IDS.has(id)) return "configure";
  if (id.startsWith("run-")) return "review";
  if (id.startsWith("monitor-")) return "insights";
  if (id.startsWith("config-")) return "configure";
  if (id.startsWith("settings-") || id === "settings") return "system";
  return "system";
}

/** Ordered list of all tab ids with their human-readable labels and section. */
export const TAB_LIST: ReadonlyArray<{ id: MainTabId; label: string; section: TabSection }> =
  VALID_TAB_IDS.map((id) => ({ id, label: TAB_LABELS[id], section: deriveSection(id) }));

/** Type guard. */
export function isValidTabId(candidate: string): candidate is MainTabId {
  return VALID_TAB_IDS.includes(candidate as MainTabId);
}

/** Read the runner's last-persisted active tab from instance-scoped storage. */
export const ACTIVE_TAB_STORAGE_KEY = "qontinui-main-active-tab";

/**
 * Atomic-set-and-persist helper. Calls `setActiveTab` (the React state setter
 * passed in by `useAppNavigation`) AND writes the new tab id to the
 * port-namespaced instanceStorage in the same synchronous tick.
 *
 * Why this exists: the `useEffect([activeTab])` writer in `useAppNavigation`
 * is a backstop — it fires after the next React render. Readers that consult
 * `instanceStorage.getItem(ACTIVE_TAB_STORAGE_KEY)` directly (the F4
 * `tabs_list` handler, route-aware UI Bridge handlers, anything that bypasses
 * the React state) would otherwise see a stale value between the moment a
 * user click / Tauri event flips React state and the moment the effect
 * commits. Persisting synchronously inside the handler makes the storage
 * write atomic with the state change so post-handler probes always see the
 * new tab.
 *
 * Callers must still pass the React `setActiveTab` setter so React reconciles
 * normally — this helper does NOT bypass React.
 */
export function setActiveTabAndPersist(
  setActiveTab: (tab: MainTabId) => void,
  storage: { setItem: (key: string, value: string) => void },
  tab: MainTabId,
): void {
  setActiveTab(tab);
  storage.setItem(ACTIVE_TAB_STORAGE_KEY, tab);
}

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
