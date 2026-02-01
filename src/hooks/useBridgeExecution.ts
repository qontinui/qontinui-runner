/**
 * useBridgeExecution Hook
 *
 * Provides functions for running workflows on specific bridges and managing
 * GUI lock transfers between bridges. This enables multi-bridge scenarios
 * where workflows need to run on different bridges or GUI lock ownership
 * needs to be transferred.
 */

import { useCallback, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/**
 * Information about a bridge instance.
 */
export interface BridgeInfo {
  /** Unique bridge identifier */
  bridge_id: string;
  /** Associated task run ID, if any */
  run_id: string | null;
  /** Operating mode: "gui" or "headless" */
  mode: "gui" | "headless";
  /** Monitor indices this bridge can use (GUI mode only) */
  monitor_indices: number[];
  /** Current executor state */
  state: string;
  /** Whether this bridge holds the GUI lock */
  has_gui_lock: boolean;
  /** When the bridge was created (Unix timestamp in ms) */
  created_at: number;
}

/**
 * Response from bridge execution commands.
 */
interface CommandResponse {
  success: boolean;
  message: string | null;
  data: unknown;
}

/**
 * Result of the useBridgeExecution hook.
 */
export interface UseBridgeExecutionResult {
  /**
   * Run a workflow on a specific bridge.
   *
   * @param bridgeId - The ID of the bridge to run the workflow on
   * @param workflowId - The ID of the workflow to execute
   * @param params - Optional additional parameters to pass to the workflow
   * @returns Promise resolving to execution result
   */
  runWorkflowOnBridge: (
    bridgeId: string,
    workflowId: string,
    params?: Record<string, unknown>,
  ) => Promise<{ success: boolean; error?: string }>;

  /**
   * Transfer the GUI lock from one bridge to another.
   *
   * @param fromBridgeId - The ID of the bridge currently holding the lock
   * @param toBridgeId - The ID of the bridge to transfer the lock to
   * @returns Promise resolving to transfer result
   */
  transferGuiLock: (
    fromBridgeId: string,
    toBridgeId: string,
  ) => Promise<{ success: boolean; error?: string }>;

  /**
   * List all available bridges with their current status.
   *
   * @returns Promise resolving to list of bridge info
   */
  listBridges: () => Promise<BridgeInfo[]>;

  /**
   * Get information about a specific bridge.
   *
   * @param bridgeId - The ID of the bridge to get info for
   * @returns Promise resolving to bridge info or null if not found
   */
  getBridgeInfo: (bridgeId: string) => Promise<BridgeInfo | null>;

  /** Whether an operation is currently in progress */
  isLoading: boolean;

  /** Error message from the last operation, if any */
  error: string | null;
}

/**
 * Hook for bridge-specific workflow execution and GUI lock management.
 *
 * @returns Functions and state for bridge execution operations
 *
 * @example
 * ```tsx
 * function BridgeSelector() {
 *   const { listBridges, runWorkflowOnBridge, transferGuiLock, isLoading } = useBridgeExecution();
 *   const [bridges, setBridges] = useState<BridgeInfo[]>([]);
 *
 *   useEffect(() => {
 *     listBridges().then(setBridges);
 *   }, [listBridges]);
 *
 *   const handleRunOnBridge = async (bridgeId: string, workflowId: string) => {
 *     const result = await runWorkflowOnBridge(bridgeId, workflowId);
 *     if (!result.success) {
 *       console.error('Failed to run workflow:', result.error);
 *     }
 *   };
 *
 *   return (
 *     <div>
 *       {bridges.map(bridge => (
 *         <div key={bridge.bridge_id}>
 *           <span>{bridge.bridge_id} ({bridge.mode})</span>
 *           {bridge.has_gui_lock && <span>Has GUI Lock</span>}
 *         </div>
 *       ))}
 *     </div>
 *   );
 * }
 * ```
 */
export function useBridgeExecution(): UseBridgeExecutionResult {
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  /**
   * Run a workflow on a specific bridge.
   */
  const runWorkflowOnBridge = useCallback(
    async (
      bridgeId: string,
      workflowId: string,
      params?: Record<string, unknown>,
    ): Promise<{ success: boolean; error?: string }> => {
      setIsLoading(true);
      setError(null);

      try {
        const response = await invoke<CommandResponse>("run_workflow_on_bridge", {
          bridgeId,
          workflowId,
          params: params ?? null,
        });

        if (response.success) {
          return { success: true };
        } else {
          const errorMsg = response.message || "Failed to run workflow on bridge";
          setError(errorMsg);
          return { success: false, error: errorMsg };
        }
      } catch (err) {
        const errorMsg = err instanceof Error ? err.message : String(err);
        setError(errorMsg);
        return { success: false, error: errorMsg };
      } finally {
        setIsLoading(false);
      }
    },
    [],
  );

  /**
   * Transfer the GUI lock from one bridge to another.
   */
  const transferGuiLock = useCallback(
    async (
      fromBridgeId: string,
      toBridgeId: string,
    ): Promise<{ success: boolean; error?: string }> => {
      setIsLoading(true);
      setError(null);

      try {
        const response = await invoke<CommandResponse>("transfer_gui_lock", {
          fromBridgeId,
          toBridgeId,
        });

        if (response.success) {
          return { success: true };
        } else {
          const errorMsg = response.message || "Failed to transfer GUI lock";
          setError(errorMsg);
          return { success: false, error: errorMsg };
        }
      } catch (err) {
        const errorMsg = err instanceof Error ? err.message : String(err);
        setError(errorMsg);
        return { success: false, error: errorMsg };
      } finally {
        setIsLoading(false);
      }
    },
    [],
  );

  /**
   * List all available bridges.
   */
  const listBridges = useCallback(async (): Promise<BridgeInfo[]> => {
    setIsLoading(true);
    setError(null);

    try {
      const response = await invoke<CommandResponse>("list_bridges");

      if (response.success && response.data) {
        const data = response.data as { bridges: BridgeInfo[] };
        return data.bridges || [];
      }
      return [];
    } catch (err) {
      const errorMsg = err instanceof Error ? err.message : String(err);
      setError(errorMsg);
      return [];
    } finally {
      setIsLoading(false);
    }
  }, []);

  /**
   * Get information about a specific bridge.
   */
  const getBridgeInfo = useCallback(async (bridgeId: string): Promise<BridgeInfo | null> => {
    setIsLoading(true);
    setError(null);

    try {
      const response = await invoke<CommandResponse>("get_bridge_info", {
        bridgeId,
      });

      if (response.success && response.data) {
        return response.data as BridgeInfo;
      }
      return null;
    } catch (err) {
      const errorMsg = err instanceof Error ? err.message : String(err);
      setError(errorMsg);
      return null;
    } finally {
      setIsLoading(false);
    }
  }, []);

  return {
    runWorkflowOnBridge,
    transferGuiLock,
    listBridges,
    getBridgeInfo,
    isLoading,
    error,
  };
}

export default useBridgeExecution;
