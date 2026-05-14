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
import { consumePendingCaptureScreenshots } from "./useSdkUIBridge";

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
  /** Session ID for pending screenshots saved to DB during exploration */
  pendingScreenshotSessionId?: string;
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
  const [pendingScreenshotSessionId] = useState(initial?.pendingScreenshotSessionId);

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
      let savedConfigId: string;

      try {
        // Step 1: Create config + states (critical — must succeed)
        const config = await smConfig.createConfig({
          name: saveName.trim(),
          description: `Discovered from ${cooccurrenceData.presenceMatrix?.length ?? 0} captures via ${dataSource || "unknown"}`,
        });
        savedConfigId = config.id;

        await createStatesForConfig(config.id, result.states, cooccurrenceData);

        showToast?.(
          `Saved config "${saveName.trim()}" with ${result.states.length} states`,
          "success",
        );
      } catch (err) {
        const message = err instanceof Error ? err.message : "Failed to save config";
        showToast?.(message, "error");
        console.warn("[useUIBridgeDiscovery] Config creation failed:", message);
        setIsSaving(false);
        return null;
      }

      // Step 2: Refresh the dropdown IMMEDIATELY after config creation.
      // This must happen before any optional work so the user sees the new config.
      try {
        await smConfig.loadConfigs();
      } catch {
        // Non-fatal: user can click Refresh manually
      }

      // Step 3: Save optional assets (thumbnails, screenshots).
      // These are fire-and-forget — failures must never block or hide the config.
      saveThumbnailsForConfig(savedConfigId, cooccurrenceData);
      saveScreenshotsForConfig(savedConfigId, cooccurrenceData, pendingScreenshotSessionId);

      // Step 4: Clear discovery state
      setCooccurrenceDataState(null);
      setDataSource(null);
      setDiscoveryResult(null);
      setConfigName("");
      clearPersistedState();
      setIsSaving(false);

      return savedConfigId;
    },
    [
      cooccurrenceData,
      dataSource,
      configName,
      discoveryResult,
      smConfig,
      showToast,
      pendingScreenshotSessionId,
    ],
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

// =============================================================================
// Extracted helpers — each does ONE thing, failures are isolated
// =============================================================================

/** Create states for a config via direct Tauri IPC. */
async function createStatesForConfig(
  configId: string,
  states: FingerprintDiscoveryResult["states"],
  cooccurrenceData: CooccurrenceExport,
): Promise<void> {
  const fpDetails = cooccurrenceData.fingerprintDetails ?? {};
  for (const state of states) {
    const elementLabels: Record<string, string> = {};
    const elementPositions: Record<string, { top: number; left: number }> = {};
    const elementTags: Record<string, { tagName: string; role: string; zone: string }> = {};
    for (const eid of state.elementIds) {
      const colonIdx = eid.indexOf(":");
      const hash = colonIdx > 0 ? eid.slice(colonIdx + 1) : eid;
      const fp = fpDetails[hash] ?? fpDetails[eid];
      if (fp) {
        elementLabels[eid] = fp.accessibleName || fp.role || fp.tagName || eid.slice(0, 12);
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
      extra_metadata: { elementLabels, elementPositions, elementTags },
    };
    try {
      await invoke<StateMachineState>("sm_create_state", { configId, request });
    } catch (err) {
      console.warn(`[useUIBridgeDiscovery] Failed to create state "${state.name}":`, err);
    }
  }
}

/** Save element thumbnails for a config (fire-and-forget). */
function saveThumbnailsForConfig(configId: string, cooccurrenceData: CooccurrenceExport): void {
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
    invoke<number>("sm_save_thumbnails", { configId, thumbnails }).catch((err) => {
      console.warn("[useUIBridgeDiscovery] Failed to save thumbnails:", err);
    });
  }
}

/**
 * Save capture screenshots for a config (fire-and-forget).
 *
 * Tries multiple sources in order:
 * 1. In-memory from cooccurrenceData (exploration just completed, same page)
 * 2. Module-level pending variable (exploration completed, page navigated)
 * 3. DB pending rows from incremental saves during exploration
 * 4. Fallback: capture a fresh screenshot now
 */
function saveScreenshotsForConfig(
  configId: string,
  cooccurrenceData: CooccurrenceExport,
  pendingSessionId?: string,
): void {
  const fromMemory = cooccurrenceData.captureScreenshots || consumePendingCaptureScreenshots();
  if (fromMemory && fromMemory.length > 0) {
    invoke<number>("sm_save_capture_screenshots", {
      configId,
      screenshots: fromMemory,
    }).catch((err) => {
      console.warn("[useUIBridgeDiscovery] Failed to save capture screenshots:", err);
    });
    return;
  }

  if (pendingSessionId) {
    invoke<number>("sm_move_pending_screenshots", {
      fromConfigId: `pending-${pendingSessionId}`,
      toConfigId: configId,
    })
      .then((moved) => {
        if (moved === 0) captureFallbackScreenshot(configId, cooccurrenceData);
      })
      .catch(() => {
        captureFallbackScreenshot(configId, cooccurrenceData);
      });
    return;
  }

  captureFallbackScreenshot(configId, cooccurrenceData);
}

/** Capture a fresh screenshot and save it for the given config.
 *  Takes a live snapshot to determine which elements are currently visible,
 *  so only those elements get bounding boxes — not all ever-seen elements. */
async function captureFallbackScreenshot(
  configId: string,
  cooccurrenceData: CooccurrenceExport,
): Promise<void> {
  try {
    const { getApiBase, tracedFetch } = await import("@/lib/runner-api");
    const { captureScreenshotBase64 } = await import("@/lib/vision-api");
    const { generateFingerprints } = await import("@/lib/ui-bridge/fingerprintGenerator");

    // Use per-page fingerprints from the last presenceMatrix entry,
    // not allFingerprints which spans all pages ever visited.
    const matrix = cooccurrenceData.presenceMatrix ?? [];
    const lastEntry = matrix[matrix.length - 1];
    const visibleHashes = new Set(lastEntry?.fingerprints ?? []);

    // Fetch screenshot (Phase 2 vision/capture, runner window only) and live
    // SDK snapshot in parallel so we get actual element bounds. Use
    // individual .catch() so a snapshot failure doesn't lose the screenshot.
    const [ssResult, snapResp] = await Promise.all([
      captureScreenshotBase64({ contract: "claude" }).catch(() => null),
      tracedFetch(`${getApiBase()}/ui-bridge/sdk/snapshot?recency=current`).catch(() => null),
    ]);
    if (!ssResult) return;

    // vision/capture has no scaleFactor field; SDK element bounds are
    // reported in CSS pixels so we use the live window devicePixelRatio to
    // scale them up to image-pixel space.
    const dpr = typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1;
    // ssResult.width/height are already in physical pixels (xcap returns
    // image.width()/height()). The element bounds below ARE multiplied by
    // dpr because the SDK reports them in CSS pixels — but the screenshot
    // dims must NOT be multiplied again. Doing so was the source of the
    // 1.5x metadata-vs-bytes mismatch on HiDPI displays.
    const imgW = ssResult.width || 1920;
    const imgH = ssResult.height || 1080;

    // Build a hash→rect map from live snapshot elements (actual bounding boxes)
    const liveBounds = new Map<string, { x: number; y: number; width: number; height: number }>();
    try {
      if (!snapResp) throw new Error("snapshot fetch failed");
      const { mapSdkElement } = await import("./sdk-ui-bridge/utils");
      const snapJson = await snapResp.json();
      if (snapJson.success) {
        const candidate = snapJson.data?.elements || snapJson.elements || snapJson.data || [];
        const rawElements: Record<string, unknown>[] = Array.isArray(candidate) ? candidate : [];
        const mapped = rawElements.map(mapSdkElement);
        const { elementFingerprintMap } = generateFingerprints(mapped);
        for (const el of mapped) {
          const fp = elementFingerprintMap.get(el.id);
          if (fp && el.bounds.width > 0 && el.bounds.height > 0) {
            liveBounds.set(fp.hash, {
              x: el.bounds.x * dpr,
              y: el.bounds.y * dpr,
              width: el.bounds.width * dpr,
              height: el.bounds.height * dpr,
            });
          }
        }
      }
    } catch {
      // Live snapshot failed — fall through to relativePosition fallback
    }

    // Build bounds: prefer live snapshot bounds, fall back to relativePosition estimate
    const boundsMap: Record<string, { x: number; y: number; width: number; height: number }> = {};
    const fpDetails = cooccurrenceData.fingerprintDetails ?? {};
    for (const hash of visibleHashes) {
      const live = liveBounds.get(hash);
      if (live) {
        boundsMap[hash] = live;
      } else {
        // Fallback: use relativePosition with size estimated from sizeCategory
        const fp = fpDetails[hash];
        if (fp?.relativePosition) {
          const sizePx = estimateSizePx(fp.sizeCategory, imgW, imgH);
          boundsMap[hash] = {
            x: fp.relativePosition.left * imgW,
            y: fp.relativePosition.top * imgH,
            width: sizePx.width,
            height: sizePx.height,
          };
        }
      }
    }

    await invoke<number>("sm_save_capture_screenshots", {
      configId,
      screenshots: [
        {
          captureIndex: 0,
          screenshotBase64: ssResult.screenshot,
          width: Math.round(imgW),
          height: Math.round(imgH),
          elementBoundsJson: JSON.stringify(boundsMap),
          fingerprintHashesJson: JSON.stringify([...visibleHashes]),
          capturedAt: new Date().toISOString(),
        },
      ],
    });
  } catch (err) {
    console.warn("[useUIBridgeDiscovery] Fallback screenshot failed:", err);
  }
}

/** Estimate element pixel size from sizeCategory when actual bounds aren't available. */
function estimateSizePx(
  sizeCategory: string | undefined,
  imgW: number,
  imgH: number,
): { width: number; height: number } {
  switch (sizeCategory) {
    case "icon":
      return { width: 24 * (imgW / 1920), height: 24 * (imgH / 1080) };
    case "button":
      return { width: 80 * (imgW / 1920), height: 32 * (imgH / 1080) };
    case "small":
      return { width: 120 * (imgW / 1920), height: 36 * (imgH / 1080) };
    case "medium":
      return { width: 200 * (imgW / 1920), height: 48 * (imgH / 1080) };
    case "large":
      return { width: 400 * (imgW / 1920), height: 80 * (imgH / 1080) };
    case "fullwidth":
      return { width: imgW * 0.9, height: 48 * (imgH / 1080) };
    case "panel":
      return { width: imgW * 0.3, height: imgH * 0.4 };
    default:
      return { width: 100 * (imgW / 1920), height: 36 * (imgH / 1080) };
  }
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
