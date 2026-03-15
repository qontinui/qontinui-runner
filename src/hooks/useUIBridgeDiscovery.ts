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
import { instanceStorage } from "@/lib/instance-storage";

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
    const state = instanceStorage.getJSON<PersistedDiscoveryState | null>(STORAGE_KEY, null);
    // Validate the persisted data — reject if critical fields are missing
    if (state?.cooccurrenceData) {
      const data = state.cooccurrenceData;
      if (!Array.isArray(data.allFingerprints) || !Array.isArray(data.presenceMatrix)) {
        console.warn("[useUIBridgeDiscovery] Clearing stale/invalid persisted co-occurrence data");
        instanceStorage.removeItem(STORAGE_KEY);
        return null;
      }
    }
    if (state?.discoveryResult) {
      const result = state.discoveryResult;
      if (!Array.isArray(result.states) || !result.statistics) {
        console.warn("[useUIBridgeDiscovery] Clearing stale/invalid persisted discovery result");
        instanceStorage.removeItem(STORAGE_KEY);
        return null;
      }
    }
    return state;
  } catch {
    instanceStorage.removeItem(STORAGE_KEY);
    return null;
  }
}

function savePersistedState(state: PersistedDiscoveryState): void {
  instanceStorage.setJSON(STORAGE_KEY, state);
}

function clearPersistedState(): void {
  instanceStorage.removeItem(STORAGE_KEY);
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
      // Run fingerprint-based state discovery via Rust backend
      let result: FingerprintDiscoveryResult;
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
        result = response.data as FingerprintDiscoveryResult;
      } catch (ipcErr) {
        // Fallback: run discovery in JS when Rust backend is unavailable
        console.warn(
          "[useUIBridgeDiscovery] Rust discovery failed, running JS-based fallback:",
          ipcErr,
        );
        result = discoverStatesFromFingerprints(cooccurrenceData);
      }

      setDiscoveryResult(result);
      showToast?.(`Discovered ${result.states.length} states`, "success");
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

  const saveConfig = useCallback(
    async (
      nameOverride?: string,
      discoveryResultOverride?: FingerprintDiscoveryResult,
    ): Promise<string | null> => {
      if (!cooccurrenceData) {
        showToast?.("No co-occurrence data available", "error");
        return null;
      }
      const saveName = nameOverride || configName;
      if (!saveName.trim()) {
        showToast?.("Config name is required", "error");
        return null;
      }
      const result = discoveryResultOverride || discoveryResult;
      if (!result) {
        showToast?.("Run discovery first", "error");
        return null;
      }

      setIsSaving(true);
      try {
        const config = await smConfig.createConfig({
          name: saveName.trim(),
          description: `Discovered from ${cooccurrenceData.presenceMatrix?.length ?? 0} captures via ${dataSource || "unknown"}`,
        });

        // Create states via direct Tauri IPC to avoid React state batching issue.
        // createConfig sets activeConfig asynchronously, so calling smConfig.createState
        // immediately would fail because activeConfig hasn't updated yet.
        const fpDetails = cooccurrenceData.fingerprintDetails ?? {};
        for (const state of result.states) {
          // Build element labels and positions from fingerprint details so they
          // survive across sessions (stored in extra_metadata).
          const elementLabels: Record<string, string> = {};
          const elementPositions: Record<string, { top: number; left: number }> = {};
          const elementTags: Record<string, { tagName: string; role: string; zone: string }> = {};
          for (const eid of state.elementIds) {
            const fp = fpDetails[eid];
            if (fp) {
              elementLabels[eid] =
                fp.accessibleName || fp.role || fp.tagName || eid.slice(0, 12);
              if (fp.relativePosition) {
                elementPositions[eid] = fp.relativePosition;
              }
              elementTags[eid] = {
                tagName: fp.tagName || "",
                role: fp.role || "",
                zone: fp.positionZone || "",
              };
            }
          }

          const request: StateMachineStateCreate = {
            state_id: state.stateId,
            name: state.name,
            description: state.landmarkContext || "",
            element_ids: state.elementIds,
            confidence: state.confidence,
            extra_metadata: {
              elementLabels,
              elementPositions,
              elementTags,
            },
          };
          try {
            await invoke<StateMachineState>("sm_create_state", {
              configId: config.id,
              request,
            });
          } catch (stateErr) {
            console.warn(
              `[useUIBridgeDiscovery] Failed to create state "${state.name}":`,
              stateErr,
            );
          }
        }

        showToast?.(
          `Saved config "${saveName.trim()}" with ${result.states.length} states`,
          "success",
        );

        const savedConfigId = config.id;

        // Save thumbnails to database (survives across sessions).
        // Thumbnails may be in cooccurrenceData.elementThumbnails (if exploration just finished)
        // or in the separate localStorage key (if data was loaded from persistence without thumbnails).
        let thumbnails = cooccurrenceData.elementThumbnails;
        if (!thumbnails || Object.keys(thumbnails).length === 0) {
          try {
            const stored = localStorage.getItem("qontinui-runner-sm-thumbnails");
            if (stored) thumbnails = JSON.parse(stored);
          } catch {
            /* */
          }
        }
        if (thumbnails && Object.keys(thumbnails).length > 0) {
          try {
            await invoke<number>("sm_save_thumbnails", {
              configId: savedConfigId,
              thumbnails,
            });
          } catch (err) {
            console.warn("[useUIBridgeDiscovery] Failed to save thumbnails:", err);
          }
        }

        // Clear discovery state
        setCooccurrenceDataState(null);
        setDataSource(null);
        setDiscoveryResult(null);
        setConfigName("");
        clearPersistedState();

        return savedConfigId;
      } catch (err) {
        const message = err instanceof Error ? err.message : "Failed to save";
        showToast?.(message, "error");
        console.warn("[useUIBridgeDiscovery]", message);
        return null;
      } finally {
        setIsSaving(false);
      }
    },
    [cooccurrenceData, dataSource, configName, discoveryResult, smConfig, showToast],
  );

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

/**
 * JS-based fallback for fingerprint state discovery.
 * Groups fingerprints into states by position zone and structural similarity,
 * using co-occurrence data for confidence scoring.
 */
function discoverStatesFromFingerprints(data: CooccurrenceExport): FingerprintDiscoveryResult {
  const allFingerprints = data.allFingerprints ?? [];
  const presenceMatrix = data.presenceMatrix ?? [];
  const fingerprintDetails = data.fingerprintDetails ?? {};
  const transitions = data.transitions ?? [];
  const totalCaptures = presenceMatrix.length;
  if (totalCaptures === 0 || allFingerprints.length === 0) {
    return {
      states: [],
      transitions: [],
      statistics: {
        totalCaptures: 0,
        totalTransitions: 0,
        uniqueFingerprints: 0,
        discoveredStates: 0,
        globalStates: 0,
        modalStates: 0,
        discoveredTransitions: 0,
      },
    };
  }

  // Build fingerprint appearance counts
  const fpAppearances = new Map<string, number>();
  for (const fp of allFingerprints) fpAppearances.set(fp, 0);
  for (const capture of presenceMatrix) {
    for (const fp of capture.fingerprints) {
      fpAppearances.set(fp, (fpAppearances.get(fp) ?? 0) + 1);
    }
  }

  // Identify global fingerprints (appear in >80% of captures)
  const globalThreshold = totalCaptures * 0.8;
  const globalFps = new Set<string>();
  for (const [fp, count] of fpAppearances) {
    if (count >= globalThreshold) globalFps.add(fp);
  }

  // Group non-global fingerprints by position zone and landmark context
  // This creates meaningful states based on UI regions
  const zoneGroups = new Map<string, string[]>();
  const nonGlobalFps = allFingerprints.filter((fp) => !globalFps.has(fp));

  for (const fp of nonGlobalFps) {
    const detail = fingerprintDetails[fp];
    if (!detail) continue;

    // Create a group key from zone + landmark + structural path prefix
    const zone = detail.positionZone || "main";
    const landmark = detail.landmarkContext || "none";
    const structPrefix = detail.structuralPath?.split(" > ").slice(0, 3).join(" > ") || "";
    const groupKey = `${zone}|${landmark}|${structPrefix}`;

    if (!zoneGroups.has(groupKey)) {
      zoneGroups.set(groupKey, []);
    }
    zoneGroups.get(groupKey)!.push(fp);
  }

  // If zone grouping produced only 1 group (e.g., everything is "main"),
  // split further by structural path and role
  if (zoneGroups.size <= 1 && nonGlobalFps.length > 5) {
    zoneGroups.clear();
    for (const fp of nonGlobalFps) {
      const detail = fingerprintDetails[fp];
      if (!detail) continue;
      const zone = detail.positionZone || "main";
      const role = detail.role || detail.tagName || "unknown";
      const size = detail.sizeCategory || "medium";
      const groupKey = `${zone}|${role}|${size}`;

      if (!zoneGroups.has(groupKey)) {
        zoneGroups.set(groupKey, []);
      }
      zoneGroups.get(groupKey)!.push(fp);
    }
  }

  // Build states from groups
  const states: FingerprintDiscoveryResult["states"] = [];
  let stateIndex = 0;

  // Global state
  if (globalFps.size > 0) {
    states.push({
      stateId: "state-global",
      name: "Global (persistent UI)",
      fingerprintHashes: [...globalFps],
      elementIds: [...globalFps],
      positionZone: "global",
      isGlobal: true,
      isModal: false,
      confidence: 1.0,
      observationCount: totalCaptures,
    });
  }

  // Named zone states
  const zoneNames: Record<string, string> = {
    header: "Header",
    footer: "Footer",
    "sidebar-left": "Left Sidebar",
    "sidebar-right": "Right Sidebar",
    main: "Main Content",
    modal: "Modal Dialog",
    "fixed-top": "Top Bar",
    "fixed-bottom": "Bottom Bar",
  };

  for (const [groupKey, fps] of zoneGroups) {
    if (fps.length === 0) continue;
    const [zone, landmark, structOrRole] = groupKey.split("|");

    // Compute a human-readable name
    let name = zoneNames[zone] || zone;
    if (landmark && landmark !== "none") {
      name = landmark.charAt(0).toUpperCase() + landmark.slice(1);
    }
    if (structOrRole && structOrRole !== "none" && structOrRole !== "unknown") {
      // Use last part of structural path or role for disambiguation
      const suffix = structOrRole.includes(" > ") ? structOrRole.split(" > ").pop() : structOrRole;
      if (suffix && suffix !== name.toLowerCase()) {
        name = `${name} — ${suffix}`;
      }
    }

    // Compute confidence from appearance rate
    const avgAppearance =
      fps.reduce((sum, fp) => sum + (fpAppearances.get(fp) ?? 0), 0) / fps.length;
    const confidence = Math.min(1.0, avgAppearance / totalCaptures);

    stateIndex++;
    states.push({
      stateId: `state-${stateIndex}`,
      name: `${name} (${fps.length} elements)`,
      fingerprintHashes: fps,
      elementIds: fps,
      positionZone: zone,
      isGlobal: false,
      isModal: zone === "modal",
      confidence,
      observationCount: Math.round(avgAppearance),
    });
  }

  // Map transitions between captures to state transitions
  const discoveredTransitions: FingerprintDiscoveryResult["transitions"] = [];
  if (transitions && transitions.length > 0) {
    // Build capture→state mapping based on which state's fingerprints are present
    const captureToStates = new Map<string, Set<string>>();
    for (const capture of presenceMatrix) {
      const presentStates = new Set<string>();
      for (const st of states) {
        if (st.isGlobal) continue;
        const overlap = st.fingerprintHashes.filter((fp) =>
          capture.fingerprints.includes(fp),
        ).length;
        if (overlap > 0) presentStates.add(st.stateId);
      }
      captureToStates.set(capture.captureId, presentStates);
    }

    const transitionCounts = new Map<string, { count: number; actionType: string }>();
    for (const t of transitions) {
      const beforeStates = captureToStates.get(t.beforeCaptureId);
      const afterStates = captureToStates.get(t.afterCaptureId);
      if (!beforeStates || !afterStates) continue;

      // Find states that appeared or disappeared
      for (const toId of afterStates) {
        if (!beforeStates.has(toId)) {
          for (const fromId of beforeStates) {
            if (!afterStates.has(fromId)) {
              const key = `${fromId}->${toId}`;
              const existing = transitionCounts.get(key);
              if (existing) existing.count++;
              else transitionCounts.set(key, { count: 1, actionType: t.actionType });
            }
          }
        }
      }
    }

    for (const [key, { count, actionType }] of transitionCounts) {
      const [fromStateId, toStateId] = key.split("->");
      discoveredTransitions.push({ fromStateId, toStateId, actionType, count });
    }
  }

  return {
    states,
    transitions: discoveredTransitions,
    statistics: {
      totalCaptures,
      totalTransitions: discoveredTransitions.length,
      uniqueFingerprints: allFingerprints.length,
      discoveredStates: states.length,
      globalStates: states.filter((s) => s.isGlobal).length,
      modalStates: states.filter((s) => s.isModal).length,
      discoveredTransitions: discoveredTransitions.length,
    },
  };
}
