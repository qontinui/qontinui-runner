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
  | "monitor-findings"
  | "monitor-state-explorer"
  | "monitor-statistics"
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
  | "memory-federation"
  | "helper-tasks";

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
  "monitor-findings",
  "monitor-state-explorer",
  "monitor-statistics",
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
  "helper-tasks",
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
  "monitor-findings": "Monitor Findings",
  "monitor-state-explorer": "Monitor State Explorer",
  "monitor-statistics": "Monitor Statistics",
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
  "helper-tasks": "Helper Tasks",
};

/**
 * Logical section a tab belongs to — surfaces in `GET /control/tabs` so
 * agents can navigate the 90+ tab catalog by grouping:
 *   - "nav":      sidebar top-level navigation tabs
 *   - "run":      run-detail sub-tabs (opened from a specific run)
 *   - "monitor":  live-monitor sub-tabs
 *   - "settings": settings panel sub-tabs
 *   - "build":    workflow / spec / state-machine builders
 *   - "config":   configuration tabs (log sources, findings, hooks, triggers, tasks)
 *   - "system":   everything else (help, skills, analytics, debugging views, …)
 */
export type TabSection = "nav" | "run" | "monitor" | "settings" | "build" | "config" | "system";

/**
 * Sidebar top-level navigation tabs. Explicit override — these don't share a
 * common prefix with each other, so we hand-list them rather than try to
 * infer them. Everything not in this set falls through to the prefix rule.
 */
const NAV_TAB_IDS: ReadonlySet<MainTabId> = new Set<MainTabId>([
  "prompt-home",
  "gui-automation",
  "workflow-queue",
  "active",
  "runs",
  "history",
  "error-monitor",
  "processes",
  "terminal",
  "wrappers",
  "productivity",
  "regression",
  "helper-tasks",
]);

/**
 * Workflow / spec / state-machine building tabs. Explicit — these have
 * mixed prefixes (state-*, spec*, capture, library, *-builder, …).
 */
const BUILD_TAB_IDS: ReadonlySet<MainTabId> = new Set<MainTabId>([
  "state-machine",
  "specs",
  "capture",
  "library",
  "step-builders",
  "check-builder",
  "check-group-builder",
  "shell-command-builder",
  "task-builder",
  "context-builder",
  "playwright-test-builder",
  "unified-workflow-builder",
  "dag-workflow-editor",
]);

/**
 * Non-prefix-matching config tabs. The `config-*` family is picked up by
 * the prefix rule below; these are misc config-ish tabs without the prefix.
 */
const CONFIG_TAB_IDS: ReadonlySet<MainTabId> = new Set<MainTabId>(["triggers", "tasks"]);

/**
 * Derive the section for a tab id. Prefix rule covers the large families
 * (run-*, monitor-*, settings-*, config-*); the explicit overrides above
 * capture sidebar nav and build/config tabs that don't share a prefix.
 * Unknown ids fall back to "system".
 */
export function deriveSection(id: MainTabId): TabSection {
  if (NAV_TAB_IDS.has(id)) return "nav";
  if (BUILD_TAB_IDS.has(id)) return "build";
  if (CONFIG_TAB_IDS.has(id)) return "config";
  if (id.startsWith("run-")) return "run";
  if (id.startsWith("monitor-")) return "monitor";
  if (id.startsWith("settings-") || id === "settings") return "settings";
  if (id.startsWith("config-")) return "config";
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

/**
 * Legacy tab ids kept alive purely as ALIASES: ids that once existed (as a nav
 * item, a persisted `activeTab`, or a UI Bridge target) but no longer name a
 * page. They are deliberately NOT in `MainTabId` — nothing may route to them —
 * but a value persisted by an older build, or a stale UI Bridge caller, still
 * resolves through this table to the page that superseded them.
 *
 * (iter-2 R1: `run-summary`, `monitor-summary`, `monitor-issues`,
 * `monitor-learnings` and `monitor-discoveries` were removed from `MainTabId`
 * for exactly this reason — they were "valid" ids with no renderer, so the UI
 * Bridge happily activated them into a "could not be displayed" page.)
 */
const LEGACY_TAB_MIGRATIONS: Record<string, MainTabId> = {
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

/**
 * Resolve a tab id that arrived from OUTSIDE the type system — a UI Bridge
 * `activate-tab` payload, a `ui-bridge-set-tab` window event, a
 * `navigateHandler` path, a Tauri event. Returns `null` for anything unknown so
 * the caller can REFUSE the navigation rather than corrupt `activeTab`.
 *
 * Ordering differs from `migrateTabId` on purpose: a live id wins over an alias.
 * Several ids are BOTH a real tab and a migration key (`check-builder`,
 * `monitor-findings`, `history`, `logs`, …); an external caller asking for
 * `check-builder` means the page called "Check Builder", not the alias's
 * `step-builders` target. `migrateTabId` (persisted-storage restore) keeps its
 * alias-first ordering unchanged so a stored value still lands where previous
 * builds sent it.
 *
 * iter-2 R3: replaces three unguarded `as MainTabId` casts in
 * `useAppNavigation` that let any string become the active tab.
 */
export function resolveExternalTabId(candidate: string | null | undefined): MainTabId | null {
  if (!candidate) return null;
  const trimmed = candidate.trim();
  if (!trimmed) return null;
  if (isValidTabId(trimmed)) return trimmed;
  return LEGACY_TAB_MIGRATIONS[trimmed] ?? null;
}

/**
 * Resolve the PERSISTED active-tab value read back from instanceStorage.
 * Alias-first (see `resolveExternalTabId`), and always yields a tab — an
 * unreadable/unknown stored value falls back to `prompt-home`.
 */
export function migrateTabId(stored: string | null): MainTabId {
  if (!stored) return "prompt-home";

  if (stored in LEGACY_TAB_MIGRATIONS) {
    return LEGACY_TAB_MIGRATIONS[stored];
  }

  if (isValidTabId(stored)) {
    return stored;
  }

  return "prompt-home";
}
