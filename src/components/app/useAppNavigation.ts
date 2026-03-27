import { useEffect, useCallback, useState, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { useApiReady, useRenderPerformance } from "@/hooks";
import { getApiPort } from "@/lib/runner-api";
import { instanceStorage } from "@/lib/instance-storage";
import type { MainTabId } from "./tab-types";
import { migrateTabId, SIDEBAR_COLLAPSED_KEY } from "./tab-types";
import { useNavigation } from "./NavigationContext";

const PAGE_TO_TAB: Record<string, MainTabId> = {
  run: "gui-automation",
  "gui-automation": "gui-automation",
  active: "active",
  "unified-workflow-builder": "unified-workflow-builder",
  library: "library",
  ai: "ai",
  settings: "settings",
  help: "help",
  "ui-bridge": "settings",
  "run-recap": "run-recap",
  "run-dashboard": "run-recap",
  history: "history",
  logs: "logs",
  "error-monitor": "error-monitor",
  "state-machine": "state-machine",
};

interface UseAppNavigationReturn {
  activeTab: MainTabId;
  setActiveTab: (tab: MainTabId) => void;
  sidebarCollapsed: boolean;
  handleSidebarCollapsedChange: (value: boolean) => void;
  terminalSessionCount: number;
  setTerminalSessionCount: (count: number) => void;
  staleTaskMessage: string | null;
  setStaleTaskMessage: (msg: string | null) => void;
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

  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => {
    return instanceStorage.getItem(SIDEBAR_COLLAPSED_KEY) === "true";
  });

  const [terminalSessionCount, setTerminalSessionCount] = useState(0);
  const autoCollapsedRef = useRef(false);

  const [staleTaskMessage, setStaleTaskMessage] = useState<string | null>(null);

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
    window.addEventListener("ui-bridge-navigate", handler);
    return () => window.removeEventListener("ui-bridge-navigate", handler);
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
    const handleNavigateToErrorMonitor = () => {
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

  return {
    activeTab,
    setActiveTab,
    sidebarCollapsed,
    handleSidebarCollapsedChange,
    terminalSessionCount,
    setTerminalSessionCount,
    staleTaskMessage,
    setStaleTaskMessage,
    ProfilerWrapper,
  };
}
