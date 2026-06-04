import { useEffect, useCallback, useState, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { useRouteAwareness } from "@qontinui/ui-bridge";
import { useApiReady, useRenderPerformance } from "@/hooks";
import { getApiPort } from "@/lib/runner-api";
import { instanceStorage } from "@/lib/instance-storage";
import type { MainTabId } from "./tab-types";
import type { ProductivityView } from "@/components/productivity/types";
import {
  ACTIVE_TAB_STORAGE_KEY,
  migrateTabId,
  setActiveTabAndPersist,
  SIDEBAR_COLLAPSED_KEY,
} from "./tab-types";
import { useNavigation } from "./NavigationContext";

export const PAGE_TO_TAB: Record<string, MainTabId> = {
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
  productivity: "productivity",
  "productivity-plans": "productivity",
  "productivity-coordinator": "productivity",
  "productivity-knowledge": "productivity",
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
  // Wrappers
  wrappers: "wrappers",
  // Tools & System
  "generator-eval": "generator-eval",
  evaluation: "evaluation",
  skills: "skills",
  help: "help",
  "accessibility-explorer": "accessibility-explorer",
  // Memory Federation
  "memory-federation": "memory-federation",
  "admin/memory-federation": "memory-federation",
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
    // Phase 1 (pop-out terminal windows): a window opened with the
    // `?view=terminal` boot hint renders the Terminal page directly and
    // ignores the shared persisted active-tab (which is the main window's).
    try {
      if (new URLSearchParams(window.location.search).get("view") === "terminal") {
        return "terminal";
      }
    } catch {
      /* non-DOM / parse failure — fall through to the persisted tab */
    }
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
    // Honor a persisted explicit choice first. Otherwise default-collapse on
    // narrow viewports (<1280px) so the content area gets enough horizontal
    // room — addresses page-health `spatial_coverage` WARNING where the
    // sidebar occupied ~23% of the left half and content shrank to 9-20%.
    const stored = instanceStorage.getItem(SIDEBAR_COLLAPSED_KEY);
    if (stored !== null) return stored === "true";
    if (typeof window !== "undefined" && window.innerWidth < 1280) return true;
    return false;
  });

  const [terminalSessionCount, setTerminalSessionCount] = useState(0);
  const autoCollapsedRef = useRef(false);

  const [staleTaskMessage, setStaleTaskMessage] = useState<string | null>(null);
  const [errorMonitorScope, setErrorMonitorScope] = useState<ErrorMonitorScope>({});

  const clearErrorMonitorScope = useCallback(() => {
    setErrorMonitorScope({});
  }, []);

  // One-shot reconciliation: when the API port finalises (it can resolve
  // *after* this hook mounts on temp/spawned runners), the initial mount-time
  // read of `instanceStorage` may have hit the wrong namespace and defaulted
  // to "prompt-home". Once `isApiReady` flips true and the port is settled,
  // re-read storage ONCE to pick up the correct value.
  //
  // Critically this must NOT clobber a deliberate live state change. If the
  // user (or a Tauri event, or a UI Bridge command) has already navigated
  // away from "prompt-home" before isApiReady fires, that live state wins —
  // we only reconcile when our in-memory state is still the default
  // ("prompt-home") and storage now has a different (real) persisted value.
  // Without this guard, isApiReady flipping (e.g. after a transient health
  // probe failure) would yank the user back to whatever's stored.
  const hasReconciledApiPortRef = useRef(false);
  useEffect(() => {
    if (!isApiReady) return;
    if (getApiPort() === 9876) return;
    if (hasReconciledApiPortRef.current) return;
    hasReconciledApiPortRef.current = true;
    const stored = instanceStorage.getItem(ACTIVE_TAB_STORAGE_KEY);
    const correct = migrateTabId(stored);
    // Only adopt the storage value if our current state is still the mount
    // default AND storage actually contains a real (non-null) value. This
    // prevents wiping a deliberate navigation that happened in the gap
    // between mount and isApiReady=true.
    setActiveTab((current) => {
      if (current !== "prompt-home") return current;
      if (stored === null) return current;
      return correct;
    });
  }, [isApiReady]);

  // When a `productivity-*` alias is requested, the main tab is `productivity`
  // but the sub-view (plans/coordinator/knowledge) needs to be surfaced to the
  // Productivity page. Dispatch a window event the page listens for. Pattern
  // mirrors the Settings `defaultTab` flow but goes via an event because the
  // PAGE_TO_TAB map collapses all three aliases into a single tab id.
  //
  // Timing: we just called `setActiveTabAndPersist`, but React hasn't mounted
  // the ProductivityPage yet, so its `productivity-set-view` listener isn't
  // attached. Dispatching synchronously drops the event. Instead we await
  // ProductivityPage's mount-promise: it sets `__qontinuiProductivityReady`
  // and dispatches `productivity-page-mounted` from inside its mount effect.
  // A 250ms timeout fires the dispatch anyway as a defensive guard against
  // the page never mounting (e.g. user navigated to a different tab right
  // after this call).
  const dispatchProductivitySubView = useCallback((page: string) => {
    let view: ProductivityView | null = null;
    if (page === "productivity-plans" || page === "productivity") {
      view = "plans";
    } else if (page === "productivity-coordinator") {
      view = "coordinator";
    } else if (page === "productivity-knowledge") {
      view = "knowledge";
    }
    if (!view) return;
    const targetView = view;
    const fire = () => {
      window.dispatchEvent(
        new CustomEvent("productivity-set-view", { detail: { view: targetView } }),
      );
    };
    const ready = (window as unknown as Record<string, unknown>).__qontinuiProductivityReady;
    if (ready === true) {
      fire();
      return;
    }
    let fired = false;
    const onMounted = () => {
      if (fired) return;
      fired = true;
      window.removeEventListener("productivity-page-mounted", onMounted);
      clearTimeout(timeoutId);
      fire();
    };
    window.addEventListener("productivity-page-mounted", onMounted);
    const timeoutId = window.setTimeout(() => {
      if (fired) return;
      fired = true;
      window.removeEventListener("productivity-page-mounted", onMounted);
      console.warn(
        `[useAppNavigation] productivity-page-mounted timeout (250ms) for view="${targetView}"; firing dispatch anyway`,
      );
      fire();
    }, 250);
  }, []);

  useEffect(() => {
    registerNavigate((page: string) => {
      const tabId = PAGE_TO_TAB[page];
      if (tabId) {
        setActiveTabAndPersist(setActiveTab, instanceStorage, tabId);
        dispatchProductivitySubView(page);
      }
    });
  }, [registerNavigate, dispatchProductivitySubView]);

  useEffect(() => {
    const handler = (e: WindowEventMap["ui-bridge-navigate"]) => {
      const { page } = e.detail;
      const tabId = PAGE_TO_TAB[page];
      if (tabId) {
        setActiveTabAndPersist(setActiveTab, instanceStorage, tabId);
        dispatchProductivitySubView(page);
      }
    };
    // Direct tab setter (bypasses PAGE_TO_TAB for navigate_tab endpoint)
    const directHandler = (e: CustomEvent<{ tab: MainTabId }>) => {
      setActiveTabAndPersist(setActiveTab, instanceStorage, e.detail.tab);
    };
    window.addEventListener("ui-bridge-navigate", handler);
    window.addEventListener("ui-bridge-set-tab", directHandler as EventListener);
    return () => {
      window.removeEventListener("ui-bridge-navigate", handler);
      window.removeEventListener("ui-bridge-set-tab", directHandler as EventListener);
    };
  }, [dispatchProductivitySubView]);

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
        setActiveTabAndPersist(setActiveTab, instanceStorage, tab_id as MainTabId);
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
          setActiveTabAndPersist(setActiveTab, instanceStorage, tabId);
          if (page === "state-machine") {
            setTimeout(() => window.dispatchEvent(new Event("sm-show-exploration")), 200);
          }
          dispatchProductivitySubView(page);
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
  }, [dispatchProductivitySubView]);

  useEffect(() => {
    const handleNavigateToErrorMonitor = (
      e: CustomEvent<{ taskRunId?: string; taskRunName?: string }>,
    ) => {
      const { taskRunId, taskRunName } = e.detail ?? {};
      setErrorMonitorScope({ taskRunId, taskRunName });
      setActiveTabAndPersist(setActiveTab, instanceStorage, "error-monitor");
    };
    window.addEventListener("navigate-to-error-monitor", handleNavigateToErrorMonitor);
    return () =>
      window.removeEventListener("navigate-to-error-monitor", handleNavigateToErrorMonitor);
  }, []);

  useEffect(() => {
    const handleNavigateToActive = () => {
      setActiveTabAndPersist(setActiveTab, instanceStorage, "active");
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

  // Viewport-driven auto-collapse: when the window narrows below 1280px,
  // collapse the sidebar so the content area gets the horizontal room it
  // needs (page-health `spatial_coverage` improvement). When the window
  // widens past the threshold, re-expand IF the previous collapse was
  // automatic (autoCollapsedRef set) so we don't undo an explicit operator
  // toggle.
  useEffect(() => {
    if (typeof window === "undefined") return;
    const NARROW_BREAKPOINT_PX = 1280;
    const onResize = () => {
      const narrow = window.innerWidth < NARROW_BREAKPOINT_PX;
      setSidebarCollapsed((current) => {
        if (narrow && !current) {
          autoCollapsedRef.current = true;
          return true;
        }
        if (!narrow && current && autoCollapsedRef.current) {
          autoCollapsedRef.current = false;
          return false;
        }
        return current;
      });
    };
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  // Backstop persistence: every event-handler that calls `setActiveTab`
  // already persists synchronously via `setActiveTabAndPersist`, but this
  // useEffect catches any caller we missed (and also runs once on mount to
  // seed storage with the initial state) so storage and React state never
  // permanently diverge.
  useEffect(() => {
    instanceStorage.setItem(ACTIVE_TAB_STORAGE_KEY, activeTab);
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
          setActiveTabAndPersist(setActiveTab, instanceStorage, tabId);
          dispatchProductivitySubView(path);
        } else {
          // Fall back: try using the path directly as a tab ID
          setActiveTabAndPersist(setActiveTab, instanceStorage, path as MainTabId);
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
  }, [setActiveTab, dispatchProductivitySubView]);

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
