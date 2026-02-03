/**
 * useLiveBrowser Hook
 *
 * Manages live connections to automation targets:
 * - Browser tabs (via Qontinui DevTools extension WebSocket)
 * - Mobile devices (via ADB for screenshots/logcat)
 * - Tauri apps (future - via IPC)
 *
 * Provides element discovery and action execution for Live Browser Mode.
 */

import { useState, useCallback, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";

// =============================================================================
// Types
// =============================================================================

/**
 * Target types for automation
 */
export type TargetType = "browser" | "mobile" | "tauri";

/**
 * Browser tab information from extension
 */
export interface BrowserTab {
  id: number;
  url: string;
  title: string;
  active: boolean;
  windowId: number;
  favIconUrl?: string;
}

/**
 * Mobile device information from ADB
 */
export interface MobileDevice {
  device_id: string;
  device_type: "emulator" | "physical";
  model?: string;
  state: string;
}

/**
 * Discovered element from the page
 */
export interface DiscoveredElement {
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
  parent?: string;
  children?: string[];
  actions: string[];
  hasUiId?: boolean;
}

/**
 * Connection status for a target
 */
export type ConnectionStatus = "disconnected" | "connecting" | "connected" | "error";

/**
 * Currently connected target
 */
export interface ConnectedTarget {
  type: TargetType;
  id: string | number;
  name: string;
  url?: string;
}

/**
 * Return type for the useLiveBrowser hook
 */
export interface UseLiveBrowserReturn {
  // Connection state
  connectionStatus: ConnectionStatus;
  connectedTarget: ConnectedTarget | null;
  error: string | null;

  // Available targets
  browserTabs: BrowserTab[];
  mobileDevices: MobileDevice[];
  isLoadingTargets: boolean;

  // Discovered elements
  elements: DiscoveredElement[];
  isLoadingElements: boolean;
  selectedElementId: string | null;

  // Actions
  refreshTargets: () => Promise<void>;
  connectToTab: (tabId: number) => Promise<void>;
  connectToMobile: (deviceId: string) => Promise<void>;
  disconnect: () => void;
  refreshElements: () => Promise<void>;
  selectElement: (elementId: string | null) => void;
  highlightElement: (elementId: string) => Promise<void>;
  executeAction: (
    elementId: string,
    action: string,
    params?: Record<string, unknown>,
  ) => Promise<void>;
  enablePicker: () => Promise<void>;
  disablePicker: () => void;

  // Extension status
  isExtensionConnected: boolean;
  checkExtensionStatus: () => Promise<boolean>;
}

// =============================================================================
// Hook Implementation
// =============================================================================

const RUNNER_API = "http://localhost:9876";

export function useLiveBrowser(): UseLiveBrowserReturn {
  // Connection state
  const [connectionStatus, setConnectionStatus] = useState<ConnectionStatus>("disconnected");
  const [connectedTarget, setConnectedTarget] = useState<ConnectedTarget | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Targets
  const [browserTabs, setBrowserTabs] = useState<BrowserTab[]>([]);
  const [mobileDevices, setMobileDevices] = useState<MobileDevice[]>([]);
  const [isLoadingTargets, setIsLoadingTargets] = useState(false);

  // Elements
  const [elements, setElements] = useState<DiscoveredElement[]>([]);
  const [isLoadingElements, setIsLoadingElements] = useState(false);
  const [selectedElementId, setSelectedElementId] = useState<string | null>(null);

  // Extension status
  const [isExtensionConnected, setIsExtensionConnected] = useState(false);

  // Picker state
  const _pickerResolveRef = useRef<((element: DiscoveredElement | null) => void) | null>(null);

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
   * Refresh available targets (browser tabs and mobile devices)
   */
  const refreshTargets = useCallback(async () => {
    setIsLoadingTargets(true);
    setError(null);

    try {
      // Fetch browser tabs via WebSocket exploration command
      const tabsResponse = await fetch(`${RUNNER_API}/extension/command`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          action: "listTabs",
          params: {},
        }),
      });

      if (tabsResponse.ok) {
        const tabsData = await tabsResponse.json();
        if (tabsData.success && tabsData.data?.tabs) {
          setBrowserTabs(tabsData.data.tabs);
        }
        setIsExtensionConnected(true);
      } else {
        // Extension might not be connected
        setBrowserTabs([]);
        setIsExtensionConnected(false);
      }
    } catch {
      setBrowserTabs([]);
      setIsExtensionConnected(false);
    }

    try {
      // Fetch mobile devices via Tauri command
      const result = await invoke<{ success: boolean; data?: MobileDevice[] }>(
        "list_mobile_devices",
      );
      if (result.success && result.data) {
        setMobileDevices(result.data);
      } else {
        setMobileDevices([]);
      }
    } catch {
      setMobileDevices([]);
    }

    setIsLoadingTargets(false);
  }, []);

  /**
   * Connect to a browser tab
   */
  const connectToTab = useCallback(async (tabId: number) => {
    console.log("[useLiveBrowser] connectToTab called with tabId:", tabId);
    setConnectionStatus("connecting");
    setError(null);

    try {
      // Select the tab for exploration
      console.log("[useLiveBrowser] Sending selectTab command...");
      const selectResponse = await fetch(`${RUNNER_API}/extension/command`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          action: "selectTab",
          params: { tabId },
        }),
      });

      if (!selectResponse.ok) {
        console.error("[useLiveBrowser] selectTab response not ok:", selectResponse.status);
        throw new Error("Failed to select tab");
      }

      const selectData = await selectResponse.json();
      console.log("[useLiveBrowser] selectTab response:", selectData);
      if (!selectData.success) {
        throw new Error(selectData.error || "Failed to select tab");
      }

      // Connect to verify UI Bridge availability
      console.log("[useLiveBrowser] Sending connect command...");
      const connectResponse = await fetch(`${RUNNER_API}/extension/command`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          action: "connect",
          params: {},
        }),
      });

      if (!connectResponse.ok) {
        console.error("[useLiveBrowser] connect response not ok:", connectResponse.status);
        throw new Error("Failed to connect to tab");
      }

      const connectData = await connectResponse.json();
      console.log("[useLiveBrowser] connect response:", connectData);
      if (!connectData.success) {
        throw new Error(connectData.error || "UI Bridge not available on this page");
      }

      console.log("[useLiveBrowser] Connection successful! Setting connected status.");
      setConnectedTarget({
        type: "browser",
        id: tabId,
        name: selectData.data?.title || `Tab ${tabId}`,
        url: selectData.data?.url,
      });
      setConnectionStatus("connected");

      // Auto-refresh elements
      await refreshElementsInternal();
    } catch (err) {
      const errorMessage = err instanceof Error ? err.message : "Failed to connect to tab";
      console.error("[useLiveBrowser] Connection failed:", errorMessage, err);
      setError(errorMessage);
      setConnectionStatus("error");
      setConnectedTarget(null);
    }
  }, []);

  /**
   * Connect to a mobile device
   */
  const connectToMobile = useCallback(async (deviceId: string) => {
    setConnectionStatus("connecting");
    setError(null);

    try {
      // For mobile, we just verify the device is available
      const result = await invoke<{ success: boolean; data?: MobileDevice[] }>(
        "list_mobile_devices",
      );

      const device = result.data?.find((d) => d.device_id === deviceId);
      if (!device) {
        throw new Error("Device not found");
      }

      if (device.state !== "device") {
        throw new Error(`Device is ${device.state}, not ready`);
      }

      setConnectedTarget({
        type: "mobile",
        id: deviceId,
        name: device.model || deviceId,
      });
      setConnectionStatus("connected");

      // Mobile doesn't have element discovery, clear elements
      setElements([]);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to connect to device");
      setConnectionStatus("error");
      setConnectedTarget(null);
    }
  }, []);

  /**
   * Disconnect from current target
   */
  const disconnect = useCallback(() => {
    // Clear tab selection if browser
    if (connectedTarget?.type === "browser") {
      fetch(`${RUNNER_API}/extension/command`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          action: "clearSelectedTab",
          params: {},
        }),
      }).catch(() => {
        // Ignore errors
      });
    }

    setConnectedTarget(null);
    setConnectionStatus("disconnected");
    setElements([]);
    setSelectedElementId(null);
    setError(null);
  }, [connectedTarget]);

  /**
   * Internal element refresh
   */
  const refreshElementsInternal = async () => {
    setIsLoadingElements(true);

    try {
      const response = await fetch(`${RUNNER_API}/extension/command`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          action: "getElements",
          params: {},
        }),
      });

      if (!response.ok) {
        throw new Error("Failed to get elements");
      }

      const data = await response.json();
      if (data.success && data.data?.elements) {
        setElements(data.data.elements);
      } else {
        setElements([]);
      }
    } catch (err) {
      console.error("[useLiveBrowser] Failed to refresh elements:", err);
      setElements([]);
    } finally {
      setIsLoadingElements(false);
    }
  };

  /**
   * Refresh elements from connected target
   */
  const refreshElements = useCallback(async () => {
    if (connectedTarget?.type !== "browser") {
      return;
    }
    await refreshElementsInternal();
  }, [connectedTarget]);

  /**
   * Select an element
   */
  const selectElement = useCallback((elementId: string | null) => {
    setSelectedElementId(elementId);
  }, []);

  /**
   * Highlight an element in the page
   */
  const highlightElement = useCallback(
    async (elementId: string) => {
      if (connectedTarget?.type !== "browser") {
        return;
      }

      try {
        await fetch(`${RUNNER_API}/extension/command`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            action: "highlightElement",
            params: { elementId },
          }),
        });
      } catch (err) {
        console.error("[useLiveBrowser] Failed to highlight element:", err);
      }
    },
    [connectedTarget],
  );

  /**
   * Execute an action on an element
   */
  const executeAction = useCallback(
    async (elementId: string, action: string, params: Record<string, unknown> = {}) => {
      if (connectedTarget?.type !== "browser") {
        throw new Error("Not connected to a browser tab");
      }

      try {
        const response = await fetch(`${RUNNER_API}/extension/command`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            action: "executeAction",
            params: { elementId, action, params },
          }),
        });

        if (!response.ok) {
          throw new Error("Failed to execute action");
        }

        const data = await response.json();
        if (!data.success) {
          throw new Error(data.error || "Action failed");
        }

        // Refresh elements after action
        await refreshElementsInternal();
      } catch (err) {
        console.error("[useLiveBrowser] Failed to execute action:", err);
        throw err;
      }
    },
    [connectedTarget],
  );

  /**
   * Enable element picker in the page
   */
  const enablePicker = useCallback(async () => {
    if (connectedTarget?.type !== "browser") {
      return;
    }

    try {
      await fetch(`${RUNNER_API}/extension/command`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          action: "enablePicker",
          params: {},
        }),
      });
    } catch (err) {
      console.error("[useLiveBrowser] Failed to enable picker:", err);
    }
  }, [connectedTarget]);

  /**
   * Disable element picker
   */
  const disablePicker = useCallback(() => {
    if (connectedTarget?.type !== "browser") {
      return;
    }

    fetch(`${RUNNER_API}/extension/command`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        action: "disablePicker",
        params: {},
      }),
    }).catch(() => {
      // Ignore errors
    });
  }, [connectedTarget]);

  // Check extension status on mount
  useEffect(() => {
    checkExtensionStatus();
  }, [checkExtensionStatus]);

  return {
    // Connection state
    connectionStatus,
    connectedTarget,
    error,

    // Available targets
    browserTabs,
    mobileDevices,
    isLoadingTargets,

    // Discovered elements
    elements,
    isLoadingElements,
    selectedElementId,

    // Actions
    refreshTargets,
    connectToTab,
    connectToMobile,
    disconnect,
    refreshElements,
    selectElement,
    highlightElement,
    executeAction,
    enablePicker,
    disablePicker,

    // Extension status
    isExtensionConnected,
    checkExtensionStatus,
  };
}
