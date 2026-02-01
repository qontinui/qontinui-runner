/**
 * useExternalUIBridge Hook
 *
 * Connects to an external browser via the UI Bridge HTTP API at port 9876.
 * Used by the UI Bridge Inspector for debugging element discovery and actions.
 *
 * This is distinct from the internal `useUIBridge` hook which connects to
 * the runner's own UI Bridge (for the runner app itself).
 */

import { useState, useCallback, useEffect, useRef } from "react";

// =============================================================================
// Types
// =============================================================================

export interface BrowserTab {
  id: number;
  url: string;
  title: string;
  active: boolean;
  windowId: number;
  favIconUrl?: string;
}

export interface ComputedStyle {
  color?: string;
  backgroundColor?: string;
  fontSize?: string;
  fontWeight?: string;
  fontFamily?: string;
  borderColor?: string;
  borderWidth?: string;
  borderRadius?: string;
  padding?: string;
  margin?: string;
  display?: string;
  position?: string;
  opacity?: string;
  cursor?: string;
  textAlign?: string;
  lineHeight?: string;
  zIndex?: string;
}

export interface RepeatPattern {
  type: string; // 'list' | 'table-row' | 'grid-item' | etc.
  containerTag?: string;
  containerRole?: string;
  index: number;
  count: number;
  itemSelector?: string;
  itemRole?: string;
}

export interface ElementFingerprint {
  // Structural identity (most stable)
  structuralPath: string; // Tag-only path: "nav > ul > li > a"
  positionZone: string; // 'header' | 'footer' | 'sidebar-left' | 'sidebar-right' | 'main' | 'modal' | 'fixed-top' | 'fixed-bottom'
  landmarkContext: string; // Nearest landmark role
  landmarkLabel?: string; // Landmark aria-label if present

  // Semantic identity
  role: string; // ARIA role (explicit or implicit)
  tagName: string;
  accessibleName?: string; // Normalized accessible name

  // Visual identity
  sizeCategory: string; // 'icon' | 'button' | 'small' | 'medium' | 'large' | 'fullwidth' | 'panel'
  relativePosition: {
    top: number; // 0-1 viewport percentage
    left: number;
  };

  // Repeat pattern (for list/grid items)
  isRepeating: boolean;
  repeatPattern?: RepeatPattern;

  // Hash for quick matching
  hash: string;
}

export interface ExternalElement {
  id: string;
  tagName: string;
  type: string;
  bounds: {
    x: number;
    y: number;
    width: number;
    height: number;
    top?: number;
    right?: number;
    bottom?: number;
    left?: number;
  };
  visible: boolean;
  enabled: boolean;
  focused: boolean;
  value?: string;
  checked?: boolean;
  text?: string;
  label?: string;
  parent?: string | null;
  children?: string[];
  actions: string[];
  hasUiId?: boolean;

  // Accessibility object (matches what extension already collects)
  accessibility?: {
    role: string; // ARIA role (explicit or implicit)
    accessibleName?: string; // Computed accessible name
    accessibleDescription?: string; // aria-describedby content
    ariaLabel?: string;
    ariaLabelledBy?: string;
    ariaDescribedBy?: string;
    ariaExpanded?: boolean;
    ariaSelected?: boolean;
    ariaChecked?: boolean | "mixed";
    ariaHidden?: boolean;
    ariaDisabled?: boolean;
    ariaRequired?: boolean;
    ariaLive?: "off" | "polite" | "assertive";
    tabIndex?: number;
    isInTabOrder?: boolean;
    isKeyboardAccessible?: boolean;
    implicitRole?: string;
    hasExplicitRole?: boolean;
  };

  // Top-level convenience properties (flattened from accessibility)
  role?: string; // ARIA role
  accessibleName?: string; // Computed accessible name
  is_expanded?: boolean; // aria-expanded
  is_pressed?: boolean; // aria-pressed
  is_selected?: boolean; // aria-selected
  is_required?: boolean; // required or aria-required
  is_readonly?: boolean; // readonly or aria-readonly
  is_interactive?: boolean; // Can be interacted with
  ref?: string; // Auto-generated reference like @e1, @e2

  // Extraction attributes (for automation/extraction pipeline)
  selector?: string; // CSS selector
  xpath?: string; // XPath expression
  classes?: string[]; // CSS class list
  href?: string; // For links
  src?: string; // For images
  alt?: string; // For images
  title?: string; // Title attribute
  placeholder?: string; // For inputs
  name?: string; // Name attribute
  inputType?: string; // Input type attribute
  formAction?: string; // Form action URL
  formMethod?: string; // Form method
  dataAttributes?: Record<string, string>; // data-* attributes
  computedStyle?: ComputedStyle; // Computed visual styles
  sourceUrl?: string; // Current page URL
  viewportWidth?: number; // Window width
  viewportHeight?: number; // Window height
  tagIndex?: number; // Index among siblings with same tag
  depth?: number; // Depth in DOM tree
  _discovered?: boolean; // Whether this was discovered (vs UI Bridge instrumented)

  // Fingerprint for cross-page element matching
  fingerprint?: ElementFingerprint;
}

export interface PageContext {
  url: string;
  title: string;
  elements: ExternalElement[];
  timestamp: number;
}

export type ConnectionStatus = "disconnected" | "connecting" | "connected" | "error";

export interface CommandResult<T = unknown> {
  success: boolean;
  data?: T;
  error?: string;
  duration?: number;
}

export interface PageScreenshot {
  /** Base64 encoded PNG data (without data URL prefix) */
  data: string;
  /** Timestamp when screenshot was captured */
  capturedAt: number;
  /** Viewport dimensions at capture time */
  viewport: { width: number; height: number };
}

// =============================================================================
// Capture Session Types (for State Machine Discovery)
// =============================================================================

export interface CaptureRecord {
  captureId: string;
  timestamp: number;
  url: string;
  title: string;
  elementFingerprints: string[]; // Array of fingerprint hashes
  elementCount: number;
  triggeredBy?: {
    actionType: string;
    targetFingerprint: string;
    previousCaptureId: string;
  };
}

export interface ActionRecord {
  actionId: string;
  timestamp: number;
  actionType: string;
  targetFingerprint: string;
  beforeCaptureId: string;
  afterCaptureId: string;
  addedFingerprints: string[];
  removedFingerprints: string[];
}

export interface CaptureSessionStatus {
  active: boolean;
  sessionId?: string;
  startedAt?: number;
  captureCount?: number;
  actionCount?: number;
  uniqueFingerprints?: number;
}

export interface CaptureSessionExport {
  sessionId: string;
  startedAt: number;
  endedAt?: number;
  exportedAt?: number;
  captures: CaptureRecord[];
  actions: ActionRecord[];
  fingerprintCatalog: Record<string, ElementFingerprint>;
}

/**
 * Co-occurrence export format optimized for state discovery algorithms.
 * This format makes it easy to:
 * 1. Build co-occurrence matrices
 * 2. Identify element clusters (potential states)
 * 3. Detect transitions between states
 */
export interface CooccurrenceExport {
  /** Session metadata */
  sessionId: string;
  exportedAt: number;

  /** All unique fingerprint hashes seen across all captures */
  allFingerprints: string[];

  /** Full fingerprint details for each hash */
  fingerprintDetails: Record<string, ElementFingerprint>;

  /**
   * Presence matrix: for each capture, which fingerprints are present.
   * Format: { captureId: string, url: string, fingerprints: string[] }
   */
  presenceMatrix: Array<{
    captureId: string;
    captureIndex: number;
    timestamp: number;
    url: string;
    title: string;
    fingerprints: string[];
  }>;

  /**
   * Co-occurrence counts: how often each pair of fingerprints appears together.
   * Format: { [fp1]: { [fp2]: count, ... }, ... }
   * Only includes pairs where fp1 < fp2 (alphabetically) to avoid duplicates.
   */
  cooccurrenceCounts: Record<string, Record<string, number>>;

  /**
   * Fingerprint statistics: for each fingerprint, how often it appears.
   * Field names match Python FingerprintStats dataclass.
   */
  fingerprintStats: Record<string, {
    hash: string;
    totalAppearances: number;
    captureIds: string[];
    firstSeen: number; // capture index
    lastSeen: number; // capture index
  }>;

  /**
   * Transition data: what fingerprints appeared/disappeared by each action.
   * Field names match Python TransitionRecord dataclass.
   */
  transitions: Array<{
    actionId: string;
    actionType: string;
    targetFingerprint: string;
    beforeCaptureId: string;
    afterCaptureId: string;
    appearedFingerprints: string[];
    disappearedFingerprints: string[];
    timestamp: number;
  }>;

  /**
   * Suggested state candidates based on co-occurrence analysis.
   * Elements that always appear together (100% co-occurrence) are grouped.
   */
  stateCandidates: Array<{
    fingerprints: string[];
    cooccurrenceRate: number; // 1.0 = always appear together
    appearanceCount: number; // how many captures contain this group
  }>;
}

export interface UseExternalUIBridgeReturn {
  // Connection state
  connectionStatus: ConnectionStatus;
  isExtensionConnected: boolean;
  connectedTabId: number | null;
  connectedTabInfo: BrowserTab | null;
  error: string | null;

  // Data
  browserTabs: BrowserTab[];
  elements: ExternalElement[];
  pageContext: PageContext | null;

  // Screenshot data
  pageScreenshot: PageScreenshot | null;
  isCapturingScreenshot: boolean;

  // Loading states
  isLoadingTabs: boolean;
  isLoadingElements: boolean;

  // Selection
  selectedElementId: string | null;
  selectedElement: ExternalElement | null;
  selectElement: (elementId: string | null) => void;

  // Actions
  checkExtensionStatus: () => Promise<boolean>;
  refreshTabs: () => Promise<void>;
  connectToTab: (tabId: number) => Promise<void>;
  disconnect: () => void;
  refreshElements: () => Promise<void>;
  capturePageScreenshot: () => Promise<string | null>;
  executeAction: (
    elementId: string,
    action: string,
    params?: Record<string, unknown>
  ) => Promise<CommandResult>;
  highlightElement: (elementId: string) => Promise<void>;
  enablePicker: () => Promise<void>;
  disablePicker: () => void;

  // Raw API access (for Postman-like panel)
  sendCommand: <T = unknown>(
    action: string,
    params?: Record<string, unknown>
  ) => Promise<CommandResult<T>>;
  lastCommandResult: CommandResult | null;
  commandHistory: Array<{
    id: number;
    timestamp: number;
    action: string;
    params?: Record<string, unknown>;
    result: CommandResult;
  }>;
  clearCommandHistory: () => void;

  // Capture Session (for State Machine Discovery)
  captureSession: CaptureSessionStatus;
  lastCapture: CaptureRecord | null;
  cooccurrenceData: CooccurrenceExport | null;
  isLoadingCooccurrence: boolean;
  startCaptureSession: () => Promise<{ sessionId: string; startedAt: number } | null>;
  endCaptureSession: () => Promise<CaptureSessionExport | null>;
  exportCaptureSession: () => Promise<CaptureSessionExport | null>;
  generateCooccurrenceExport: () => Promise<CooccurrenceExport | null>;
}

// =============================================================================
// Constants
// =============================================================================

const RUNNER_API = "http://localhost:9876";
const MAX_COMMAND_HISTORY = 50;

// =============================================================================
// Hook Implementation
// =============================================================================

export function useExternalUIBridge(): UseExternalUIBridgeReturn {
  // Connection state
  const [connectionStatus, setConnectionStatus] = useState<ConnectionStatus>("disconnected");
  const [isExtensionConnected, setIsExtensionConnected] = useState(false);
  const [connectedTabId, setConnectedTabId] = useState<number | null>(null);
  const [connectedTabInfo, setConnectedTabInfo] = useState<BrowserTab | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Data
  const [browserTabs, setBrowserTabs] = useState<BrowserTab[]>([]);
  const [elements, setElements] = useState<ExternalElement[]>([]);
  const [pageContext, setPageContext] = useState<PageContext | null>(null);

  // Loading states
  const [isLoadingTabs, setIsLoadingTabs] = useState(false);
  const [isLoadingElements, setIsLoadingElements] = useState(false);

  // Screenshot state
  const [pageScreenshot, setPageScreenshot] = useState<PageScreenshot | null>(null);
  const [isCapturingScreenshot, setIsCapturingScreenshot] = useState(false);

  // Selection
  const [selectedElementId, setSelectedElementId] = useState<string | null>(null);

  // Command history for raw API panel
  const [lastCommandResult, setLastCommandResult] = useState<CommandResult | null>(null);
  const [commandHistory, setCommandHistory] = useState<
    Array<{
      id: number;
      timestamp: number;
      action: string;
      params?: Record<string, unknown>;
      result: CommandResult;
    }>
  >([]);
  const commandIdRef = useRef(0);

  // Capture session state (for State Machine Discovery)
  const [captureSession, setCaptureSession] = useState<CaptureSessionStatus>({ active: false });
  const [lastCapture, setLastCapture] = useState<CaptureRecord | null>(null);
  const [cooccurrenceData, setCooccurrenceData] = useState<CooccurrenceExport | null>(null);
  const [isLoadingCooccurrence, setIsLoadingCooccurrence] = useState(false);

  // Computed: selected element
  const selectedElement = selectedElementId
    ? elements.find((el) => el.id === selectedElementId) || null
    : null;

  /**
   * Send a command to the UI Bridge HTTP API
   */
  const sendCommand = useCallback(
    async <T = unknown>(
      action: string,
      params: Record<string, unknown> = {}
    ): Promise<CommandResult<T>> => {
      const startTime = Date.now();

      try {
        const response = await fetch(`${RUNNER_API}/extension/command`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ action, params }),
        });

        const duration = Date.now() - startTime;

        if (!response.ok) {
          const result: CommandResult<T> = {
            success: false,
            error: `HTTP ${response.status}: ${response.statusText}`,
            duration,
          };

          // Add to history
          const historyEntry = {
            id: ++commandIdRef.current,
            timestamp: Date.now(),
            action,
            params: Object.keys(params).length > 0 ? params : undefined,
            result,
          };
          setCommandHistory((prev) => [historyEntry, ...prev].slice(0, MAX_COMMAND_HISTORY));
          setLastCommandResult(result);

          return result;
        }

        const data = await response.json();
        const result: CommandResult<T> = {
          success: data.success,
          data: data.data as T,
          error: data.error,
          duration,
        };

        // Add to history
        const historyEntry = {
          id: ++commandIdRef.current,
          timestamp: Date.now(),
          action,
          params: Object.keys(params).length > 0 ? params : undefined,
          result,
        };
        setCommandHistory((prev) => [historyEntry, ...prev].slice(0, MAX_COMMAND_HISTORY));
        setLastCommandResult(result);

        return result;
      } catch (err) {
        const duration = Date.now() - startTime;
        const result: CommandResult<T> = {
          success: false,
          error: err instanceof Error ? err.message : "Network error",
          duration,
        };

        // Add to history
        const historyEntry = {
          id: ++commandIdRef.current,
          timestamp: Date.now(),
          action,
          params: Object.keys(params).length > 0 ? params : undefined,
          result,
        };
        setCommandHistory((prev) => [historyEntry, ...prev].slice(0, MAX_COMMAND_HISTORY));
        setLastCommandResult(result);

        return result;
      }
    },
    []
  );

  /**
   * Clear command history
   */
  const clearCommandHistory = useCallback(() => {
    setCommandHistory([]);
    setLastCommandResult(null);
  }, []);

  /**
   * Check if the extension WebSocket is connected
   */
  const checkExtensionStatus = useCallback(async (): Promise<boolean> => {
    try {
      const response = await fetch(`${RUNNER_API}/extension/status`);
      if (!response.ok) {
        setIsExtensionConnected(false);
        return false;
      }

      const data = await response.json();
      const connected = data.success && data.data?.connected === true;
      setIsExtensionConnected(connected);
      return connected;
    } catch {
      setIsExtensionConnected(false);
      return false;
    }
  }, []);

  /**
   * Refresh available browser tabs
   */
  const refreshTabs = useCallback(async () => {
    setIsLoadingTabs(true);
    setError(null);

    try {
      const result = await sendCommand<{ tabs: BrowserTab[] }>("listTabs");

      if (result.success && result.data?.tabs) {
        setBrowserTabs(result.data.tabs);
        setIsExtensionConnected(true);
      } else {
        setBrowserTabs([]);
        if (!result.success) {
          setIsExtensionConnected(false);
        }
      }
    } catch {
      setBrowserTabs([]);
      setIsExtensionConnected(false);
    } finally {
      setIsLoadingTabs(false);
    }
  }, [sendCommand]);

  /**
   * Connect to a browser tab
   */
  const connectToTab = useCallback(
    async (tabId: number) => {
      setConnectionStatus("connecting");
      setError(null);

      try {
        // Select the tab
        const selectResult = await sendCommand<{ title?: string; url?: string }>("selectTab", {
          tabId,
        });

        if (!selectResult.success) {
          throw new Error(selectResult.error || "Failed to select tab");
        }

        // Connect to verify UI Bridge availability
        const connectResult = await sendCommand("connect");

        if (!connectResult.success) {
          throw new Error(connectResult.error || "UI Bridge not available on this page");
        }

        // Find tab info
        const tabInfo = browserTabs.find((t) => t.id === tabId) || {
          id: tabId,
          url: selectResult.data?.url || "",
          title: selectResult.data?.title || `Tab ${tabId}`,
          active: true,
          windowId: 0,
        };

        setConnectedTabId(tabId);
        setConnectedTabInfo(tabInfo);
        setConnectionStatus("connected");

        // Auto-refresh elements
        await refreshElementsInternal();
      } catch (err) {
        const errorMessage = err instanceof Error ? err.message : "Failed to connect to tab";
        setError(errorMessage);
        setConnectionStatus("error");
        setConnectedTabId(null);
        setConnectedTabInfo(null);
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps -- refreshElementsInternal is stable, including it causes infinite loops
    [browserTabs, sendCommand]
  );

  /**
   * Disconnect from current tab
   */
  const disconnect = useCallback(() => {
    if (connectedTabId !== null) {
      sendCommand("clearSelectedTab").catch(() => {
        // Ignore errors
      });
    }

    setConnectedTabId(null);
    setConnectedTabInfo(null);
    setConnectionStatus("disconnected");
    setElements([]);
    setPageContext(null);
    setPageScreenshot(null);
    setSelectedElementId(null);
    setError(null);
  }, [connectedTabId, sendCommand]);

  /**
   * Internal element refresh (doesn't check connection)
   */
  const refreshElementsInternal = async () => {
    setIsLoadingElements(true);

    try {
      const result = await sendCommand<{ elements: ExternalElement[] }>("getElements");

      if (result.success && result.data?.elements) {
        setElements(result.data.elements);

        // Update page context
        if (connectedTabInfo) {
          setPageContext({
            url: connectedTabInfo.url,
            title: connectedTabInfo.title,
            elements: result.data.elements,
            timestamp: Date.now(),
          });

          // Create a capture record if a session is active
          if (captureSession.active) {
            try {
              const captureResult = await sendCommand<CaptureRecord>("createCapture", {
                url: connectedTabInfo.url,
                title: connectedTabInfo.title,
                elements: result.data.elements,
              });

              if (captureResult.success && captureResult.data) {
                setLastCapture(captureResult.data);

                // Update session status
                const statusResult = await sendCommand<CaptureSessionStatus>("getCaptureSessionStatus");
                if (statusResult.success && statusResult.data) {
                  setCaptureSession(statusResult.data);
                }
              }
            } catch (err) {
              console.error("[useExternalUIBridge] Failed to create capture record:", err);
            }
          }
        }

        // Capture screenshot for thumbnails after elements load
        // Do this asynchronously to not block the element loading
        capturePageScreenshotInternal();
      } else {
        setElements([]);
      }
    } catch {
      setElements([]);
    } finally {
      setIsLoadingElements(false);
    }
  };

  /**
   * Internal screenshot capture (doesn't update loading state for elements)
   */
  const capturePageScreenshotInternal = async () => {
    setIsCapturingScreenshot(true);

    try {
      const result = await sendCommand<{
        screenshot: string;
        capturedAt: number;
        viewport: { width: number; height: number };
      }>("capturePageScreenshot", {});

      if (result.success && result.data?.screenshot) {
        setPageScreenshot({
          data: result.data.screenshot,
          capturedAt: result.data.capturedAt,
          viewport: result.data.viewport,
        });
      }
    } catch (err) {
      console.error("[useExternalUIBridge] Screenshot capture failed:", err);
    } finally {
      setIsCapturingScreenshot(false);
    }
  };

  /**
   * Refresh elements from connected tab
   */
  const refreshElements = useCallback(async () => {
    if (connectionStatus !== "connected") {
      return;
    }
    await refreshElementsInternal();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- refreshElementsInternal is stable
  }, [connectionStatus]);

  /**
   * Select an element
   */
  const selectElement = useCallback((elementId: string | null) => {
    setSelectedElementId(elementId);
  }, []);

  /**
   * Helper to create a capture for action tracking
   * Returns the capture record and the elements
   */
  const createActionCapture = async (triggeredBy?: {
    actionType: string;
    targetFingerprint: string;
    previousCaptureId: string;
  }): Promise<{ capture: CaptureRecord; elements: ExternalElement[] } | null> => {
    if (!connectedTabInfo) return null;

    try {
      // Get current elements
      const elemResult = await sendCommand<{ elements: ExternalElement[] }>("getElements");
      if (!elemResult.success || !elemResult.data?.elements) {
        return null;
      }

      const currentElements = elemResult.data.elements;

      // Create capture record
      const captureResult = await sendCommand<CaptureRecord>("createCapture", {
        url: connectedTabInfo.url,
        title: connectedTabInfo.title,
        elements: currentElements,
        triggeredBy,
      });

      if (captureResult.success && captureResult.data) {
        return {
          capture: captureResult.data,
          elements: currentElements,
        };
      }

      return null;
    } catch (err) {
      console.error("[useExternalUIBridge] Failed to create action capture:", err);
      return null;
    }
  };

  /**
   * Execute an action on an element
   */
  const executeAction = useCallback(
    async (
      elementId: string,
      action: string,
      params: Record<string, unknown> = {}
    ): Promise<CommandResult> => {
      if (connectionStatus !== "connected") {
        return { success: false, error: "Not connected to a browser tab" };
      }

      // If capture session is active, capture before state
      let beforeCaptureId: string | null = null;
      let targetFingerprint: string | null = null;

      if (captureSession.active) {
        // Find the target element's fingerprint
        const targetElement = elements.find(el => el.id === elementId);
        targetFingerprint = targetElement?.fingerprint?.hash || null;

        // Capture before state
        const beforeResult = await createActionCapture();
        if (beforeResult) {
          beforeCaptureId = beforeResult.capture.captureId;
          console.log("[Action] Before capture:", beforeCaptureId);
        }
      }

      // Execute the action
      const result = await sendCommand("executeAction", {
        elementId,
        action,
        params,
      });

      // Refresh elements after action
      if (result.success) {
        // If capture session is active, capture after state and record action
        if (captureSession.active && beforeCaptureId) {
          // Small delay to let the page update
          await new Promise(resolve => setTimeout(resolve, 100));

          // Capture after state
          const afterResult = await createActionCapture({
            actionType: action,
            targetFingerprint: targetFingerprint || elementId,
            previousCaptureId: beforeCaptureId,
          });

          if (afterResult) {
            console.log("[Action] After capture:", afterResult.capture.captureId);

            // Record the action with before/after
            const actionResult = await sendCommand<ActionRecord>("recordAction", {
              actionType: action,
              targetFingerprint: targetFingerprint || elementId,
              beforeCaptureId,
              afterCaptureId: afterResult.capture.captureId,
            });

            if (actionResult.success && actionResult.data) {
              console.log("[Action] Recorded:", action, "on", elementId,
                "added:", actionResult.data.addedFingerprints.length,
                "removed:", actionResult.data.removedFingerprints.length);
            }

            // Update elements and session status
            setElements(afterResult.elements);
            setLastCapture(afterResult.capture);

            // Update session status
            const statusResult = await sendCommand<CaptureSessionStatus>("getCaptureSessionStatus");
            if (statusResult.success && statusResult.data) {
              setCaptureSession(statusResult.data);
            }
          }
        } else {
          // No active session, just refresh normally
          await refreshElementsInternal();
        }
      }

      return result;
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps -- createActionCapture and refreshElementsInternal are stable
    [connectionStatus, sendCommand, captureSession.active, elements, connectedTabInfo]
  );

  /**
   * Highlight an element in the page
   */
  const highlightElement = useCallback(
    async (elementId: string) => {
      if (connectionStatus !== "connected") {
        return;
      }

      await sendCommand("highlightElement", { elementId });
    },
    [connectionStatus, sendCommand]
  );

  /**
   * Enable element picker
   */
  const enablePicker = useCallback(async () => {
    if (connectionStatus !== "connected") {
      return;
    }

    await sendCommand("enablePicker");
  }, [connectionStatus, sendCommand]);

  /**
   * Disable element picker
   */
  const disablePicker = useCallback(() => {
    if (connectionStatus !== "connected") {
      return;
    }

    sendCommand("disablePicker").catch(() => {
      // Ignore errors
    });
  }, [connectionStatus, sendCommand]);

  /**
   * Capture a screenshot of the current page
   */
  const capturePageScreenshot = useCallback(async (): Promise<string | null> => {
    if (connectionStatus !== "connected") {
      return null;
    }

    setIsCapturingScreenshot(true);

    try {
      const result = await sendCommand<{
        screenshot: string;
        capturedAt: number;
        viewport: { width: number; height: number };
      }>("capturePageScreenshot", {});

      if (result.success && result.data?.screenshot) {
        setPageScreenshot({
          data: result.data.screenshot,
          capturedAt: result.data.capturedAt,
          viewport: result.data.viewport,
        });
        return result.data.screenshot;
      }

      return null;
    } catch (err) {
      console.error("[useExternalUIBridge] Screenshot capture failed:", err);
      return null;
    } finally {
      setIsCapturingScreenshot(false);
    }
  }, [connectionStatus, sendCommand]);

  // Check extension status on mount
  useEffect(() => {
    checkExtensionStatus();
  }, [checkExtensionStatus]);

  // Periodic extension status check when disconnected
  useEffect(() => {
    if (connectionStatus === "disconnected") {
      const interval = setInterval(() => {
        checkExtensionStatus();
      }, 5000);
      return () => clearInterval(interval);
    }
  }, [connectionStatus, checkExtensionStatus]);

  // ==========================================================================
  // Capture Session Management (for State Machine Discovery)
  // ==========================================================================

  /**
   * Start a new capture session for state machine discovery
   */
  const startCaptureSession = useCallback(async () => {
    try {
      const result = await sendCommand<{ sessionId: string; startedAt: number }>("startCaptureSession");

      if (result.success && result.data) {
        setCaptureSession({
          active: true,
          sessionId: result.data.sessionId,
          startedAt: result.data.startedAt,
          captureCount: 0,
          actionCount: 0,
          uniqueFingerprints: 0,
        });
        setLastCapture(null);
        setCooccurrenceData(null); // Clear previous session data
        return result.data;
      }

      return null;
    } catch (err) {
      console.error("[useExternalUIBridge] Failed to start capture session:", err);
      return null;
    }
  }, [sendCommand]);

  /**
   * End the current capture session and get all data
   */
  const endCaptureSession = useCallback(async () => {
    try {
      const result = await sendCommand<CaptureSessionExport>("endCaptureSession");

      if (result.success && result.data) {
        setCaptureSession({ active: false });
        setLastCapture(null);
        return result.data;
      }

      return null;
    } catch (err) {
      console.error("[useExternalUIBridge] Failed to end capture session:", err);
      return null;
    }
  }, [sendCommand]);

  /**
   * Export the current capture session without ending it
   */
  const exportCaptureSession = useCallback(async () => {
    try {
      const result = await sendCommand<CaptureSessionExport>("exportCaptureSession");

      if (result.success && result.data) {
        return result.data;
      }

      return null;
    } catch (err) {
      console.error("[useExternalUIBridge] Failed to export capture session:", err);
      return null;
    }
  }, [sendCommand]);

  /**
   * Generate a co-occurrence export optimized for state discovery algorithms.
   * This processes the raw session data into analysis-friendly formats.
   */
  const generateCooccurrenceExport = useCallback(async (): Promise<CooccurrenceExport | null> => {
    setIsLoadingCooccurrence(true);

    try {
      const sessionData = await exportCaptureSession();
      if (!sessionData) {
        console.error("[useExternalUIBridge] No session data for co-occurrence export");
        setIsLoadingCooccurrence(false);
        return null;
      }

      const { sessionId, captures, actions, fingerprintCatalog } = sessionData;

      // Collect all unique fingerprints
      const allFingerprintsSet = new Set<string>();
      captures.forEach(cap => {
        cap.elementFingerprints.forEach(fp => allFingerprintsSet.add(fp));
      });
      const allFingerprints = Array.from(allFingerprintsSet).sort();

      // Build presence matrix
      const presenceMatrix = captures.map((cap, index) => ({
        captureId: cap.captureId,
        captureIndex: index,
        timestamp: cap.timestamp,
        url: cap.url,
        title: cap.title,
        fingerprints: cap.elementFingerprints.sort(),
      }));

      // Build co-occurrence counts
      const cooccurrenceCounts: Record<string, Record<string, number>> = {};
      for (const capture of captures) {
        const fps = capture.elementFingerprints;
        for (let i = 0; i < fps.length; i++) {
          for (let j = i + 1; j < fps.length; j++) {
            const [fp1, fp2] = [fps[i], fps[j]].sort();
            if (!cooccurrenceCounts[fp1]) {
              cooccurrenceCounts[fp1] = {};
            }
            cooccurrenceCounts[fp1][fp2] = (cooccurrenceCounts[fp1][fp2] || 0) + 1;
          }
        }
      }

      // Build fingerprint statistics (matching Python FingerprintStats dataclass)
      const fingerprintStats: Record<string, {
        hash: string;
        totalAppearances: number;
        captureIds: string[];
        firstSeen: number;
        lastSeen: number;
      }> = {};

      for (const fp of allFingerprints) {
        const captureIds: string[] = [];
        let firstSeen = -1;
        let lastSeen = -1;

        captures.forEach((cap, index) => {
          if (cap.elementFingerprints.includes(fp)) {
            captureIds.push(cap.captureId);
            if (firstSeen === -1) firstSeen = index;
            lastSeen = index;
          }
        });

        fingerprintStats[fp] = {
          hash: fp,
          totalAppearances: captureIds.length,
          captureIds,
          firstSeen,
          lastSeen,
        };
      }

      // Build transitions from actions (matching Python TransitionRecord dataclass)
      const transitions = actions.map(action => ({
        actionId: action.actionId,
        actionType: action.actionType,
        targetFingerprint: action.targetFingerprint,
        beforeCaptureId: action.beforeCaptureId,
        afterCaptureId: action.afterCaptureId,
        appearedFingerprints: action.addedFingerprints,
        disappearedFingerprints: action.removedFingerprints,
        timestamp: action.timestamp,
      }));

      // Find state candidates (groups of fingerprints that always appear together)
      const stateCandidates: Array<{
        fingerprints: string[];
        cooccurrenceRate: number;
        appearanceCount: number;
      }> = [];

      // Find fingerprints with 100% co-occurrence
      const processedPairs = new Set<string>();
      for (const fp1 of allFingerprints) {
        const stats1 = fingerprintStats[fp1];
        if (stats1.totalAppearances < 2) continue; // Need at least 2 appearances

        const group = [fp1];

        for (const fp2 of allFingerprints) {
          if (fp1 >= fp2) continue; // Avoid duplicates

          const stats2 = fingerprintStats[fp2];
          if (stats2.totalAppearances !== stats1.totalAppearances) continue;

          // Check if they always appear together
          const [sorted1, sorted2] = [fp1, fp2].sort();
          const coCount = cooccurrenceCounts[sorted1]?.[sorted2] || 0;

          if (coCount === stats1.totalAppearances) {
            // They always appear together
            group.push(fp2);
          }
        }

        if (group.length > 1) {
          const groupKey = group.sort().join(',');
          if (!processedPairs.has(groupKey)) {
            processedPairs.add(groupKey);
            stateCandidates.push({
              fingerprints: group,
              cooccurrenceRate: 1.0,
              appearanceCount: stats1.totalAppearances,
            });
          }
        }
      }

      // Sort state candidates by appearance count (most common first)
      stateCandidates.sort((a, b) => b.appearanceCount - a.appearanceCount);

      const result: CooccurrenceExport = {
        sessionId,
        exportedAt: Date.now(),
        allFingerprints,
        fingerprintDetails: fingerprintCatalog,
        presenceMatrix,
        cooccurrenceCounts,
        fingerprintStats,
        transitions,
        stateCandidates,
      };

      console.log("[useExternalUIBridge] Generated co-occurrence export:",
        allFingerprints.length, "fingerprints,",
        captures.length, "captures,",
        stateCandidates.length, "state candidates");

      setCooccurrenceData(result);
      setIsLoadingCooccurrence(false);
      return result;
    } catch (err) {
      console.error("[useExternalUIBridge] Failed to generate co-occurrence export:", err);
      setIsLoadingCooccurrence(false);
      return null;
    }
  }, [exportCaptureSession]);

  return {
    // Connection state
    connectionStatus,
    isExtensionConnected,
    connectedTabId,
    connectedTabInfo,
    error,

    // Data
    browserTabs,
    elements,
    pageContext,

    // Screenshot data
    pageScreenshot,
    isCapturingScreenshot,

    // Loading states
    isLoadingTabs,
    isLoadingElements,

    // Selection
    selectedElementId,
    selectedElement,
    selectElement,

    // Actions
    checkExtensionStatus,
    refreshTabs,
    connectToTab,
    disconnect,
    refreshElements,
    capturePageScreenshot,
    executeAction,
    highlightElement,
    enablePicker,
    disablePicker,

    // Raw API access
    sendCommand,
    lastCommandResult,
    commandHistory,
    clearCommandHistory,

    // Capture Session (for State Machine Discovery)
    captureSession,
    lastCapture,
    cooccurrenceData,
    isLoadingCooccurrence,
    startCaptureSession,
    endCaptureSession,
    exportCaptureSession,
    generateCooccurrenceExport,
  };
}

export default useExternalUIBridge;
