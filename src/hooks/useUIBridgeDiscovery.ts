/**
 * useUIBridgeDiscovery Hook
 *
 * Runner-specific discovery workflow hook. Manages co-occurrence data collection,
 * fingerprint-based state discovery via Tauri IPC, and saving to config.
 *
 * Uses localStorage for persistence across tab switches.
 */

import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { CooccurrenceExport } from "../types/ui-bridge-types";
import type { StateMachineState, StateMachineStateCreate } from "@qontinui/shared-types";
import type { UseStateMachineConfigReturn } from "./useStateMachineConfig";
import type { ShowToastFn } from "./useToast";

// Discovery result from the fingerprint-based discovery Tauri command.
// Must match the Rust structs serialized with #[serde(rename_all = "camelCase")].
export interface FingerprintDiscoveryResult {
  states: Array<{
    stateId: string;
    name: string;
    fingerprintHashes: string[];
    elementIds: string[];
    positionZone: string;
    landmarkContext?: string;
    isGlobal: boolean;
    isModal: boolean;
    repeatPatternCount?: number;
    confidence: number;
    observationCount?: number;
  }>;
  transitions: Array<{
    fromStateId: string;
    toStateId: string;
    actionType: string;
    count: number;
  }>;
  statistics: {
    totalCaptures: number;
    totalTransitions: number;
    uniqueFingerprints: number;
    discoveredStates: number;
    globalStates: number;
    modalStates: number;
    discoveredTransitions: number;
  };
}

// localStorage persistence
const STORAGE_KEY = "qontinui-runner-sm-discovery";

interface PersistedDiscoveryState {
  cooccurrenceData: CooccurrenceExport | null;
  dataSource: "explore" | "record" | null;
  discoveryResult: FingerprintDiscoveryResult | null;
  configName: string;
}

function loadPersistedState(): PersistedDiscoveryState | null {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored) return JSON.parse(stored) as PersistedDiscoveryState;
  } catch {
    // ignore parse errors
  }
  return null;
}

function savePersistedState(state: PersistedDiscoveryState): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  } catch {
    // ignore quota errors
  }
}

function clearPersistedState(): void {
  try {
    localStorage.removeItem(STORAGE_KEY);
  } catch {
    // ignore
  }
}

// Tauri IPC response shape
interface CommandResponse {
  success: boolean;
  message?: string;
  data?: unknown;
}

export function useUIBridgeDiscovery(
  smConfig: UseStateMachineConfigReturn,
  showToast?: ShowToastFn,
) {
  const [initial] = useState(() => loadPersistedState());

  const [cooccurrenceData, setCooccurrenceDataState] = useState<CooccurrenceExport | null>(
    initial?.cooccurrenceData ?? null,
  );
  const [dataSource, setDataSource] = useState<"explore" | "record" | null>(
    initial?.dataSource ?? null,
  );
  const [discoveryResult, setDiscoveryResult] = useState<FingerprintDiscoveryResult | null>(
    initial?.discoveryResult ?? null,
  );
  const [isDiscovering, setIsDiscovering] = useState(false);
  const [configName, setConfigName] = useState(initial?.configName ?? "");
  const [isSaving, setIsSaving] = useState(false);

  // Persist to localStorage when state changes
  useEffect(() => {
    if (!cooccurrenceData && !discoveryResult && !configName) {
      clearPersistedState();
      return;
    }
    savePersistedState({ cooccurrenceData, dataSource, discoveryResult, configName });
  }, [cooccurrenceData, dataSource, discoveryResult, configName]);

  const setCooccurrenceData = useCallback(
    (data: CooccurrenceExport, source: "explore" | "record") => {
      setCooccurrenceDataState(data);
      setDataSource(source);
      setDiscoveryResult(null);
    },
    [],
  );

  const runDiscovery = useCallback(async () => {
    if (!cooccurrenceData) {
      showToast?.("No co-occurrence data to discover states from", "error");
      console.warn("[useUIBridgeDiscovery] No co-occurrence data to discover states from");
      return null;
    }

    setIsDiscovering(true);
    try {
      const response = await invoke<CommandResponse>(
        "ui_bridge_discover_states_from_fingerprints",
        {
          cooccurrenceExport: cooccurrenceData,
        },
      );

      if (!response.success) {
        throw new Error(response.message || "Discovery failed");
      }

      const result = response.data as FingerprintDiscoveryResult;
      setDiscoveryResult(result);
      showToast?.(`Discovered ${result.states.length} states`, "success");
      console.log(`[useUIBridgeDiscovery] Discovered ${result.states.length} states`);
      return result;
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      showToast?.(message, "error");
      console.warn("[useUIBridgeDiscovery]", message);
      return null;
    } finally {
      setIsDiscovering(false);
    }
  }, [cooccurrenceData, showToast]);

  const saveConfig = useCallback(async (): Promise<string | null> => {
    if (!cooccurrenceData) {
      showToast?.("No co-occurrence data available", "error");
      console.warn("[useUIBridgeDiscovery] No co-occurrence data available");
      return null;
    }
    if (!configName.trim()) {
      showToast?.("Config name is required", "error");
      console.warn("[useUIBridgeDiscovery] Config name is required");
      return null;
    }
    if (!discoveryResult) {
      showToast?.("Run discovery first", "error");
      console.warn("[useUIBridgeDiscovery] Run discovery first");
      return null;
    }

    setIsSaving(true);
    try {
      const config = await smConfig.createConfig({
        name: configName.trim(),
        description: `Discovered from ${cooccurrenceData.presenceMatrix.length} captures via ${dataSource || "unknown"}`,
      });

      // Create states via direct Tauri IPC to avoid React state batching issue.
      // createConfig sets activeConfig asynchronously, so calling smConfig.createState
      // immediately would fail because activeConfig hasn't updated yet.
      for (const state of discoveryResult.states) {
        const request: StateMachineStateCreate = {
          state_id: state.stateId,
          name: state.name,
          description: state.landmarkContext || "",
          element_ids: state.elementIds,
          confidence: state.confidence,
        };
        await invoke<StateMachineState>("sm_create_state", {
          configId: config.id,
          request,
        });
      }

      // Reload the full config so the UI reflects all created states
      await smConfig.loadConfig(config.id);

      showToast?.(
        `Saved config "${configName.trim()}" with ${discoveryResult.states.length} states`,
        "success",
      );
      console.log(
        `[useUIBridgeDiscovery] Saved config "${configName.trim()}" with ${discoveryResult.states.length} states`,
      );

      // Clear discovery state
      setCooccurrenceDataState(null);
      setDataSource(null);
      setDiscoveryResult(null);
      setConfigName("");
      clearPersistedState();

      return config.id;
    } catch (err) {
      const message = err instanceof Error ? err.message : "Failed to save";
      showToast?.(message, "error");
      console.warn("[useUIBridgeDiscovery]", message);
      return null;
    } finally {
      setIsSaving(false);
    }
  }, [cooccurrenceData, dataSource, configName, discoveryResult, smConfig, showToast]);

  const reset = useCallback(() => {
    setCooccurrenceDataState(null);
    setDataSource(null);
    setDiscoveryResult(null);
    setConfigName("");
    clearPersistedState();
  }, []);

  return {
    cooccurrenceData,
    dataSource,
    discoveryResult,
    isDiscovering,
    configName,
    setConfigName,
    isSaving,
    setCooccurrenceData,
    runDiscovery,
    saveConfig,
    reset,
  };
}
