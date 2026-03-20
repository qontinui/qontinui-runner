/**
 * useConnection — Connection management sub-hook.
 *
 * Manages connection state, multi-connection tracking, connect/disconnect/switch,
 * mount-time hydration, and periodic health checks.
 */

import { useState, useCallback, useEffect } from "react";
import type { ConnectionStatus, ExternalElement } from "./types";
import type { SdkAppInfo } from "./types";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

export interface UseConnectionReturn {
  connectionStatus: ConnectionStatus;
  connectedApp: SdkAppInfo | null;
  error: string | null;
  connect: (url: string, appInfo?: Partial<SdkAppInfo>) => Promise<boolean>;
  disconnect: (url?: string) => Promise<void>;
  connections: Map<string, SdkAppInfo>;
  switchConnection: (url: string) => Promise<boolean>;
  // Setters exposed for cross-hook coordination
  setError: React.Dispatch<React.SetStateAction<string | null>>;
  setConnectionStatus: React.Dispatch<React.SetStateAction<ConnectionStatus>>;
  setConnectedApp: React.Dispatch<React.SetStateAction<SdkAppInfo | null>>;
}

export function useConnection(
  fetchElements: () => Promise<void>,
  clearElementState: () => void,
): UseConnectionReturn {
  const [connectionStatus, setConnectionStatus] = useState<ConnectionStatus>("disconnected");
  const [connectedApp, setConnectedApp] = useState<SdkAppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [connections, setConnections] = useState<Map<string, SdkAppInfo>>(new Map());

  // =========================================================================
  // Connect
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

  // =========================================================================
  // Disconnect
  // =========================================================================

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
    clearElementState();
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
  }, [clearElementState]);

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
      clearElementState();

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
  }, [clearElementState]);

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
            clearElementState();
          }
        }
      } catch {
        // Runner itself is unreachable — don't change state,
        // it might just be temporarily slow
      }
    }, 30000);

    return () => clearInterval(interval);
  }, [connectionStatus, clearElementState]);

  return {
    connectionStatus,
    connectedApp,
    error,
    connect,
    disconnect,
    connections,
    switchConnection,
    setError,
    setConnectionStatus,
    setConnectedApp,
  };
}
