/**
 * RenderLogContext
 *
 * Provides automatic render logging across all views in the application.
 * Captures DOM snapshots on:
 * - Tab/route changes
 * - Significant DOM mutations
 * - Manual triggers
 *
 * This enables Python tests to verify UI state without Playwright.
 */

import {
  createContext,
  useContext,
  useEffect,
  useRef,
  useCallback,
  useState,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  captureDOMSnapshot,
  captureQuickSummary,
  type DOMSnapshot,
  type CaptureConfig,
} from "../utils/domCapture";

// ============================================================================
// Types
// ============================================================================

export interface RenderLogEntry {
  id: string;
  timestamp: number;
  component: string;
  trigger: string;
  data: Record<string, unknown>;
  visible_sections?: string[];
  task_run_id?: number;
}

interface RenderLogContextValue {
  /** Current active tab */
  activeTab: string | null;
  /** Whether automatic logging is enabled */
  isEnabled: boolean;
  /** Enable/disable automatic logging */
  setEnabled: (enabled: boolean) => void;
  /** Manually capture a full DOM snapshot */
  captureSnapshot: (trigger?: string) => Promise<void>;
  /** Manually capture a quick summary */
  captureQuick: (trigger?: string) => Promise<void>;
  /** Clear the render log */
  clearLog: () => Promise<void>;
  /** Last capture timestamp */
  lastCaptureTime: number | null;
  /** Number of captures in current session */
  captureCount: number;
}

const RenderLogContext = createContext<RenderLogContextValue | null>(null);

// ============================================================================
// Provider Component
// ============================================================================

interface RenderLogProviderProps {
  children: ReactNode;
  /** Current active tab ID from App */
  activeTab: string;
  /** Task run ID if within a run context */
  taskRunId?: number;
  /** Enable capture on mount */
  enableOnMount?: boolean;
  /** Debounce time for mutation captures (ms) */
  mutationDebounceMs?: number;
  /** Enable mutation observer */
  enableMutationObserver?: boolean;
  /** Custom capture config */
  captureConfig?: Partial<CaptureConfig>;
}

export function RenderLogProvider({
  children,
  activeTab,
  taskRunId,
  enableOnMount = true,
  mutationDebounceMs = 500,
  enableMutationObserver = true,
  captureConfig = {},
}: RenderLogProviderProps) {
  const [isEnabled, setEnabled] = useState(enableOnMount);
  const [lastCaptureTime, setLastCaptureTime] = useState<number | null>(null);
  const [captureCount, setCaptureCount] = useState(0);

  // Refs for debouncing and tracking
  const mutationTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const lastTabRef = useRef<string | null>(null);
  const observerRef = useRef<MutationObserver | null>(null);
  const isCapturingRef = useRef(false);

  // Check if we're in development mode
  const isDev = import.meta.env.DEV;

  /**
   * Send a render log entry to the backend
   */
  const sendLog = useCallback(
    async (entry: RenderLogEntry) => {
      if (!isDev) return;

      try {
        await invoke("append_render_log", { entry });
      } catch (error) {
        console.debug("[RenderLog] Failed to append log:", error);
      }
    },
    [isDev],
  );

  /**
   * Capture and send a full DOM snapshot
   */
  const captureSnapshot = useCallback(
    async (trigger: string = "manual") => {
      if (!isEnabled || !isDev || isCapturingRef.current) return;

      isCapturingRef.current = true;

      try {
        // Wait a frame for DOM to settle
        await new Promise((resolve) => requestAnimationFrame(resolve));

        const snapshot = captureDOMSnapshot(
          captureConfig,
          activeTab,
          trigger as DOMSnapshot["captureType"],
        );

        const entry: RenderLogEntry = {
          id: snapshot.id,
          timestamp: snapshot.timestamp,
          component: "DOMSnapshot",
          trigger,
          data: {
            // Page info
            page_title: snapshot.page.title,
            page_url: snapshot.page.url,
            viewport: snapshot.page.viewport,
            scroll: snapshot.page.scroll,

            // Active tab
            active_tab: snapshot.activeTab,

            // Text content (organized)
            headings: snapshot.textContent.headings,
            buttons: snapshot.textContent.buttons,
            links: snapshot.textContent.links.slice(0, 50),
            labels: snapshot.textContent.labels,
            paragraphs: snapshot.textContent.paragraphs.slice(0, 20),
            list_items: snapshot.textContent.listItems.slice(0, 50),

            // Images
            images: snapshot.images.slice(0, 20),

            // Forms
            forms: snapshot.forms,

            // Stats
            stats: snapshot.stats,

            // Sample of elements (keep size reasonable)
            element_sample: snapshot.elements.slice(0, 100).map((el) => ({
              id: el.id,
              tag: el.tag,
              testId: el.testId,
              selector: el.selector,
              text: el.text,
              innerText: el.innerText?.slice(0, 100),
              visible: el.visibility.isVisible,
              rect: el.rect,
            })),
          },
          task_run_id: taskRunId,
        };

        await sendLog(entry);

        setLastCaptureTime(Date.now());
        setCaptureCount((c) => c + 1);

        console.debug(
          `[RenderLog] Captured snapshot: ${trigger} (${snapshot.stats.captureTimeMs}ms)`,
        );
      } catch (error) {
        console.debug("[RenderLog] Failed to capture snapshot:", error);
      } finally {
        isCapturingRef.current = false;
      }
    },
    [isEnabled, isDev, activeTab, taskRunId, captureConfig, sendLog],
  );

  /**
   * Capture and send a quick summary
   */
  const captureQuick = useCallback(
    async (trigger: string = "quick") => {
      if (!isEnabled || !isDev || isCapturingRef.current) return;

      isCapturingRef.current = true;

      try {
        const summary = captureQuickSummary(activeTab);

        const entry: RenderLogEntry = {
          id: `quick-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
          timestamp: summary.timestamp,
          component: "QuickSummary",
          trigger,
          data: {
            active_tab: summary.activeTab,
            title: summary.title,
            headings: summary.headings,
            button_texts: summary.buttonTexts,
            visible_text: summary.visibleText,
            error_messages: summary.errorMessages,
            loading_indicators: summary.loadingIndicators,
          },
          task_run_id: taskRunId,
        };

        await sendLog(entry);

        setLastCaptureTime(Date.now());
        setCaptureCount((c) => c + 1);
      } catch (error) {
        console.debug("[RenderLog] Failed to capture quick summary:", error);
      } finally {
        isCapturingRef.current = false;
      }
    },
    [isEnabled, isDev, activeTab, taskRunId, sendLog],
  );

  /**
   * Clear the render log
   */
  const clearLog = useCallback(async () => {
    if (!isDev) return;

    try {
      await invoke("clear_render_log");
      setCaptureCount(0);
      setLastCaptureTime(null);
      console.debug("[RenderLog] Log cleared");
    } catch (error) {
      console.debug("[RenderLog] Failed to clear log:", error);
    }
  }, [isDev]);

  /**
   * Handle tab change - capture on route change
   */
  useEffect(() => {
    if (!isEnabled || !isDev) return;

    // Skip if tab hasn't actually changed
    if (lastTabRef.current === activeTab) return;
    lastTabRef.current = activeTab;

    // Delay slightly to let the new tab render
    const timeoutId = setTimeout(() => {
      captureSnapshot("route_change");
    }, 100);

    return () => clearTimeout(timeoutId);
  }, [activeTab, isEnabled, isDev, captureSnapshot]);

  /**
   * Setup MutationObserver for significant DOM changes
   */
  useEffect(() => {
    if (!isEnabled || !isDev || !enableMutationObserver) return;

    // Create observer
    const observer = new MutationObserver((mutations) => {
      // Filter for significant mutations
      const significantMutation = mutations.some((mutation) => {
        // Added/removed nodes
        if (mutation.addedNodes.length > 0 || mutation.removedNodes.length > 0) {
          // Check if any are element nodes (not text)
          for (const node of mutation.addedNodes) {
            if (node.nodeType === Node.ELEMENT_NODE) {
              const el = node as Element;
              // Skip script/style/svg internals
              if (["SCRIPT", "STYLE", "SVG"].includes(el.tagName)) continue;
              // Skip elements marked for no capture
              if (el.hasAttribute("data-no-capture")) continue;
              return true;
            }
          }
          for (const node of mutation.removedNodes) {
            if (node.nodeType === Node.ELEMENT_NODE) {
              return true;
            }
          }
        }

        // Attribute changes on data-* or class
        if (mutation.type === "attributes") {
          const attrName = mutation.attributeName || "";
          if (attrName.startsWith("data-") || attrName === "class") {
            // Only significant class changes (not animation classes)
            if (attrName === "class") {
              const el = mutation.target as Element;
              const classes = el.className || "";
              if (classes.includes("animate-") || classes.includes("transition-")) {
                return false;
              }
            }
            return true;
          }
        }

        return false;
      });

      if (significantMutation) {
        // Debounce the capture
        if (mutationTimeoutRef.current) {
          clearTimeout(mutationTimeoutRef.current);
        }

        mutationTimeoutRef.current = setTimeout(() => {
          captureQuick("mutation");
        }, mutationDebounceMs);
      }
    });

    // Start observing
    observer.observe(document.body, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: ["class", "data-state", "data-selected", "aria-expanded"],
    });

    observerRef.current = observer;

    return () => {
      observer.disconnect();
      observerRef.current = null;
      if (mutationTimeoutRef.current) {
        clearTimeout(mutationTimeoutRef.current);
      }
    };
  }, [isEnabled, isDev, enableMutationObserver, mutationDebounceMs, captureQuick]);

  /**
   * Initial capture on mount (if enabled)
   */
  useEffect(() => {
    if (!isEnabled || !isDev || !enableOnMount) return;

    // Delay to let initial render complete
    const timeoutId = setTimeout(() => {
      captureSnapshot("auto");
    }, 500);

    return () => clearTimeout(timeoutId);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- Intentionally only run on mount, not when captureSnapshot/enableOnMount/isEnabled change
  }, [isDev]);

  const value: RenderLogContextValue = {
    activeTab,
    isEnabled,
    setEnabled,
    captureSnapshot,
    captureQuick,
    clearLog,
    lastCaptureTime,
    captureCount,
  };

  return <RenderLogContext.Provider value={value}>{children}</RenderLogContext.Provider>;
}

// ============================================================================
// Hook
// ============================================================================

export function useRenderLog(): RenderLogContextValue {
  const context = useContext(RenderLogContext);
  if (!context) {
    throw new Error("useRenderLog must be used within a RenderLogProvider");
  }
  return context;
}

/**
 * Optional hook that returns null if not in provider
 */
export function useRenderLogOptional(): RenderLogContextValue | null {
  return useContext(RenderLogContext);
}
