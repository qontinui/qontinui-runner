/**
 * useSdkUIBridge Hook
 *
 * Connects to UI Bridge SDK-integrated apps via direct HTTP through the runner.
 * Unlike useExternalUIBridge (legacy extension relay),
 * this communicates directly with SDK apps:
 *
 *   Frontend → POST /ui-bridge/sdk/* → Runner (Rust) → HTTP → SDK App
 *
 * The hook exposes a compatible interface so the ExternalUIBridgeInspector
 * can use either hook interchangeably.
 */

import { useState, useCallback, useRef, useEffect } from "react";
import type {
  ExternalElement,
  ElementFingerprint,
  ConnectionStatus,
  CommandResult,
  PageScreenshot,
  CaptureRecord,
  CaptureSessionStatus,
  CooccurrenceExport,
} from "../types/ui-bridge-types";
import {
  generateFingerprints,
  extractFingerprintHashes,
} from "../lib/ui-bridge/fingerprintGenerator";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { cropThumbnails } from "@/lib/thumbnail-cropper";

/**
 * Module-level storage for capture screenshots that survives React route changes.
 * During self-connect exploration, the runner navigates away from the States page
 * which unmounts all React components. localStorage can't hold screenshots (too large).
 * This module variable persists across route changes since it's in the module scope.
 */
let pendingCaptureScreenshots: CooccurrenceExport["captureScreenshots"] | undefined;

/** Retrieve and consume pending capture screenshots saved during exploration. */
export function consumePendingCaptureScreenshots():
  | CooccurrenceExport["captureScreenshots"]
  | undefined {
  const data = pendingCaptureScreenshots;
  pendingCaptureScreenshots = undefined;
  return data;
}

// =============================================================================
// Types
// =============================================================================

export interface SdkAppInfo {
  appId: string;
  appName: string;
  appType: string;
  framework?: string;
  version?: string;
  capabilities: string[];
  port: number;
}

/** Information about a single SDK connection */
export interface SdkConnectionInfo {
  url: string;
  app: SdkAppInfo;
  connectedAt: number;
  isActive: boolean;
}

interface CommandHistoryEntry {
  id: number;
  timestamp: number;
  action: string;
  params?: Record<string, unknown>;
  result: CommandResult;
}

export interface ExplorationProgress {
  current: number;
  total: number;
  currentElement?: string;
}

export interface UseSdkUIBridgeReturn {
  // Connection
  connectionStatus: ConnectionStatus;
  connectedApp: SdkAppInfo | null;
  error: string | null;
  connect: (url: string, appInfo?: Partial<SdkAppInfo>) => Promise<boolean>;
  disconnect: (url?: string) => Promise<void>;

  // Multi-connection
  connections: Map<string, SdkAppInfo>;
  switchConnection: (url: string) => Promise<boolean>;

  // Elements
  elements: ExternalElement[];
  selectedElementId: string | null;
  selectedElement: ExternalElement | null;
  selectElement: (id: string | null) => void;
  refreshElements: () => Promise<void>;
  isLoadingElements: boolean;

  // Actions
  executeAction: (
    elementId: string,
    action: string,
    params?: Record<string, unknown>,
  ) => Promise<CommandResult>;
  lastCommandResult: CommandResult | null;
  commandHistory: CommandHistoryEntry[];
  clearCommandHistory: () => void;

  // Snapshot
  snapshot: unknown | null;
  refreshSnapshot: () => Promise<void>;

  // Components
  components: unknown[];
  refreshComponents: () => Promise<void>;

  // Raw API (sendCommand for RawApiPanel compatibility)
  sendCommand: <T = unknown>(
    action: string,
    params?: Record<string, unknown>,
  ) => Promise<CommandResult<T>>;

  // AI
  aiSearch: (query: string) => Promise<unknown>;
  aiExecute: (instruction: string) => Promise<unknown>;

  // Screenshot
  pageScreenshot: PageScreenshot | null;
  isCapturingScreenshot: boolean;
  captureScreenshot: (monitor?: number) => Promise<void>;

  // Highlight
  highlightElement: (elementId: string) => Promise<void>;

  // Fingerprinting & Capture Sessions
  captureSession: CaptureSessionStatus;
  startCaptureSession: () => void;
  stopCaptureSession: () => void;
  cooccurrenceData: CooccurrenceExport | null;
  isLoadingCooccurrence: boolean;
  generateCooccurrenceExport: () => Promise<CooccurrenceExport | null>;

  // State exploration
  exploreStates: () => Promise<CooccurrenceExport | null>;
  isExploring: boolean;
  explorationProgress: ExplorationProgress | null;
  cancelExploration: () => void;
}

// =============================================================================
// Constants
// =============================================================================

const MAX_COMMAND_HISTORY = 50;

/** Maximum number of interactions during automated state exploration */
const MAX_EXPLORATION_INTERACTIONS = 50;

/** Delay in ms after each action to let the UI settle */
const EXPLORATION_SETTLE_DELAY = 500;

/**
 * Position zones considered global (header/footer/nav).
 * Elements in these zones are skipped during exploration to avoid
 * navigating away or triggering global actions.
 */
const GLOBAL_POSITION_ZONES = new Set(["header", "footer", "fixed-top", "fixed-bottom"]);

// =============================================================================
// Helpers
// =============================================================================

/** Map an SDK element to the ExternalElement interface used by the inspector UI */
function mapSdkElement(raw: Record<string, unknown>): ExternalElement {
  // Bounds may be at raw.bounds (flat) or raw.state.rect (nested from SDK discover/snapshot)
  const rawState = raw.state as Record<string, unknown> | undefined;
  const rawRect = rawState?.rect as Record<string, number> | undefined;
  const bounds = (raw.bounds as Record<string, number>) || rawRect || {};
  const actions = (raw.actions as string[]) || [];
  const accessibility = raw.accessibility as ExternalElement["accessibility"] | undefined;

  return {
    id: (raw.id as string) || "",
    tagName: (raw.tagName as string) || (raw.type as string) || "div",
    type: (raw.type as string) || (raw.tagName as string) || "unknown",
    bounds: {
      x: bounds.x ?? 0,
      y: bounds.y ?? 0,
      width: bounds.width ?? 0,
      height: bounds.height ?? 0,
      top: bounds.top,
      right: bounds.right,
      bottom: bounds.bottom,
      left: bounds.left,
    },
    visible: (raw.visible as boolean) ?? (rawState?.visible as boolean) ?? true,
    enabled: (raw.enabled as boolean) ?? (rawState?.enabled as boolean) ?? true,
    focused: (raw.focused as boolean) ?? false,
    value: raw.value as string | undefined,
    checked: raw.checked as boolean | undefined,
    text: (raw.label as string) || (raw.text as string) || "",
    label: (raw.label as string) || "",
    parent: (raw.parent as string) || null,
    children: (raw.children as string[]) || [],
    actions,
    accessibility,
    role: accessibility?.role || (raw.role as string),
    accessibleName: accessibility?.accessibleName || (raw.accessibleName as string),
    is_interactive: actions.length > 0,
    selector: raw.selector as string | undefined,
    xpath: raw.xpath as string | undefined,
    classes: raw.classes as string[] | undefined,
    href: raw.href as string | undefined,
    src: raw.src as string | undefined,
  };
}

// =============================================================================
// Hook
// =============================================================================

export function useSdkUIBridge(): UseSdkUIBridgeReturn {
  // Connection state
  const [connectionStatus, setConnectionStatus] = useState<ConnectionStatus>("disconnected");
  const [connectedApp, setConnectedApp] = useState<SdkAppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Multi-connection tracking
  const [connections, setConnections] = useState<Map<string, SdkAppInfo>>(new Map());

  // Elements
  const [elements, setElements] = useState<ExternalElement[]>([]);
  const [selectedElementId, setSelectedElementId] = useState<string | null>(null);
  const [isLoadingElements, setIsLoadingElements] = useState(false);

  // Actions
  const [lastCommandResult, setLastCommandResult] = useState<CommandResult | null>(null);
  const [commandHistory, setCommandHistory] = useState<CommandHistoryEntry[]>([]);
  const commandIdRef = useRef(0);

  // Screenshot
  const [pageScreenshot, setPageScreenshot] = useState<PageScreenshot | null>(null);
  const [isCapturingScreenshot, setIsCapturingScreenshot] = useState(false);

  // Snapshot & components
  const [snapshot, setSnapshot] = useState<unknown | null>(null);
  const [components, setComponents] = useState<unknown[]>([]);

  // Capture session state
  const [captureSession, setCaptureSession] = useState<CaptureSessionStatus>({ active: false });
  const captureSessionRef = useRef<{
    sessionId: string;
    startedAt: number;
    captures: CaptureRecord[];
    fingerprintCatalog: Record<string, ElementFingerprint>;
    lastCaptureId: string | null;
    /** Element bounds keyed by fingerprint hash — used for thumbnail cropping */
    elementBoundsMap?: Record<string, { x: number; y: number; width: number; height: number }>;
    /** Cropped element thumbnails keyed by fingerprint hash */
    elementThumbnails?: Record<string, string>;
    /** Full capture screenshots for screenshot state view (deduplicated by fingerprint set) */
    captureScreenshots?: Array<{
      captureIndex: number;
      screenshotBase64: string;
      width: number;
      height: number;
      elementBoundsJson: string;
      fingerprintHashesJson: string;
      capturedAt: string;
    }>;
    /** Previous capture's fingerprint hash set for deduplication */
    prevCaptureHashes?: Set<string>;
  } | null>(null);
  const [cooccurrenceData, setCooccurrenceData] = useState<CooccurrenceExport | null>(null);
  const [isLoadingCooccurrence, setIsLoadingCooccurrence] = useState(false);

  // State exploration
  const [isExploring, setIsExploring] = useState(false);
  const [explorationProgress, setExplorationProgress] = useState<ExplorationProgress | null>(null);
  const explorationCancelledRef = useRef(false);

  // Derived: selected element
  const selectedElement = selectedElementId
    ? (elements.find((e) => e.id === selectedElementId) ?? null)
    : null;

  // =========================================================================
  // Connection
  // =========================================================================

  const connect = useCallback(
    async (url: string, appInfo?: Partial<SdkAppInfo>): Promise<boolean> => {
      setConnectionStatus("connecting");
      setError(null);

      try {
        const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/connect`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            url,
            appId: appInfo?.appId,
            appName: appInfo?.appName,
            appType: appInfo?.appType,
            framework: appInfo?.framework,
            port: appInfo?.port,
          }),
        });

        const json = await resp.json();
        if (!json.success) {
          setError(json.error || "Connection failed");
          setConnectionStatus("error");
          return false;
        }

        const data = json.data;
        setConnectedApp(data.app);
        setConnectionStatus("connected");

        // Add to connections map
        setConnections((prev) => {
          const next = new Map(prev);
          next.set(url.replace(/\/+$/, ""), data.app);
          return next;
        });

        // Auto-fetch elements on connect
        try {
          await fetchElements();
        } catch {
          // Non-fatal: connection succeeded even if initial element fetch fails
        }

        return true;
      } catch (e) {
        setError(e instanceof Error ? e.message : "Connection failed");
        setConnectionStatus("error");
        return false;
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [],
  );

  const disconnect = useCallback(async (url?: string) => {
    try {
      await tracedFetch(`${getApiBase()}/ui-bridge/sdk/disconnect`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ url: url ?? null }),
      });
    } catch {
      // Best-effort disconnect
    }

    if (url) {
      // Remove specific connection from map
      setConnections((prev) => {
        const next = new Map(prev);
        next.delete(url.replace(/\/+$/, ""));
        return next;
      });
    } else {
      // Disconnecting active — remove it from the map too
      // Fetch updated status to stay in sync
      setConnections((prev) => {
        // We don't know which was active on the client side,
        // so we'll sync from server in the health check
        return prev;
      });
    }

    // Clear UI state (will be repopulated if another connection becomes active)
    setConnectionStatus("disconnected");
    setConnectedApp(null);
    setElements([]);
    setSelectedElementId(null);
    setSnapshot(null);
    setComponents([]);
    setError(null);

    // Check if other connections remain and sync state
    try {
      const statusResp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/status`);
      const statusJson = await statusResp.json();
      if (statusJson.success && statusJson.data) {
        const data = statusJson.data;
        // Rebuild connections map from server
        const newConnections = new Map<string, SdkAppInfo>();
        for (const conn of data.allConnections || []) {
          newConnections.set(conn.url, conn.app);
        }
        setConnections(newConnections);

        if (data.connected && data.app) {
          setConnectedApp(data.app);
          setConnectionStatus("connected");
        }
      }
    } catch {
      // If status check fails, keep disconnected state
    }
  }, []);

  // =========================================================================
  // Switch Connection
  // =========================================================================

  const switchConnection = useCallback(async (url: string): Promise<boolean> => {
    try {
      const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/switch`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ url }),
      });

      const json = await resp.json();
      if (!json.success) {
        setError(json.error || "Switch failed");
        return false;
      }

      const data = json.data;
      setConnectedApp(data.app);
      setConnectionStatus("connected");
      setError(null);

      // Clear element-level state since we switched apps
      setElements([]);
      setSelectedElementId(null);
      setSnapshot(null);
      setComponents([]);

      // Auto-fetch elements for the new active app
      try {
        await fetchElements();
      } catch {
        // Non-fatal
      }

      return true;
    } catch (e) {
      setError(e instanceof Error ? e.message : "Switch failed");
      return false;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // =========================================================================
  // Elements
  // =========================================================================

  const fetchElements = useCallback(async () => {
    setIsLoadingElements(true);
    try {
      const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/elements`);
      const json = await resp.json();

      if (json.success === false) {
        setError(json.error || "Failed to fetch elements");
        return;
      }

      // The response might have elements at different locations depending on SDK
      const rawElements: unknown[] = json.data?.elements || json.elements || json.data || [];

      const mapped = (rawElements as Record<string, unknown>[]).map(mapSdkElement);

      // Generate fingerprints for all elements
      const { catalog } = generateFingerprints(mapped);

      setElements(mapped);
      setError(null);

      // Auto-capture if session is active
      const session = captureSessionRef.current;
      if (session) {
        const hashes = extractFingerprintHashes(mapped);
        const captureId = `cap-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;

        const capture: CaptureRecord = {
          captureId,
          timestamp: Date.now(),
          url: connectedApp?.port ? `http://localhost:${connectedApp.port}` : "unknown",
          title: connectedApp?.appName || "SDK App",
          elementFingerprints: hashes,
          elementCount: mapped.length,
        };

        session.captures.push(capture);
        session.lastCaptureId = captureId;

        // Merge new fingerprints into catalog
        Object.assign(session.fingerprintCatalog, catalog);

        setCaptureSession({
          active: true,
          sessionId: session.sessionId,
          startedAt: session.startedAt,
          captureCount: session.captures.length,
          uniqueFingerprints: Object.keys(session.fingerprintCatalog).length,
        });
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to fetch elements");
    } finally {
      setIsLoadingElements(false);
    }
  }, [connectedApp]);

  const refreshElements = useCallback(async () => {
    await fetchElements();
  }, [fetchElements]);

  const selectElement = useCallback((id: string | null) => {
    setSelectedElementId(id);
  }, []);

  // =========================================================================
  // Actions
  // =========================================================================

  const executeAction = useCallback(
    async (
      elementId: string,
      action: string,
      params?: Record<string, unknown>,
    ): Promise<CommandResult> => {
      const startTime = Date.now();

      // Capture before-state for transition tracking
      const session = captureSessionRef.current;
      const beforeCaptureId = session?.lastCaptureId || null;
      const beforeHashes = session ? new Set(extractFingerprintHashes(elements)) : null;
      const targetFingerprint = elements.find((e) => e.id === elementId)?.fingerprint?.hash || null;

      try {
        const resp = await tracedFetch(
          `${getApiBase()}/ui-bridge/sdk/element/${encodeURIComponent(elementId)}/action`,
          {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ action, params }),
          },
        );

        const json = await resp.json();
        const result: CommandResult = {
          success: json.success !== false,
          data: json.data,
          error: json.error,
          duration: Date.now() - startTime,
        };

        setLastCommandResult(result);
        setCommandHistory((prev) =>
          [
            {
              id: ++commandIdRef.current,
              timestamp: Date.now(),
              action: `${action} on ${elementId}`,
              params,
              result,
            },
            ...prev,
          ].slice(0, MAX_COMMAND_HISTORY),
        );

        // After action, refresh elements to capture the new state
        // This triggers auto-capture if session is active
        if (session && result.success) {
          // Small delay for the UI to update after the action
          await new Promise((r) => setTimeout(r, 500));
          await fetchElements();

          // Record the transition if we have before/after captures
          const afterCaptureId = captureSessionRef.current?.lastCaptureId || null;
          if (
            beforeCaptureId &&
            afterCaptureId &&
            beforeCaptureId !== afterCaptureId &&
            beforeHashes
          ) {
            const afterHashes = new Set(extractFingerprintHashes(elements));
            const appeared = [...afterHashes].filter((h) => !beforeHashes.has(h));
            const disappeared = [...beforeHashes].filter((h) => !afterHashes.has(h));

            // Update the after-capture with transition info
            const afterCapture = session.captures.find((c) => c.captureId === afterCaptureId);
            if (afterCapture) {
              afterCapture.triggeredBy = {
                actionType: action,
                targetFingerprint: targetFingerprint || "",
                previousCaptureId: beforeCaptureId,
              };
            }

            // Log transition for debugging
            if (appeared.length > 0 || disappeared.length > 0) {
              console.log(
                `[useSdkUIBridge] Transition: ${action} on ${elementId} — +${appeared.length} -${disappeared.length} fingerprints`,
              );
            }
          }
        }

        return result;
      } catch (e) {
        const result: CommandResult = {
          success: false,
          error: e instanceof Error ? e.message : "Action failed",
          duration: Date.now() - startTime,
        };
        setLastCommandResult(result);
        return result;
      }
    },
    [elements, fetchElements],
  );

  const clearCommandHistory = useCallback(() => {
    setCommandHistory([]);
    setLastCommandResult(null);
  }, []);

  // =========================================================================
  // Raw API (sendCommand for RawApiPanel compatibility)
  // =========================================================================

  const sendCommand = useCallback(
    async <T = unknown>(
      action: string,
      params: Record<string, unknown> = {},
    ): Promise<CommandResult<T>> => {
      const startTime = Date.now();

      try {
        // Map common actions to SDK endpoints
        let url: string;
        let method: string = "GET";
        let body: unknown = undefined;

        switch (action) {
          case "getElements":
            url = `${getApiBase()}/ui-bridge/sdk/elements`;
            break;
          case "getElement":
            url = `${getApiBase()}/ui-bridge/sdk/element/${params?.elementId || params?.id}`;
            break;
          case "executeAction":
            url = `${getApiBase()}/ui-bridge/sdk/element/${params?.elementId || params?.id}/action`;
            method = "POST";
            body = { action: params?.action, params: params?.actionParams };
            break;
          case "getSnapshot":
            url = `${getApiBase()}/ui-bridge/sdk/snapshot`;
            break;
          case "getComponents":
            url = `${getApiBase()}/ui-bridge/sdk/components`;
            break;
          case "discover":
            url = `${getApiBase()}/ui-bridge/sdk/discover`;
            method = "POST";
            body = params;
            break;
          case "aiSearch":
            url = `${getApiBase()}/ui-bridge/sdk/ai/search`;
            method = "POST";
            body = params;
            break;
          case "aiExecute":
            url = `${getApiBase()}/ui-bridge/sdk/ai/execute`;
            method = "POST";
            body = params;
            break;
          case "getHealth":
            url = `${getApiBase()}/ui-bridge/sdk/health`;
            break;
          case "getMetrics":
            url = `${getApiBase()}/ui-bridge/sdk/debug/metrics`;
            break;
          default:
            // Generic: try as a GET to /ui-bridge/sdk/{action}
            url = `${getApiBase()}/ui-bridge/sdk/${action}`;
            if (params && Object.keys(params).length > 0) {
              method = "POST";
              body = params;
            }
            break;
        }

        const resp = await tracedFetch(url, {
          method,
          headers: method === "POST" ? { "Content-Type": "application/json" } : undefined,
          body: body ? JSON.stringify(body) : undefined,
        });

        const json = await resp.json();
        const duration = Date.now() - startTime;
        const result: CommandResult<T> = {
          success: json.success !== false,
          data: (json.data ?? json) as T,
          error: json.error,
          duration,
        };

        setLastCommandResult(result as CommandResult);
        setCommandHistory((prev) =>
          [
            {
              id: ++commandIdRef.current,
              timestamp: Date.now(),
              action,
              params: Object.keys(params).length > 0 ? params : undefined,
              result: result as CommandResult,
            },
            ...prev,
          ].slice(0, MAX_COMMAND_HISTORY),
        );

        return result;
      } catch (e) {
        const duration = Date.now() - startTime;
        const result: CommandResult<T> = {
          success: false,
          error: e instanceof Error ? e.message : "Command failed",
          duration,
        };

        setLastCommandResult(result as CommandResult);
        setCommandHistory((prev) =>
          [
            {
              id: ++commandIdRef.current,
              timestamp: Date.now(),
              action,
              params: Object.keys(params).length > 0 ? params : undefined,
              result: result as CommandResult,
            },
            ...prev,
          ].slice(0, MAX_COMMAND_HISTORY),
        );

        return result;
      }
    },
    [],
  );

  // =========================================================================
  // Snapshot
  // =========================================================================

  const refreshSnapshot = useCallback(async () => {
    try {
      const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/snapshot`);
      const json = await resp.json();
      if (json.success !== false) {
        setSnapshot(json.data || json);
      }
    } catch {
      // Non-fatal
    }
  }, []);

  // =========================================================================
  // Components
  // =========================================================================

  const refreshComponents = useCallback(async () => {
    try {
      const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/components`);
      const json = await resp.json();
      if (json.success !== false) {
        setComponents(json.data?.components || json.components || json.data || []);
      }
    } catch {
      // Non-fatal
    }
  }, []);

  // =========================================================================
  // AI
  // =========================================================================

  const aiSearch = useCallback(async (query: string): Promise<unknown> => {
    try {
      const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/ai/search`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ query }),
      });
      return await resp.json();
    } catch (e) {
      return { success: false, error: e instanceof Error ? e.message : "AI search failed" };
    }
  }, []);

  const aiExecute = useCallback(async (instruction: string): Promise<unknown> => {
    try {
      const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/ai/execute`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ instruction }),
      });
      return await resp.json();
    } catch (e) {
      return { success: false, error: e instanceof Error ? e.message : "AI execute failed" };
    }
  }, []);

  // =========================================================================
  // Screenshot
  // =========================================================================

  const captureScreenshot = useCallback(async (monitor?: number) => {
    setIsCapturingScreenshot(true);
    try {
      const params = new URLSearchParams();
      if (monitor !== undefined) {
        params.set("monitor", String(monitor));
      }
      const queryString = params.toString();
      const url = `${getApiBase()}/ui-bridge/sdk/screenshot${queryString ? `?${queryString}` : ""}`;

      const resp = await tracedFetch(url);
      const json = await resp.json();

      if (json.success && json.data) {
        setPageScreenshot({
          data: json.data.screenshot,
          capturedAt: Date.now(),
          viewport: {
            width: json.data.width,
            height: json.data.height,
          },
        });
      } else {
        console.warn("[useSdkUIBridge] Screenshot capture failed:", json.error || "Unknown error");
      }
    } catch (e) {
      console.warn(
        "[useSdkUIBridge] Screenshot capture error:",
        e instanceof Error ? e.message : e,
      );
    } finally {
      setIsCapturingScreenshot(false);
    }
  }, []);

  // =========================================================================
  // Highlight
  // =========================================================================

  const highlightElement = useCallback(async (elementId: string) => {
    try {
      await tracedFetch(
        `${getApiBase()}/ui-bridge/sdk/debug/highlight/${encodeURIComponent(elementId)}`,
        {
          method: "POST",
        },
      );
    } catch {
      // Best-effort highlight
    }
  }, []);

  // =========================================================================
  // Capture Sessions (Fingerprint-based state discovery)
  // =========================================================================

  const startCaptureSession = useCallback(() => {
    const sessionId = `sdk-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;

    captureSessionRef.current = {
      sessionId,
      startedAt: Date.now(),
      captures: [],
      fingerprintCatalog: {},
      lastCaptureId: null,
    };

    setCaptureSession({
      active: true,
      sessionId,
      startedAt: Date.now(),
      captureCount: 0,
      uniqueFingerprints: 0,
    });
    setCooccurrenceData(null);

    // Immediately capture current state
    fetchElements();

    console.log(`[useSdkUIBridge] Capture session started: ${sessionId}`);
  }, [fetchElements]);

  const stopCaptureSession = useCallback(() => {
    captureSessionRef.current = null;
    setCaptureSession({ active: false });
    console.log("[useSdkUIBridge] Capture session stopped");
  }, []);

  const generateCooccurrenceExport = useCallback(async (): Promise<CooccurrenceExport | null> => {
    const session = captureSessionRef.current;
    if (!session || session.captures.length === 0) {
      console.warn("[useSdkUIBridge] No capture data to export");
      return null;
    }

    setIsLoadingCooccurrence(true);

    try {
      const { sessionId, captures, fingerprintCatalog } = session;

      // Collect all unique fingerprints
      const allFingerprintsSet = new Set<string>();
      for (const cap of captures) {
        for (const fp of cap.elementFingerprints) {
          allFingerprintsSet.add(fp);
        }
      }
      const allFingerprints = Array.from(allFingerprintsSet).sort();

      // Build presence matrix
      const presenceMatrix = captures.map((cap, index) => ({
        captureId: cap.captureId,
        captureIndex: index,
        timestamp: cap.timestamp,
        url: cap.url,
        title: cap.title,
        fingerprints: [...cap.elementFingerprints].sort(),
      }));

      // Build co-occurrence counts
      const cooccurrenceCounts: Record<string, Record<string, number>> = {};
      for (const capture of captures) {
        const fps = capture.elementFingerprints;
        for (let i = 0; i < fps.length; i++) {
          for (let j = i + 1; j < fps.length; j++) {
            const [fp1, fp2] = [fps[i], fps[j]].sort();
            if (!cooccurrenceCounts[fp1]) cooccurrenceCounts[fp1] = {};
            cooccurrenceCounts[fp1][fp2] = (cooccurrenceCounts[fp1][fp2] || 0) + 1;
          }
        }
      }

      // Build fingerprint statistics
      const fingerprintStats: CooccurrenceExport["fingerprintStats"] = {};
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

      // Build transitions from captures that have triggeredBy
      const transitions: CooccurrenceExport["transitions"] = [];
      for (const cap of captures) {
        if (!cap.triggeredBy) continue;

        const beforeCapture = captures.find(
          (c) => c.captureId === cap.triggeredBy!.previousCaptureId,
        );
        if (!beforeCapture) continue;

        const beforeSet = new Set(beforeCapture.elementFingerprints);
        const afterSet = new Set(cap.elementFingerprints);

        transitions.push({
          actionId: `action-${transitions.length + 1}`,
          actionType: cap.triggeredBy.actionType,
          targetFingerprint: cap.triggeredBy.targetFingerprint,
          beforeCaptureId: cap.triggeredBy.previousCaptureId,
          afterCaptureId: cap.captureId,
          appearedFingerprints: [...afterSet].filter((h) => !beforeSet.has(h)),
          disappearedFingerprints: [...beforeSet].filter((h) => !afterSet.has(h)),
          timestamp: cap.timestamp,
        });
      }

      // Find state candidates (groups of fingerprints that always appear together)
      const stateCandidates: CooccurrenceExport["stateCandidates"] = [];
      const processedGroups = new Set<string>();

      for (const fp1 of allFingerprints) {
        const stats1 = fingerprintStats[fp1];
        if (stats1.totalAppearances < 2) continue;

        const group = [fp1];

        for (const fp2 of allFingerprints) {
          if (fp1 >= fp2) continue;

          const stats2 = fingerprintStats[fp2];
          if (stats2.totalAppearances !== stats1.totalAppearances) continue;

          const [sorted1, sorted2] = [fp1, fp2].sort();
          const coCount = cooccurrenceCounts[sorted1]?.[sorted2] || 0;

          if (coCount === stats1.totalAppearances) {
            group.push(fp2);
          }
        }

        if (group.length > 1) {
          const groupKey = group.sort().join(",");
          if (!processedGroups.has(groupKey)) {
            processedGroups.add(groupKey);
            stateCandidates.push({
              fingerprints: group,
              cooccurrenceRate: 1.0,
              appearanceCount: stats1.totalAppearances,
            });
          }
        }
      }

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
        elementThumbnails: session.elementThumbnails,
        captureScreenshots: session.captureScreenshots,
      };

      console.log(
        `[useSdkUIBridge] Co-occurrence export: ${allFingerprints.length} fingerprints, ${captures.length} captures, ${stateCandidates.length} state candidates`,
      );

      setCooccurrenceData(result);
      return result;
    } catch (err) {
      console.error("[useSdkUIBridge] Failed to generate co-occurrence export:", err);
      return null;
    } finally {
      setIsLoadingCooccurrence(false);
    }
  }, []);

  // =========================================================================
  // State Exploration
  // =========================================================================

  const cancelExploration = useCallback(() => {
    explorationCancelledRef.current = true;
    console.log("[useSdkUIBridge] Exploration cancellation requested");
  }, []);

  /**
   * Automated state exploration.
   *
   * Systematically clicks every interactive element in the main content area,
   * capturing UI state snapshots before and after each interaction. When new
   * elements appear, they are queued for exploration too (breadth-first).
   *
   * Returns the co-occurrence export when done, which can be fed into the
   * state discovery algorithm.
   */
  const exploreStates = useCallback(async (): Promise<CooccurrenceExport | null> => {
    if (isExploring) {
      console.warn("[useSdkUIBridge] Exploration already in progress");
      return null;
    }

    console.log("[useSdkUIBridge] Starting automated state exploration");
    setIsExploring(true);
    explorationCancelledRef.current = false;
    setExplorationProgress({ current: 0, total: 0 });

    // Step 1: Start a capture session if not already active
    const sessionWasActive = captureSessionRef.current !== null;
    if (!sessionWasActive) {
      const sessionId = `explore-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
      captureSessionRef.current = {
        sessionId,
        startedAt: Date.now(),
        captures: [],
        fingerprintCatalog: {},
        lastCaptureId: null,
      };
      setCaptureSession({
        active: true,
        sessionId,
        startedAt: Date.now(),
        captureCount: 0,
        uniqueFingerprints: 0,
      });
      setCooccurrenceData(null);
      console.log(`[useSdkUIBridge] Exploration capture session started: ${sessionId}`);
    }

    try {
      // Step 2: Capture initial state via snapshot (triggers DOM scan, unlike
      // /elements which only returns pre-registered SDK elements)
      await fetchElements();
      await new Promise((r) => setTimeout(r, EXPLORATION_SETTLE_DELAY));

      if (explorationCancelledRef.current) {
        console.log("[useSdkUIBridge] Exploration cancelled before starting interactions");
        return null;
      }

      // Track which element fingerprints we've already interacted with
      // to avoid clicking the same logical element twice (even if its id changes)
      const interactedFingerprints = new Set<string>();
      // Track element ids we've interacted with (fallback when no fingerprint)
      const interactedIds = new Set<string>();
      let interactionCount = 0;

      // We need a way to get the latest elements. Since fetchElements updates
      // the state, and we are inside an async function, we can capture elements
      // from the fetch response directly. Let's create a helper.
      const fetchLatestElements = async (): Promise<ExternalElement[]> => {
        try {
          // Use snapshot endpoint which triggers a full DOM scan, unlike /elements
          // which only returns pre-registered SDK elements (may be 0 for self-connections)
          let resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/snapshot`);
          let json = await resp.json();

          if (json.success === false) {
            // Fall back to elements endpoint
            resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/elements`);
            json = await resp.json();
            if (json.success === false) return [];
          }

          const rawElements: unknown[] = json.data?.elements || json.elements || json.data || [];
          const mapped = (rawElements as Record<string, unknown>[]).map(mapSdkElement);
          const { catalog } = generateFingerprints(mapped);

          // Update hook state
          setElements(mapped);

          // Record capture in session (MUST succeed for transitions to work)
          const session = captureSessionRef.current;
          if (session) {
            const hashes = extractFingerprintHashes(mapped);
            const captureId = `cap-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
            const capture: CaptureRecord = {
              captureId,
              timestamp: Date.now(),
              url: connectedApp?.port ? `http://localhost:${connectedApp.port}` : "unknown",
              title: connectedApp?.appName || "SDK App",
              elementFingerprints: hashes,
              elementCount: mapped.length,
            };
            session.captures.push(capture);
            session.lastCaptureId = captureId;
            Object.assign(session.fingerprintCatalog, catalog);

            // Track element bounds for thumbnail cropping
            if (!session.elementBoundsMap) session.elementBoundsMap = {};
            for (const el of mapped) {
              if (
                el.fingerprint?.hash &&
                el.bounds &&
                el.bounds.width > 0 &&
                el.bounds.height > 0
              ) {
                session.elementBoundsMap[el.fingerprint.hash] = {
                  x: el.bounds.x,
                  y: el.bounds.y,
                  width: el.bounds.width,
                  height: el.bounds.height,
                };
              }
            }

            setCaptureSession({
              active: true,
              sessionId: session.sessionId,
              startedAt: session.startedAt,
              captureCount: session.captures.length,
              uniqueFingerprints: Object.keys(session.fingerprintCatalog).length,
            });

            // Capture thumbnails (non-fatal, in separate try/catch)
            try {
              if (Object.keys(session.elementBoundsMap).length > 0) {
                // Capture the connected app's window so element bounds align with the screenshot.
                // For self-connection (same port), use runner=true (Tauri window capture).
                // For cross-runner connections, target the other window by title.
                const ssParams = new URLSearchParams();
                const isSelfConnection = connectedApp?.port === Number(new URL(getApiBase()).port);
                if (isSelfConnection) {
                  ssParams.set("runner", "true");
                } else if (connectedApp?.appName) {
                  // Try window title match for the other runner instance
                  ssParams.set("window_title", connectedApp.appName);
                }
                const ssUrl = `${getApiBase()}/ui-bridge/sdk/screenshot${ssParams.toString() ? `?${ssParams}` : ""}`;
                const ssResp = await tracedFetch(ssUrl);
                const ssJson = await ssResp.json();
                if (ssJson.success && ssJson.data?.screenshot) {
                  const dpr = ssJson.data.scaleFactor || 1;
                  const existingThumbs = session.elementThumbnails || {};
                  const newElements = Object.entries(session.elementBoundsMap)
                    .filter(([hash]) => !existingThumbs[hash])
                    .map(([hash, bounds]) => ({
                      id: hash,
                      bounds: {
                        x: (bounds as { x: number }).x * dpr,
                        y: (bounds as { y: number }).y * dpr,
                        width: (bounds as { width: number }).width * dpr,
                        height: (bounds as { height: number }).height * dpr,
                      },
                    }));
                  if (newElements.length > 0) {
                    const thumbs = await cropThumbnails(ssJson.data.screenshot, newElements, {
                      maxSize: 48,
                    });
                    if (!session.elementThumbnails) session.elementThumbnails = {};
                    for (const [id, data] of thumbs) {
                      session.elementThumbnails[id] = data;
                    }
                  }

                  // Store full screenshot for screenshot state view (deduplicated)
                  const currentHashes = new Set(Object.keys(session.elementBoundsMap));
                  const prevHashes = session.prevCaptureHashes;
                  const hashesChanged =
                    !prevHashes ||
                    currentHashes.size !== prevHashes.size ||
                    [...currentHashes].some((h) => !prevHashes.has(h));
                  if (hashesChanged) {
                    if (!session.captureScreenshots) session.captureScreenshots = [];
                    // Build element bounds with DPR-scaled coordinates
                    const boundsMap: Record<
                      string,
                      { x: number; y: number; width: number; height: number }
                    > = {};
                    for (const [hash, bounds] of Object.entries(session.elementBoundsMap)) {
                      boundsMap[hash] = {
                        x: (bounds as { x: number }).x * dpr,
                        y: (bounds as { y: number }).y * dpr,
                        width: (bounds as { width: number }).width * dpr,
                        height: (bounds as { height: number }).height * dpr,
                      };
                    }
                    const screenshotEntry = {
                      captureIndex: session.captureScreenshots.length,
                      screenshotBase64: ssJson.data.screenshot,
                      width: Math.round((ssJson.data.width || 1920) * dpr),
                      height: Math.round((ssJson.data.height || 1080) * dpr),
                      elementBoundsJson: JSON.stringify(boundsMap),
                      fingerprintHashesJson: JSON.stringify([...currentHashes]),
                      capturedAt: new Date().toISOString(),
                    };
                    session.captureScreenshots.push(screenshotEntry);
                    session.prevCaptureHashes = currentHashes;
                    // Also persist to module-level variable so screenshots survive
                    // page navigation during self-connect exploration
                    pendingCaptureScreenshots = [...session.captureScreenshots];
                    // Save screenshot to DB immediately with a pending config ID
                    // so it survives process restarts and page navigation
                    import("@tauri-apps/api/core")
                      .then(({ invoke }) => {
                        invoke("sm_save_capture_screenshots", {
                          configId: `pending-${session.sessionId}`,
                          screenshots: [screenshotEntry],
                        }).catch(() => {
                          /* non-fatal */
                        });
                      })
                      .catch(() => {
                        /* non-fatal */
                      });
                  }
                }
              }
            } catch {
              // Thumbnails are optional — don't fail the capture
            }
          }

          return mapped;
        } catch {
          return [];
        }
      };

      const executeExplorationAction = async (
        elementId: string,
        action: string,
      ): Promise<ExternalElement[]> => {
        const session = captureSessionRef.current;
        const beforeCaptureId = session?.lastCaptureId || null;

        try {
          await tracedFetch(
            `${getApiBase()}/ui-bridge/sdk/element/${encodeURIComponent(elementId)}/action`,
            {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({ action, params: {} }),
            },
          );
        } catch {
          // Action failed — still capture current state for co-occurrence
        }

        // Wait for UI to settle, then capture
        await new Promise((r) => setTimeout(r, EXPLORATION_SETTLE_DELAY));

        const afterElements = await fetchLatestElements();

        // Record transition if we have before/after captures
        if (session && beforeCaptureId) {
          const afterCaptureId = session.lastCaptureId;
          if (afterCaptureId && afterCaptureId !== beforeCaptureId) {
            const afterCapture = session.captures.find((c) => c.captureId === afterCaptureId);
            const targetEl = afterElements.find((e) => e.id === elementId) || undefined;
            const targetFingerprint = targetEl?.fingerprint?.hash || "";

            if (afterCapture) {
              afterCapture.triggeredBy = {
                actionType: action,
                targetFingerprint,
                previousCaptureId: beforeCaptureId,
              };
            }
          }
        }

        return afterElements;
      };

      /**
       * Filter elements to only those eligible for exploration.
       */
      const filterExplorable = (els: ExternalElement[]): ExternalElement[] => {
        return els.filter((el) => {
          // Must be interactive
          if (!el.actions || el.actions.length === 0) return false;
          // Must be visible and enabled
          if (!el.visible || !el.enabled) return false;
          // Skip global zones (header, footer, fixed areas)
          const zone = el.fingerprint?.positionZone;
          if (zone && GLOBAL_POSITION_ZONES.has(zone)) return false;
          // Skip already interacted (by fingerprint)
          if (el.fingerprint?.hash && interactedFingerprints.has(el.fingerprint.hash)) return false;
          // Skip already interacted (by id, fallback)
          if (!el.fingerprint?.hash && interactedIds.has(el.id)) return false;
          return true;
        });
      };

      // Get initial explorable elements
      let currentElements = await fetchLatestElements();
      let queue = filterExplorable(currentElements);

      setExplorationProgress({
        current: 0,
        total: queue.length,
      });

      console.log(
        `[useSdkUIBridge] Initial exploration queue: ${queue.length} interactive elements (filtered from ${currentElements.length} total)`,
      );

      // Snapshot the initial fingerprint set to detect "page changes"
      let baselineFingerprints = new Set(extractFingerprintHashes(currentElements));

      // Breadth-first exploration loop
      while (queue.length > 0 && interactionCount < MAX_EXPLORATION_INTERACTIONS) {
        if (explorationCancelledRef.current) {
          console.log(
            `[useSdkUIBridge] Exploration cancelled after ${interactionCount} interactions`,
          );
          break;
        }

        const element = queue.shift()!;
        const elementLabel = element.accessibleName || element.label || element.text || element.id;

        // Mark as interacted
        if (element.fingerprint?.hash) {
          interactedFingerprints.add(element.fingerprint.hash);
        }
        interactedIds.add(element.id);

        interactionCount++;
        setExplorationProgress({
          current: interactionCount,
          total: interactionCount + queue.length,
          currentElement: elementLabel,
        });

        console.log(
          `[useSdkUIBridge] Exploring [${interactionCount}/${interactionCount + queue.length}]: click "${elementLabel}" (${element.id})`,
        );

        // Capture the fingerprint set before the action
        const beforeFingerprints = new Set(extractFingerprintHashes(currentElements));

        // Execute click — creates a capture with triggeredBy for transition tracking
        // and returns the post-action elements (avoids duplicate fetchLatestElements call)
        currentElements = await executeExplorationAction(element.id, "click");
        const afterFingerprints = new Set(extractFingerprintHashes(currentElements));

        // Detect significant change: more than 30% of fingerprints changed
        const appeared = [...afterFingerprints].filter((h) => !beforeFingerprints.has(h));
        const disappeared = [...beforeFingerprints].filter((h) => !afterFingerprints.has(h));
        const totalBefore = beforeFingerprints.size;
        const changeRatio =
          totalBefore > 0 ? (appeared.length + disappeared.length) / totalBefore : 0;

        if (appeared.length > 0 || disappeared.length > 0) {
          console.log(
            `[useSdkUIBridge] State change: +${appeared.length} -${disappeared.length} fingerprints (${(changeRatio * 100).toFixed(1)}% change)`,
          );
        }

        // If significant change (likely navigated to a new "page"), discover new elements
        if (changeRatio > 0.3) {
          console.log(
            "[useSdkUIBridge] Significant state change detected — exploring new elements",
          );

          // Add newly visible interactive elements to the queue
          const newExplorable = filterExplorable(currentElements);
          for (const newEl of newExplorable) {
            // Only add if not already in queue (by id)
            if (!queue.some((q) => q.id === newEl.id)) {
              queue.push(newEl);
            }
          }

          // Update total in progress
          setExplorationProgress({
            current: interactionCount,
            total: interactionCount + queue.length,
            currentElement: elementLabel,
          });

          // Try to navigate back to restore baseline state
          // Attempt: look for a "back" button or similar navigation element
          const backElement = currentElements.find((el) => {
            const name = (el.accessibleName || el.label || el.text || "").toLowerCase();
            return (
              (name.includes("back") ||
                name.includes("close") ||
                name.includes("cancel") ||
                name.includes("return")) &&
              el.actions.length > 0 &&
              el.visible &&
              el.enabled
            );
          });

          if (backElement) {
            console.log(
              `[useSdkUIBridge] Attempting to navigate back via "${backElement.accessibleName || backElement.label || backElement.text || backElement.id}"`,
            );
            currentElements = await executeExplorationAction(backElement.id, "click");

            // Check if we returned to baseline
            const returnedFingerprints = new Set(extractFingerprintHashes(currentElements));
            const returnOverlap = [...baselineFingerprints].filter((h) =>
              returnedFingerprints.has(h),
            ).length;
            const returnRatio =
              baselineFingerprints.size > 0 ? returnOverlap / baselineFingerprints.size : 0;

            if (returnRatio > 0.7) {
              console.log(
                `[useSdkUIBridge] Successfully returned to baseline (${(returnRatio * 100).toFixed(1)}% match)`,
              );
            } else {
              console.log(
                `[useSdkUIBridge] Navigation back resulted in different state (${(returnRatio * 100).toFixed(1)}% baseline match)`,
              );
              // Update baseline since we're in a new state
              baselineFingerprints = new Set(extractFingerprintHashes(currentElements));
            }
          } else {
            // No obvious back button — update baseline to current state
            console.log(
              "[useSdkUIBridge] No back navigation found — updating baseline to current state",
            );
            baselineFingerprints = new Set(extractFingerprintHashes(currentElements));
          }

          // Refresh the queue with current elements
          queue = filterExplorable(currentElements);
        }
      }

      if (interactionCount >= MAX_EXPLORATION_INTERACTIONS) {
        console.log(
          `[useSdkUIBridge] Exploration reached maximum interaction limit (${MAX_EXPLORATION_INTERACTIONS})`,
        );
      }

      console.log(
        `[useSdkUIBridge] Exploration complete: ${interactionCount} interactions, ${captureSessionRef.current?.captures.length ?? 0} captures`,
      );

      // Step 5: Persist thumbnails to dedicated localStorage key
      const thumbs = captureSessionRef.current?.elementThumbnails;
      const thumbCount = Object.keys(thumbs || {}).length;
      if (thumbCount > 0) {
        console.log(
          `[useSdkUIBridge] Captured ${thumbCount} element thumbnails during exploration`,
        );
        try {
          localStorage.setItem("qontinui-runner-sm-thumbnails", JSON.stringify(thumbs));
        } catch {
          /* */
        }
      }

      // Step 5b: Persist capture screenshots to module-level variable
      // (survives route changes unlike React state; too large for localStorage)
      const captureScreenshotsData = captureSessionRef.current?.captureScreenshots;
      if (captureScreenshotsData && captureScreenshotsData.length > 0) {
        pendingCaptureScreenshots = captureScreenshotsData;
        console.log(
          `[useSdkUIBridge] Stored ${captureScreenshotsData.length} capture screenshots for later save`,
        );
      }

      // Step 6: Generate co-occurrence export
      const result = await generateCooccurrenceExport();

      // Step 7: Persist the result to localStorage so it survives page navigation
      // (self-connection exploration causes the runner to navigate away from the
      // State Machine page, unmounting the discovery component)
      if (result) {
        try {
          const { instanceStorage } = await import("@/lib/instance-storage");
          // Strip thumbnails and capture screenshots from the persisted discovery data
          // (they're saved separately to the DB and would make this JSON too large for localStorage)
          const {
            elementThumbnails: _thumbs,
            captureScreenshots: _screenshots,
            ...resultWithoutThumbs
          } = result;
          instanceStorage.setJSON("qontinui-runner-sm-discovery", {
            cooccurrenceData: resultWithoutThumbs,
            dataSource: "explore",
            discoveryResult: null,
            configName: "",
            pendingScreenshotSessionId: captureSessionRef.current?.sessionId,
          });
          console.log("[useSdkUIBridge] Persisted exploration result to localStorage");
        } catch {
          console.warn("[useSdkUIBridge] Failed to persist exploration result");
        }
      }

      // Step 8: Stop capture session if we started it
      if (!sessionWasActive) {
        captureSessionRef.current = null;
        setCaptureSession({ active: false });
        console.log("[useSdkUIBridge] Exploration capture session stopped");
      }

      return result;
    } catch (err) {
      console.error("[useSdkUIBridge] State exploration failed:", err);
      return null;
    } finally {
      setIsExploring(false);
      setExplorationProgress(null);
    }
  }, [isExploring, connectedApp, fetchElements, generateCooccurrenceExport]);

  // =========================================================================
  // Mount-time hydration — sync from backend if already connected
  // =========================================================================

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/status`);
        const json = await resp.json();
        if (cancelled || !json.success || !json.data) return;

        const data = json.data as {
          connected: boolean;
          app?: SdkAppInfo;
          url?: string;
          allConnections?: Array<{
            url: string;
            app: SdkAppInfo;
            connectedAt: number;
            isActive: boolean;
          }>;
        };

        if (data.connected && data.app) {
          setConnectionStatus("connected");
          setConnectedApp(data.app);

          // Hydrate connections map
          if (data.allConnections) {
            setConnections(() => {
              const map = new Map<string, SdkAppInfo>();
              for (const conn of data.allConnections!) {
                map.set(conn.url.replace(/\/+$/, ""), conn.app);
              }
              return map;
            });
          }
        }
      } catch {
        // Runner may not be available yet — leave state as disconnected
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // =========================================================================
  // Periodic Health Check (reconnection / stale cleanup)
  // =========================================================================

  useEffect(() => {
    if (connectionStatus !== "connected") return;

    const interval = setInterval(async () => {
      try {
        const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/check-health`);
        const json = await resp.json();

        if (!json.success || !json.data) return;

        const data = json.data as {
          activeHealthy: boolean;
          activeUrl: string | null;
          results: Array<{ url: string; healthy: boolean; appName: string }>;
          staleRemoved: string[];
          totalConnections: number;
        };

        // Update connections map: remove stale, keep healthy
        if (data.staleRemoved.length > 0) {
          setConnections((prev) => {
            const next = new Map(prev);
            for (const url of data.staleRemoved) {
              next.delete(url);
            }
            return next;
          });
        }

        // If active connection is gone, update status
        if (!data.activeHealthy) {
          if (data.totalConnections > 0) {
            // Other connections exist but active one died
            setConnectionStatus("error");
            setConnectedApp(null);
            setError("Active connection lost. Switch to another connection.");
          } else {
            // No connections remain
            setConnectionStatus("disconnected");
            setConnectedApp(null);
            setElements([]);
            setSnapshot(null);
            setComponents([]);
          }
        }
      } catch {
        // Runner itself is unreachable — don't change state,
        // it might just be temporarily slow
      }
    }, 30000);

    return () => clearInterval(interval);
  }, [connectionStatus]);

  return {
    connectionStatus,
    connectedApp,
    error,
    connect,
    disconnect,
    connections,
    switchConnection,
    elements,
    selectedElementId,
    selectedElement,
    selectElement,
    refreshElements,
    isLoadingElements,
    executeAction,
    sendCommand,
    lastCommandResult,
    commandHistory,
    clearCommandHistory,
    snapshot,
    refreshSnapshot,
    components,
    refreshComponents,
    aiSearch,
    aiExecute,
    pageScreenshot,
    isCapturingScreenshot,
    captureScreenshot,
    highlightElement,
    captureSession,
    startCaptureSession,
    stopCaptureSession,
    cooccurrenceData,
    isLoadingCooccurrence,
    generateCooccurrenceExport,
    exploreStates,
    isExploring,
    explorationProgress,
    cancelExploration,
  };
}
