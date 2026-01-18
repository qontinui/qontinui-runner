/**
 * useAccessibilityTree
 *
 * TanStack Query hook for accessibility tree capture.
 * Provides data fetching with caching, mutation for capture, and connection management.
 */

import { useState, useCallback } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { accessibilityService } from "../services/accessibility-service";
import type {
  AccessibilitySnapshot,
  AccessibilityCaptureOptions,
  CDPTarget,
} from "../types/accessibility";

// Query keys for react-query cache
export const accessibilityKeys = {
  all: ["accessibility"] as const,
  snapshot: () => [...accessibilityKeys.all, "snapshot"] as const,
  aiContext: () => [...accessibilityKeys.all, "aiContext"] as const,
  cdpPorts: () => [...accessibilityKeys.all, "cdpPorts"] as const,
  browserTargets: (port: number) => [...accessibilityKeys.all, "browserTargets", port] as const,
};

/**
 * Connection state for CDP
 */
export interface CDPConnectionState {
  isConnected: boolean;
  port?: number;
  host?: string;
  target?: CDPTarget;
}

/**
 * Result of the useAccessibilityTree hook.
 */
export interface UseAccessibilityTreeResult {
  // Snapshot data
  snapshot: AccessibilitySnapshot | null;
  aiContext: string | null;
  isLoading: boolean;
  error: string | null;

  // Connection state
  connection: CDPConnectionState;
  availablePorts: number[];
  isScanning: boolean;

  // Actions
  captureTree: (options?: AccessibilityCaptureOptions) => Promise<void>;
  autoConnect: () => Promise<void>;
  disconnect: () => Promise<void>;
  scanPorts: () => Promise<void>;
  selectTarget: (port: number, target?: CDPTarget) => void;

  // Ref actions
  clickRef: (ref: string) => Promise<{ success: boolean; error?: string }>;
  fillRef: (
    ref: string,
    value: string,
    clearFirst?: boolean,
  ) => Promise<{ success: boolean; error?: string }>;
  focusRef: (ref: string) => Promise<{ success: boolean; error?: string }>;
}

/**
 * Hook for accessibility tree capture and interaction.
 */
export function useAccessibilityTree(): UseAccessibilityTreeResult {
  const queryClient = useQueryClient();

  // Local state for connection
  const [connection, setConnection] = useState<CDPConnectionState>({
    isConnected: false,
  });
  const [error, setError] = useState<string | null>(null);

  // Query for current snapshot
  const snapshotQuery = useQuery({
    queryKey: accessibilityKeys.snapshot(),
    queryFn: async () => {
      const result = await accessibilityService.getCurrentSnapshot();
      if (result.success && result.snapshot) {
        return result.snapshot;
      }
      return null;
    },
    enabled: connection.isConnected,
    staleTime: 30000, // 30 seconds
  });

  // Query for AI context
  const aiContextQuery = useQuery({
    queryKey: accessibilityKeys.aiContext(),
    queryFn: async () => {
      const result = await accessibilityService.getAIContext();
      if (result.success && result.ai_context) {
        return result.ai_context;
      }
      return null;
    },
    enabled: connection.isConnected && !!snapshotQuery.data,
    staleTime: 30000,
  });

  // Query for CDP ports
  const portsQuery = useQuery({
    queryKey: accessibilityKeys.cdpPorts(),
    queryFn: async () => {
      const result = await accessibilityService.scanCDPPorts();
      if (result.success && result.available_ports) {
        return result.available_ports;
      }
      return [];
    },
    staleTime: 60000, // 1 minute
  });

  // Mutation for capturing tree
  const captureMutation = useMutation({
    mutationFn: async (options: AccessibilityCaptureOptions = {}) => {
      return accessibilityService.captureTree(options);
    },
    onSuccess: (result) => {
      if (result.success) {
        setConnection((prev) => ({
          ...prev,
          isConnected: true,
        }));
        setError(null);
        // Invalidate queries to refresh data
        queryClient.invalidateQueries({ queryKey: accessibilityKeys.snapshot() });
        queryClient.invalidateQueries({ queryKey: accessibilityKeys.aiContext() });
      } else {
        setError(result.error || "Failed to capture accessibility tree");
      }
    },
    onError: (err) => {
      setError(err instanceof Error ? err.message : String(err));
    },
  });

  // Mutation for auto-connect
  const autoConnectMutation = useMutation({
    mutationFn: async () => {
      return accessibilityService.autoConnect();
    },
    onSuccess: (result) => {
      if (result.success) {
        setConnection({
          isConnected: true,
          port: result.port,
          host: "localhost",
          target: result.target,
        });
        setError(null);
        // Invalidate queries to refresh data
        queryClient.invalidateQueries({ queryKey: accessibilityKeys.snapshot() });
        queryClient.invalidateQueries({ queryKey: accessibilityKeys.aiContext() });
      } else {
        setError(result.error || "Failed to auto-connect");
      }
    },
    onError: (err) => {
      setError(err instanceof Error ? err.message : String(err));
    },
  });

  // Mutation for disconnect
  const disconnectMutation = useMutation({
    mutationFn: async () => {
      return accessibilityService.disconnect();
    },
    onSuccess: () => {
      setConnection({ isConnected: false });
      // Clear cached data
      queryClient.setQueryData(accessibilityKeys.snapshot(), null);
      queryClient.setQueryData(accessibilityKeys.aiContext(), null);
    },
  });

  // Action handlers
  const captureTree = useCallback(
    async (options: AccessibilityCaptureOptions = {}) => {
      await captureMutation.mutateAsync(options);
    },
    [captureMutation],
  );

  const autoConnect = useCallback(async () => {
    await autoConnectMutation.mutateAsync();
  }, [autoConnectMutation]);

  const disconnect = useCallback(async () => {
    await disconnectMutation.mutateAsync();
  }, [disconnectMutation]);

  const scanPorts = useCallback(async () => {
    await queryClient.invalidateQueries({ queryKey: accessibilityKeys.cdpPorts() });
  }, [queryClient]);

  const selectTarget = useCallback((port: number, target?: CDPTarget) => {
    setConnection((prev) => ({
      ...prev,
      port,
      target,
    }));
  }, []);

  // Ref action handlers
  const clickRef = useCallback(async (ref: string) => {
    try {
      const result = await accessibilityService.clickRef(ref);
      return { success: result.success, error: result.error };
    } catch (err) {
      return {
        success: false,
        error: err instanceof Error ? err.message : String(err),
      };
    }
  }, []);

  const fillRef = useCallback(async (ref: string, value: string, clearFirst = false) => {
    try {
      const result = await accessibilityService.fillRef(ref, value, clearFirst);
      return { success: result.success, error: result.error };
    } catch (err) {
      return {
        success: false,
        error: err instanceof Error ? err.message : String(err),
      };
    }
  }, []);

  const focusRef = useCallback(async (ref: string) => {
    try {
      const result = await accessibilityService.focusRef(ref);
      return { success: result.success, error: result.error };
    } catch (err) {
      return {
        success: false,
        error: err instanceof Error ? err.message : String(err),
      };
    }
  }, []);

  return {
    // Data
    snapshot: snapshotQuery.data ?? null,
    aiContext: aiContextQuery.data ?? null,
    isLoading:
      captureMutation.isPending || autoConnectMutation.isPending || snapshotQuery.isLoading,
    error,

    // Connection
    connection,
    availablePorts: portsQuery.data ?? [],
    isScanning: portsQuery.isLoading,

    // Actions
    captureTree,
    autoConnect,
    disconnect,
    scanPorts,
    selectTarget,

    // Ref actions
    clickRef,
    fillRef,
    focusRef,
  };
}

export default useAccessibilityTree;
