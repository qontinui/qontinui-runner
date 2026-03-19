/**
 * StateExplorerPanel
 *
 * Wrapper panel that wires the orphaned StateDiscoveryPanel and
 * ExplorationConfigDialog into the UI Bridge Integration page.
 * Manages all state and API calls needed by those components.
 */

import { useState, useCallback } from "react";
import { Compass, RefreshCw } from "lucide-react";
import { getApiBase } from "@/lib/runner-api";
import { StateDiscoveryPanel } from "../../components/ui-bridge/StateDiscoveryPanel";
import type { FingerprintDiscoveryResult } from "../../components/ui-bridge/discovery-types";
import {
  ExplorationConfigDialog,
  type ExplorationConfig,
  type ExplorationResult,
} from "../../components/ui-bridge/ExplorationConfigDialog";
import type { CooccurrenceExport } from "../../types/ui-bridge-types";

export function StateExplorerPanel() {
  // Core data
  const [cooccurrenceData, setCooccurrenceData] = useState<CooccurrenceExport | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [sessionActive, setSessionActive] = useState(false);
  const [captureCount, setCaptureCount] = useState(0);

  // Discovery state
  const [discoveryResult, setDiscoveryResult] = useState<FingerprintDiscoveryResult | null>(null);
  const [isRunningDiscovery, setIsRunningDiscovery] = useState(false);

  // Exploration state
  const [isExploring, setIsExploring] = useState(false);
  const [isStopping, setIsStopping] = useState(false);
  const [lastExplorationResult, setLastExplorationResult] = useState<ExplorationResult | null>(
    null,
  );
  const [explorationDialogOpen, setExplorationDialogOpen] = useState(false);
  const [wasStopped, setWasStopped] = useState(false);

  /**
   * Refresh co-occurrence data from the explore results endpoint.
   */
  const onRefresh = useCallback(async (): Promise<CooccurrenceExport | null> => {
    setIsLoading(true);
    try {
      const resp = await fetch(`${getApiBase()}/ui-bridge/explore/results`);
      if (!resp.ok) {
        console.error("Failed to fetch explore results:", resp.status);
        return null;
      }
      const data = await resp.json();
      if (data.success && data.data) {
        const exportData = data.data as CooccurrenceExport;
        setCooccurrenceData(exportData);

        // Update session info from the data
        if (exportData.sessionId) {
          setSessionActive(true);
        }
        if (exportData.presenceMatrix) {
          setCaptureCount(exportData.presenceMatrix.length);
        }

        return exportData;
      }
      return null;
    } catch (err) {
      console.error("Failed to refresh explore results:", err);
      return null;
    } finally {
      setIsLoading(false);
    }
  }, []);

  /**
   * Run fingerprint state discovery on the current co-occurrence data.
   */
  const onRunDiscovery = useCallback(
    async (data: CooccurrenceExport): Promise<FingerprintDiscoveryResult | null> => {
      setIsRunningDiscovery(true);
      try {
        const resp = await fetch(`${getApiBase()}/ui-bridge/discover-states`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(data),
        });
        if (!resp.ok) {
          console.error("Failed to run discovery:", resp.status);
          return null;
        }
        const result = await resp.json();
        if (result.success && result.data) {
          const discovery = result.data as FingerprintDiscoveryResult;
          setDiscoveryResult(discovery);
          return discovery;
        }
        return null;
      } catch (err) {
        console.error("Failed to run state discovery:", err);
        return null;
      } finally {
        setIsRunningDiscovery(false);
      }
    },
    [],
  );

  /**
   * Run automatic exploration. Starts exploration and polls for status
   * until complete.
   */
  const onRunExploration = useCallback(
    async (config: ExplorationConfig): Promise<ExplorationResult | null> => {
      setIsExploring(true);
      setIsStopping(false);
      setWasStopped(false);
      try {
        // Start exploration
        const startResp = await fetch(`${getApiBase()}/ui-bridge/explore`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(config),
        });
        if (!startResp.ok) {
          console.error("Failed to start exploration:", startResp.status);
          return null;
        }

        // Poll for completion
        let result: ExplorationResult | null = null;
        while (true) {
          await new Promise((resolve) => setTimeout(resolve, 1000));

          const statusResp = await fetch(`${getApiBase()}/ui-bridge/explore/status`);
          if (!statusResp.ok) continue;

          const statusData = await statusResp.json();
          if (!statusData.success) continue;

          const status = statusData.data;
          if (status?.complete || status?.stopped) {
            result = {
              explorationId: status.explorationId || crypto.randomUUID(),
              elementsDiscovered: status.elementsDiscovered ?? 0,
              elementsExplored: status.elementsExplored ?? 0,
              errors: status.errors ?? [],
              stateDiscoveryResult: status.stateDiscoveryResult ?? undefined,
            };

            if (status.stopped) {
              setWasStopped(true);
            }
            break;
          }
        }

        setLastExplorationResult(result);

        // If we got state discovery results, update the panel data
        if (result?.stateDiscoveryResult) {
          setDiscoveryResult(result.stateDiscoveryResult);
        }

        // Refresh co-occurrence data after exploration
        await onRefresh();

        return result;
      } catch (err) {
        console.error("Exploration failed:", err);
        return null;
      } finally {
        setIsExploring(false);
      }
    },
    [onRefresh],
  );

  /**
   * Stop a running exploration.
   */
  const onStopExploration = useCallback(async (): Promise<void> => {
    setIsStopping(true);
    try {
      await fetch(`${getApiBase()}/ui-bridge/explore/stop`, {
        method: "POST",
      });
      // The poll loop in onRunExploration will pick up the stopped status
    } catch (err) {
      console.error("Failed to stop exploration:", err);
      setIsStopping(false);
    }
  }, []);

  return (
    <div className="flex flex-col gap-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h3 className="text-sm font-medium">State Explorer</h3>
          <p className="text-xs text-muted-foreground mt-0.5">
            Discover application states through co-occurrence analysis and automatic exploration
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => setExplorationDialogOpen(true)}
            disabled={isExploring}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded
                       bg-cyan-500/10 text-cyan-400 border border-cyan-500/20
                       hover:bg-cyan-500/20 disabled:opacity-50 transition-colors"
          >
            {isExploring ? (
              <RefreshCw className="w-3.5 h-3.5 animate-spin" />
            ) : (
              <Compass className="w-3.5 h-3.5" />
            )}
            {isExploring ? "Exploring..." : "Auto-Explore"}
          </button>
        </div>
      </div>

      {/* State Discovery Panel */}
      <StateDiscoveryPanel
        cooccurrenceData={cooccurrenceData}
        isLoading={isLoading}
        sessionActive={sessionActive}
        captureCount={captureCount}
        onRefresh={onRefresh}
        onRunDiscovery={onRunDiscovery}
        isRunningDiscovery={isRunningDiscovery}
        discoveryResult={discoveryResult}
      />

      {/* Exploration Config Dialog */}
      <ExplorationConfigDialog
        isOpen={explorationDialogOpen}
        onClose={() => setExplorationDialogOpen(false)}
        onRunExploration={onRunExploration}
        onStopExploration={onStopExploration}
        isExploring={isExploring}
        isStopping={isStopping}
        lastResult={lastExplorationResult}
        wasStopped={wasStopped}
      />
    </div>
  );
}
