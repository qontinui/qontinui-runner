import { useEffect, useCallback, useState, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { useRouteAwareness } from "@qontinui/ui-bridge";
import { useApiReady, useRenderPerformance } from "@/hooks";
import { getApiPort } from "@/lib/runner-api";
import { instanceStorage } from "@/lib/instance-storage";
import type { MainTabId } from "./tab-types";
import { migrateTabId, SIDEBAR_COLLAPSED_KEY } from "./tab-types";
import { useNavigation } from "./NavigationContext";

const PAGE_TO_TAB: Record<string, MainTabId> = {
  // Prompt Home
  "prompt-home": "prompt-home",
  home: "prompt-home",
  // Run
  run: "gui-automation",
  "gui-automation": "gui-automation",
  active: "active",
  "workflow-queue": "workflow-queue",
  terminal: "terminal",
  "orchestration-loop": "orchestration-loop",
  // Observe
  runs: "runs",
  "run-recap": "run-recap",
  "run-dashboard": "run-recap",
  "run-actions": "run-actions",
  "run-image": "run-image",
  "run-findings": "run-findings",
  "run-state-explorer": "run-state-explorer",
  "run-tests": "run-tests",
  "run-ai-output": "run-ai-output",
  "run-ai-data": "run-ai-data",
  "run-statistics": "run-statistics",
  "run-traces": "run-traces",
  "run-summary": "run-recap",
  "error-monitor": "error-monitor",
  processes: "processes",
  "activity-timeline": "activity-timeline",
  "automation-health": "automation-health",
  "llm-analytics": "llm-analytics",
  "cost-control": "cost-control",
  // Learn
  "memory-search": "memory-search",
  "knowledge-explorer": "knowledge-explorer",
  "decision-trail": "decision-trail",
  "session-recap": "session-recap",
  reflection: "reflection",
  architecture: "architecture",
  "api-surface": "api-surface",
  "development-intelligence": "development-intelligence",
  "project-explainer": "project-explainer",
  // Build
  "unified-workflow-builder": "unified-workflow-builder",
  "step-builders": "step-builders",
  library: "library",
  "state-machine": "state-machine",
  specs: "specs",
  capture: "capture",
  "demo-video": "demo-video",
  "product-tours": "product-tours",
  // Configure
  triggers: "triggers",
  tasks: "tasks",
  settings: "settings",
  "settings-ai": "settings-ai",
  "settings-agentic": "settings-agentic",
  "settings-world-state-verifier": "settings-world-state-verifier",
  "settings-general": "settings-general",
  "config-findings": "config-findings",
  "config-hooks": "config-hooks",
  "config-log-sources": "config-log-sources",
  "config-ui-bridge": "config-ui-bridge",
  // Tools & System
  "generator-eval": "generator-eval",
  evaluation: "evaluation",
  skills: "skills",
  help: "help",
  "accessibility-explorer": "accessibility-explorer",
  // Legacy aliases
  ai: "run-ai-output",
  history: "runs",
  logs: "run-recap",
  "ui-bridge": "config-ui-bridge",
};

export interface ErrorMonitorScope {
  taskRunId?: string;
  taskRunName?: string;
}

interface UseAppNavigationReturn {
  activeTab: MainTabId;
  setActiveTab: (tab: MainTabId) => void;
  sidebarCollapsed: boolean;
  handleSidebarCollapsedChange: (value: boolean) => void;
  terminalSessionCount: number;
  setTerminalSessionCount: (count: number) => void;
  staleTaskMessage: string | null;
  setStaleTaskMessage: (msg: string | null) => void;
  errorMonitorScope: ErrorMonitorScope;
  clearErrorMonitorScope: () => void;
  ProfilerWrapper: ReturnType<typeof useRenderPerformance>["ProfilerWrapper"];
}

export function useAppNavigation(): UseAppNavigationReturn {
  const { ProfilerWrapper } = useRenderPerformance({ componentName: "AppContent" });
  const isApiReady = useApiReady();
  const { registerNavigate } = useNavigation();

  const [activeTab, setActiveTab] = useState<MainTabId>(() => {
    const stored = instanceStorage.getItem("qontinui-main-active-tab");
    return migrateTabId(stored);
  });

  // Feed the active tab into the UI Bridge navigation tracker as the canonical
  // route signal. The runner doesn't use react-router or history.pushState, so
  // `page.pathname` is frozen at whatever the webview loaded with; consumers
  // should read `page.route.pattern` / `page.route.id` instead. See
  // `ui_bridge_core.md` (Choosing a wait/nav/batch primitive) for the rationale.
  // `id` is not in the `RouteInfo` TS interface but the tracker stores the
  // object verbatim and emits it in `page.route`, so the extra field round-trips
  // to consumers reading `page.route.id`.
  useRouteAwareness({ pattern: activeTab, id: activeTab } as Parameters<
    typeof useRouteAwareness
  >[0]);

  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => {
    return instanceStorage.getItem(SIDEBAR_COLLAPSED_KEY) === "true";
  });

  const [terminalSessionCount, setTerminalSessionCount] = useState(0);
  const autoCollapsedRef = useRef(false);

  const [staleTaskMessage, setStaleTaskMessage] = useState<string | null>(null);
  const [errorMonitorScope, setErrorMonitorScope] = useState<ErrorMonitorScope>({});

  const clearErrorMonitorScope = useCallback(() => {
    setErrorMonitorScope({});
  }, []);

  useEffect(() => {
    if (isApiReady && getApiPort() !== 9876) {
      const stored = instanceStorage.getItem("qontinui-main-active-tab");
      const correct = migrateTabId(stored);
      // eslint-disable-next-line react-hooks/set-state-in-effect -- re-sync tab after API becomes ready
      setActiveTab(correct);
    }
  }, [isApiReady]);

  useEffect(() => {
    registerNavigate((page: string) => {
      const tabId = PAGE_TO_TAB[page];
      if (tabId) {
        setActiveTab(tabId);
      }
    });
  }, [registerNavigate]);

  useEffect(() => {
    const handler = (e: WindowEventMap["ui-bridge-navigate"]) => {
      const { page } = e.detail;
      const tabId = PAGE_TO_TAB[page];
      if (tabId) {
        setActiveTab(tabId);
      }
    };
    // Direct tab setter (bypasses PAGE_TO_TAB for navigate_tab endpoint)
    const directHandler = (e: CustomEvent<{ tab: MainTabId }>) => {
      setActiveTab(e.detail.tab);
    };
    window.addEventListener("ui-bridge-navigate", handler);
    window.addEventListener("ui-bridge-set-tab", directHandler as EventListener);
    return () => {
      window.removeEventListener("ui-bridge-navigate", handler);
      window.removeEventListener("ui-bridge-set-tab", directHandler as EventListener);
    };
  }, []);

  // Tauri-native listener for the `ui-bridge:activate-tab` event emitted by
  // `POST /ui-bridge/control/activate-tab/{tab_id}`. This is intentionally
  // separate from the `ui-bridge-set-tab` window event used by the legacy
  // `/page/set-tab` endpoint, so the two paths remain independently
  // debuggable. Sub-tab propagation for `settings-*` ids is handled
  // automatically by TabContent → Settings via the `defaultTab` prop and
  // Settings' `useEffect([defaultTab])`, so we only need to set the main
  // activeTab here.
  useEffect(() => {
    let unlisten: (() => void) | null = null;

    const setup = async () => {
      unlisten = await listen<{ tab_id: string }>("ui-bridge:activate-tab", (event) => {
        const { tab_id } = event.payload ?? { tab_id: "" };
        if (!tab_id) return;
        setActiveTab(tab_id as MainTabId);
      });
    };

    setup();

    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | null = null;

    const setupNavigationListener = async () => {
      unlisten = await listen<{
        type: string;
        page: string;
        task_run_id?: number;
        select_run?: number;
      }>("test-navigation", (event) => {
        const { page } = event.payload;
        const tabId = PAGE_TO_TAB[page];
        if (tabId) {
          setActiveTab(tabId);
          if (page === "state-machine") {
            setTimeout(() => window.dispatchEvent(new Event("sm-show-exploration")), 200);
          }
        } else {
          console.warn(`[APP] Unknown page for navigation: ${page}`);
        }
      });
    };

    setupNavigationListener();

    return () => {
      if (unlisten) {
        unlisten();
      }
    };
  }, []);

  useEffect(() => {
    const handleNavigateToErrorMonitor = (
      e: CustomEvent<{ taskRunId?: string; taskRunName?: string }>,
    ) => {
      const { taskRunId, taskRunName } = e.detail ?? {};
      setErrorMonitorScope({ taskRunId, taskRunName });
      setActiveTab("error-monitor");
    };
    window.addEventListener("navigate-to-error-monitor", handleNavigateToErrorMonitor);
    return () =>
      window.removeEventListener("navigate-to-error-monitor", handleNavigateToErrorMonitor);
  }, []);

  useEffect(() => {
    const handleNavigateToActive = () => {
      setActiveTab("active");
    };
    window.addEventListener("navigate-to-active", handleNavigateToActive);
    return () => window.removeEventListener("navigate-to-active", handleNavigateToActive);
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | null = null;

    const setup = async () => {
      unlisten = await listen<{
        task_run_id: string;
        task_name: string;
        message: string;
      }>("stale-task-detected", (event) => {
        const { task_name, message } = event.payload;
        setStaleTaskMessage(`${task_name}: ${message}`);
        setTimeout(() => setStaleTaskMessage(null), 10000);
      });
    };

    setup();

    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  const handleSidebarCollapsedChange = useCallback((value: boolean) => {
    autoCollapsedRef.current = false;
    setSidebarCollapsed(value);
    instanceStorage.setItem(SIDEBAR_COLLAPSED_KEY, JSON.stringify(value));
  }, []);

  useEffect(() => {
    const shouldAutoCollapse = activeTab === "terminal" && terminalSessionCount > 1;

    if (shouldAutoCollapse && !sidebarCollapsed) {
      autoCollapsedRef.current = true;
      // eslint-disable-next-line react-hooks/set-state-in-effect -- auto-collapse sidebar for terminal multi-session
      setSidebarCollapsed(true);
    } else if (!shouldAutoCollapse && autoCollapsedRef.current) {
      autoCollapsedRef.current = false;
      setSidebarCollapsed(false);
    }
  }, [activeTab, terminalSessionCount, sidebarCollapsed]);

  useEffect(() => {
    instanceStorage.setItem("qontinui-main-active-tab", activeTab);
  }, [activeTab]);

  // Register navigateHandler on __UI_BRIDGE__ so that pageNavigate commands
  // (soft-navigation path) and the NL executor can navigate the runner via
  // URL-like paths. The runner uses tab-based navigation rather than URL routing,
  // so we translate paths into tab IDs using PAGE_TO_TAB.
  useEffect(() => {
    const g = (window as unknown as Record<string, unknown>).__UI_BRIDGE__ as
      | Record<string, unknown>
      | undefined;
    if (g) {
      g.navigateHandler = (url: string) => {
        // Strip leading slash and any query/hash
        const raw = url.startsWith("/") ? url.slice(1) : url;
        const path = raw.split(/[?#]/)[0];
        const tabId = PAGE_TO_TAB[path];
        if (tabId) {
          setActiveTab(tabId);
        } else {
          // Fall back: try using the path directly as a tab ID
          setActiveTab(path as MainTabId);
        }
      };
    }
    return () => {
      const g2 = (window as unknown as Record<string, unknown>).__UI_BRIDGE__ as
        | Record<string, unknown>
        | undefined;
      if (g2?.navigateHandler) {
        delete g2.navigateHandler;
      }
    };
  }, [setActiveTab]);

  return {
    activeTab,
    setActiveTab,
    sidebarCollapsed,
    handleSidebarCollapsedChange,
    terminalSessionCount,
    setTerminalSessionCount,
    staleTaskMessage,
    setStaleTaskMessage,
    errorMonitorScope,
    clearErrorMonitorScope,
    ProfilerWrapper,
  };
}
