/**
 * useWebSocketAutoConnect.ts
 *
 * Hook that handles automatic WebSocket connection when:
 * 1. User is authenticated
 * 2. A project is selected
 * 3. Not already connected
 *
 * This hook should be mounted at the App level to ensure it always runs,
 * regardless of which tab is active.
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ConnectionInfo } from "../types/auth";
import { instanceStorage } from "@/lib/instance-storage";
import { createLogger } from "@/lib/logger";

const logger = createLogger("WebSocketAutoConnect");

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

interface WebSocketConfig {
  enabled: boolean;
  url: string;
  token: string;
  project_id: string | null;
  runner_name: string | null;
}

export interface UseWebSocketAutoConnectOptions {
  isAuthenticated: boolean;
  selectedProjectId: string | null;
  runnerName?: string;
  onConnected?: () => void;
  onDisconnected?: () => void;
  onError?: (error: string) => void;
  onLog?: (level: "success" | "error" | "info", message: string) => void;
}

export interface UseWebSocketAutoConnectReturn {
  connected: boolean;
  connecting: boolean;
  error: string | null;
  connectionInfo: ConnectionInfo | null;
  connect: () => Promise<void>;
  disconnect: () => Promise<void>;
}

const RUNNER_NAME_STORAGE_KEY = "qontinui-runner-name";

export function useWebSocketAutoConnect({
  isAuthenticated,
  selectedProjectId,
  runnerName: runnerNameProp,
  onConnected,
  onDisconnected,
  onError,
  onLog,
}: UseWebSocketAutoConnectOptions): UseWebSocketAutoConnectReturn {
  const [connectionInfo, setConnectionInfo] = useState<ConnectionInfo | null>(null);
  const [connected, setConnected] = useState(false);
  const [connecting, setConnecting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Get runner name from prop or localStorage
  const runnerName = runnerNameProp ?? instanceStorage.getItem(RUNNER_NAME_STORAGE_KEY) ?? "";

  // Track if user manually disconnected
  const userDisconnectedRef = useRef(false);
  // Track which project we've auto-connected for
  const autoConnectAttemptedRef = useRef<string | null>(null);
  // Track when the last connect was initiated (to prevent rapid reconnection)
  const lastConnectTimeRef = useRef<number>(0);

  const loadConnectionInfo = async () => {
    try {
      const info = await invoke<ConnectionInfo>("get_connection_info");
      setConnectionInfo(info);
      logger.debug("Connection info loaded:", info);
    } catch (err) {
      console.error("[WS_AUTO_CONNECT] Failed to load connection info:", err);
      setError(err as string);
    }
  };

  // Load connection info when authenticated
  useEffect(() => {
    if (isAuthenticated) {
      loadConnectionInfo();
    } else {
      setConnectionInfo(null);
    }
  }, [isAuthenticated]);

  const connect = useCallback(async () => {
    if (!connectionInfo) {
      setError("Connection information not available");
      return;
    }

    if (!selectedProjectId) {
      setError("No project selected");
      return;
    }

    if (connecting) {
      logger.debug("Connection already in progress, skipping");
      return;
    }

    // Reset user disconnect flag
    userDisconnectedRef.current = false;

    setConnecting(true);
    setError(null);
    lastConnectTimeRef.current = Date.now();

    try {
      logger.debug("Getting access token from AuthManager...");

      let accessToken = "";
      try {
        accessToken = await invoke<string>("get_access_token_for_websocket");
      } catch (tokenErr) {
        console.error("[WS_AUTO_CONNECT] Failed to get access token:", tokenErr);
        throw new Error("Failed to retrieve authentication token. Please try logging in again.", {
          cause: tokenErr,
        });
      }

      logger.debug("Configuring WebSocket with auth token...");

      const config: WebSocketConfig = {
        enabled: true,
        url: connectionInfo.websocket_url,
        token: accessToken,
        project_id: selectedProjectId,
        runner_name: runnerName || null,
      };

      const configResult = await invoke<TauriResult<null>>("configure_websocket", {
        config,
      });

      if (!configResult.success) {
        throw new Error(configResult.message || "Failed to configure WebSocket");
      }

      logger.debug("Connecting...");

      const connectResult = await invoke<TauriResult<null>>("connect_websocket");

      if (!connectResult.success) {
        throw new Error(connectResult.message || "Failed to connect WebSocket");
      }

      setConnected(true);
      onConnected?.();
      onLog?.("success", "Connected to qontinui.io successfully");
      logger.info("Connected successfully");
    } catch (err) {
      const errorMessage = err instanceof Error ? err.message : String(err);
      setError(errorMessage);
      onError?.(errorMessage);
      onLog?.("error", `Connection failed: ${errorMessage}`);
      console.error("[WS_AUTO_CONNECT] Connection failed:", err);
    } finally {
      setConnecting(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- connecting state is checked via ref-like pattern
  }, [connectionInfo, selectedProjectId, runnerName, onConnected, onError, onLog]);

  const disconnect = useCallback(async () => {
    try {
      userDisconnectedRef.current = true;
      logger.info("User initiated disconnect - auto-reconnect disabled");

      const result = await invoke<TauriResult<null>>("disconnect_websocket");
      if (result && result.success) {
        setConnected(false);
        onDisconnected?.();
        onLog?.("success", "Disconnected from qontinui.io");
      } else {
        userDisconnectedRef.current = false;
        onLog?.("error", `Failed to disconnect: ${result?.message || "Unknown error"}`);
      }
    } catch (err) {
      userDisconnectedRef.current = false;
      console.error("[WS_AUTO_CONNECT] Disconnect failed:", err);
      onLog?.("error", `Failed to disconnect: ${err}`);
    }
  }, [onDisconnected, onLog]);

  // Auto-connect when conditions are met
  useEffect(() => {
    // Only auto-connect if:
    // 1. We have a selected project
    // 2. We have connection info
    // 3. Not already connected
    // 4. Not currently connecting
    // 5. Haven't already attempted for this project
    // 6. User didn't manually disconnect
    if (
      selectedProjectId &&
      connectionInfo &&
      !connected &&
      !connecting &&
      autoConnectAttemptedRef.current !== selectedProjectId &&
      !userDisconnectedRef.current
    ) {
      logger.debug("Auto-connecting WebSocket for project:", selectedProjectId);
      autoConnectAttemptedRef.current = selectedProjectId;
      connect();
    }
  }, [selectedProjectId, connectionInfo, connected, connecting, connect]);

  // Reset auto-connect tracking when project changes
  useEffect(() => {
    if (selectedProjectId && autoConnectAttemptedRef.current !== selectedProjectId) {
      // Project changed, reset the user disconnect flag
      userDisconnectedRef.current = false;
    }
  }, [selectedProjectId]);

  // Send heartbeat periodically when connected
  useEffect(() => {
    if (!connected || !selectedProjectId) {
      return;
    }

    let cancelled = false;

    const sendHeartbeat = async () => {
      if (cancelled) return;
      try {
        const response = await invoke<{ has_active_connection: boolean }>("send_device_heartbeat", {
          projectId: selectedProjectId,
        });
        if (cancelled) return;
        logger.debug("Heartbeat sent (has_active_connection:", response.has_active_connection, ")");

        // If backend says no active connection but we think we're connected,
        // the WebSocket has silently dropped — trigger reconnection.
        // Only trigger if we've been connected for at least 15 seconds to avoid
        // a race condition where the heartbeat fires before the backend has
        // registered the WebSocket connection.
        if (!response.has_active_connection) {
          const timeSinceConnect = Date.now() - lastConnectTimeRef.current;
          if (timeSinceConnect < 15000) {
            logger.debug(
              "Backend reports no active connection, but connected recently (" +
                Math.round(timeSinceConnect / 1000) +
                "s ago) — waiting for registration",
            );
          } else {
            console.warn(
              "[WS_AUTO_CONNECT] Backend reports no active connection — triggering reconnect",
            );
            setConnected(false);
            autoConnectAttemptedRef.current = null;
            // The auto-connect effect will pick this up and call connect()
          }
        }
      } catch (err) {
        console.error("[WS_AUTO_CONNECT] Heartbeat failed:", err);
      }
    };

    // Delay the first heartbeat by 10 seconds to allow the WebSocket connection
    // to be fully registered on the backend before checking
    const initialDelay = setTimeout(sendHeartbeat, 10000);

    // Then every 30 seconds
    const intervalId = setInterval(sendHeartbeat, 30000);

    return () => {
      cancelled = true;
      clearTimeout(initialDelay);
      clearInterval(intervalId);
    };
  }, [connected, selectedProjectId]);

  return {
    connected,
    connecting,
    error,
    connectionInfo,
    connect,
    disconnect,
  };
}
