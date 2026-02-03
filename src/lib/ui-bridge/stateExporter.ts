/**
 * State Discovery Export Utilities
 *
 * Converts state discovery data (state candidates and discovered states)
 * into the GUI automation config format used by qontinui.
 */

import type { CooccurrenceExport, ElementFingerprint } from "../../hooks/useExternalUIBridge";
import type { FingerprintDiscoveryResult } from "../../components/ui-bridge/StateDiscoveryPanel";

/**
 * Options for exporting to automation config
 */
export interface StateExportOptions {
  /** Name for the exported config */
  configName: string;
  /** Whether to mark global states as always-active */
  includeGlobalStates: boolean;
  /** Whether to mark modal states as blocking */
  includeModalStates: boolean;
  /** Which states to include (empty = all) */
  selectedStateIds: string[];
  /** Whether to include element details in the config */
  includeElementDetails: boolean;
}

/**
 * Element definition in the automation config
 */
export interface AutomationElement {
  id: string;
  type: string;
  label: string;
  fingerprint?: {
    hash: string;
    role: string;
    tagName: string;
    positionZone: string;
    landmarkContext: string;
    accessibleName?: string;
  };
}

/**
 * State definition in the automation config
 */
export interface AutomationState {
  id: string;
  name: string;
  description?: string;
  /** Fingerprint hashes that identify this state */
  fingerprints: string[];
  /** Whether this is a global state (always active) */
  isGlobal?: boolean;
  /** Whether this is a modal state (blocking) */
  isModal?: boolean;
  /** Position zone where this state appears */
  positionZone?: string;
  /** Landmark context for the state */
  landmarkContext?: string;
  /** Elements within this state */
  elements?: AutomationElement[];
  /** Confidence score from discovery (0-1) */
  confidence?: number;
  /** How many times this state was observed */
  observationCount?: number;
}

/**
 * Transition definition in the automation config
 */
export interface AutomationTransition {
  id: string;
  from: string;
  to: string;
  action: {
    type: string;
    element?: string;
    params?: Record<string, unknown>;
  };
  /** Number of times this transition was observed */
  count?: number;
}

/**
 * The exported automation config format
 */
export interface AutomationConfig {
  name: string;
  version: string;
  description?: string;
  exportedAt: string;
  source: "state-discovery";
  metadata: {
    sessionId?: string;
    totalCaptures?: number;
    totalFingerprints?: number;
    exportOptions: StateExportOptions;
  };
  states: AutomationState[];
  transitions: AutomationTransition[];
  /** Raw fingerprint details for reference */
  fingerprintDetails?: Record<string, ElementFingerprint>;
}

/**
 * Default export options
 */
export function getDefaultExportOptions(): StateExportOptions {
  return {
    configName: "Discovered States",
    includeGlobalStates: true,
    includeModalStates: true,
    selectedStateIds: [],
    includeElementDetails: true,
  };
}

/**
 * Generate a unique ID for a state candidate (used when no stateId exists)
 */
function generateStateCandidateId(index: number): string {
  return `state-candidate-${index + 1}`;
}

/**
 * Determine the dominant position zone from fingerprints
 */
function getDominantZone(
  fingerprints: string[],
  fingerprintDetails: Record<string, ElementFingerprint>,
): string {
  const zoneCounts: Record<string, number> = {};

  fingerprints.forEach((hash) => {
    const fp = fingerprintDetails[hash];
    if (fp?.positionZone) {
      zoneCounts[fp.positionZone] = (zoneCounts[fp.positionZone] || 0) + 1;
    }
  });

  const sorted = Object.entries(zoneCounts).sort((a, b) => b[1] - a[1]);
  return sorted[0]?.[0] || "main";
}

/**
 * Determine the dominant landmark from fingerprints
 */
function getDominantLandmark(
  fingerprints: string[],
  fingerprintDetails: Record<string, ElementFingerprint>,
): string {
  const landmarkCounts: Record<string, number> = {};

  fingerprints.forEach((hash) => {
    const fp = fingerprintDetails[hash];
    if (fp?.landmarkContext) {
      landmarkCounts[fp.landmarkContext] = (landmarkCounts[fp.landmarkContext] || 0) + 1;
    }
  });

  const sorted = Object.entries(landmarkCounts).sort((a, b) => b[1] - a[1]);
  return sorted[0]?.[0] || "none";
}

/**
 * Classify a state candidate as global, modal, or content based on zone
 */
function classifyStateType(zone: string): { isGlobal: boolean; isModal: boolean } {
  if (zone === "header" || zone === "footer") {
    return { isGlobal: true, isModal: false };
  }
  if (zone === "modal") {
    return { isGlobal: false, isModal: true };
  }
  return { isGlobal: false, isModal: false };
}

/**
 * Build automation elements from fingerprint hashes
 */
function buildElements(
  fingerprints: string[],
  fingerprintDetails: Record<string, ElementFingerprint>,
  includeDetails: boolean,
): AutomationElement[] {
  return fingerprints.map((hash) => {
    const fp = fingerprintDetails[hash];

    const element: AutomationElement = {
      id: hash,
      type: fp?.role || fp?.tagName || "element",
      label: fp?.accessibleName || fp?.role || hash.slice(0, 12),
    };

    if (includeDetails && fp) {
      element.fingerprint = {
        hash: fp.hash,
        role: fp.role,
        tagName: fp.tagName,
        positionZone: fp.positionZone,
        landmarkContext: fp.landmarkContext,
        accessibleName: fp.accessibleName,
      };
    }

    return element;
  });
}

/**
 * Export raw state candidates (from co-occurrence data) to automation config
 */
export function exportStateCandidatesToConfig(
  cooccurrenceData: CooccurrenceExport,
  options: StateExportOptions,
): AutomationConfig {
  const {
    configName,
    includeGlobalStates,
    includeModalStates,
    selectedStateIds,
    includeElementDetails,
  } = options;

  const states: AutomationState[] = [];

  cooccurrenceData.stateCandidates.forEach((candidate, index) => {
    const stateId = generateStateCandidateId(index);

    // Skip if specific states are selected and this isn't one of them
    if (selectedStateIds.length > 0 && !selectedStateIds.includes(stateId)) {
      return;
    }

    const zone = getDominantZone(candidate.fingerprints, cooccurrenceData.fingerprintDetails);
    const landmark = getDominantLandmark(
      candidate.fingerprints,
      cooccurrenceData.fingerprintDetails,
    );
    const { isGlobal, isModal } = classifyStateType(zone);

    // Skip based on options
    if (isGlobal && !includeGlobalStates) return;
    if (isModal && !includeModalStates) return;

    const state: AutomationState = {
      id: stateId,
      name: `State ${index + 1}`,
      description: `Discovered state with ${candidate.fingerprints.length} co-occurring elements (${(candidate.cooccurrenceRate * 100).toFixed(0)}% co-occurrence rate)`,
      fingerprints: candidate.fingerprints,
      isGlobal,
      isModal,
      positionZone: zone,
      landmarkContext: landmark,
      observationCount: candidate.appearanceCount,
    };

    if (includeElementDetails) {
      state.elements = buildElements(
        candidate.fingerprints,
        cooccurrenceData.fingerprintDetails,
        true,
      );
    }

    states.push(state);
  });

  // Build transitions from co-occurrence transition data
  const transitions: AutomationTransition[] = cooccurrenceData.transitions.map((t, index) => ({
    id: `transition-${index + 1}`,
    from: "unknown", // Raw transitions don't have state-to-state mapping yet
    to: "unknown",
    action: {
      type: t.actionType,
      element: t.targetFingerprint,
      params: {
        appearedFingerprints: t.appearedFingerprints,
        disappearedFingerprints: t.disappearedFingerprints,
      },
    },
  }));

  return {
    name: configName,
    version: "1.0.0",
    description: `State discovery export from session ${cooccurrenceData.sessionId}`,
    exportedAt: new Date().toISOString(),
    source: "state-discovery",
    metadata: {
      sessionId: cooccurrenceData.sessionId,
      totalCaptures: cooccurrenceData.presenceMatrix.length,
      totalFingerprints: cooccurrenceData.allFingerprints.length,
      exportOptions: options,
    },
    states,
    transitions,
    fingerprintDetails: includeElementDetails ? cooccurrenceData.fingerprintDetails : undefined,
  };
}

/**
 * Export discovered states (from Python library analysis) to automation config
 */
export function exportDiscoveredStatesToConfig(
  discoveryResult: FingerprintDiscoveryResult,
  fingerprintDetails: Record<string, ElementFingerprint>,
  options: StateExportOptions,
): AutomationConfig {
  const {
    configName,
    includeGlobalStates,
    includeModalStates,
    selectedStateIds,
    includeElementDetails,
  } = options;

  const states: AutomationState[] = [];

  discoveryResult.states.forEach((state) => {
    // Skip if specific states are selected and this isn't one of them
    if (selectedStateIds.length > 0 && !selectedStateIds.includes(state.stateId)) {
      return;
    }

    // Skip based on options
    if (state.isGlobal && !includeGlobalStates) return;
    if (state.isModal && !includeModalStates) return;

    const automationState: AutomationState = {
      id: state.stateId,
      name: state.name,
      fingerprints: state.fingerprintHashes,
      isGlobal: state.isGlobal,
      isModal: state.isModal,
      positionZone: state.positionZone,
      landmarkContext: state.landmarkContext,
      confidence: state.confidence,
      observationCount: state.observationCount,
    };

    if (includeElementDetails) {
      automationState.elements = buildElements(state.fingerprintHashes, fingerprintDetails, true);
    }

    states.push(automationState);
  });

  // Build transitions from discovered transition data
  const transitions: AutomationTransition[] = discoveryResult.transitions.map((t, index) => ({
    id: `transition-${index + 1}`,
    from: t.fromStateId,
    to: t.toStateId,
    action: {
      type: t.actionType,
    },
    count: t.count,
  }));

  return {
    name: configName,
    version: "1.0.0",
    description: `Discovered states from fingerprint analysis`,
    exportedAt: new Date().toISOString(),
    source: "state-discovery",
    metadata: {
      totalCaptures: discoveryResult.statistics.totalCaptures,
      totalFingerprints: discoveryResult.statistics.uniqueFingerprints,
      exportOptions: options,
    },
    states,
    transitions,
    fingerprintDetails: includeElementDetails ? fingerprintDetails : undefined,
  };
}

/**
 * Main export function that handles both raw candidates and discovered states
 */
export function exportToAutomationConfig(
  cooccurrenceData: CooccurrenceExport | null,
  discoveryResult: FingerprintDiscoveryResult | null,
  options: StateExportOptions,
): AutomationConfig {
  // Prefer discovered states if available
  if (discoveryResult && discoveryResult.states.length > 0) {
    const fingerprintDetails = cooccurrenceData?.fingerprintDetails || {};
    return exportDiscoveredStatesToConfig(discoveryResult, fingerprintDetails, options);
  }

  // Fall back to raw state candidates
  if (cooccurrenceData) {
    return exportStateCandidatesToConfig(cooccurrenceData, options);
  }

  // No data available - return empty config
  return {
    name: options.configName,
    version: "1.0.0",
    description: "Empty config - no state discovery data available",
    exportedAt: new Date().toISOString(),
    source: "state-discovery",
    metadata: {
      exportOptions: options,
    },
    states: [],
    transitions: [],
  };
}

/**
 * Get a list of available states for selection in the export dialog
 */
export function getAvailableStatesForExport(
  cooccurrenceData: CooccurrenceExport | null,
  discoveryResult: FingerprintDiscoveryResult | null,
): Array<{ id: string; name: string; isGlobal: boolean; isModal: boolean; elementCount: number }> {
  // Prefer discovered states
  if (discoveryResult && discoveryResult.states.length > 0) {
    return discoveryResult.states.map((state) => ({
      id: state.stateId,
      name: state.name,
      isGlobal: state.isGlobal,
      isModal: state.isModal,
      elementCount: state.fingerprintHashes.length,
    }));
  }

  // Fall back to raw state candidates
  if (cooccurrenceData) {
    return cooccurrenceData.stateCandidates.map((candidate, index) => {
      const zone = getDominantZone(candidate.fingerprints, cooccurrenceData.fingerprintDetails);
      const { isGlobal, isModal } = classifyStateType(zone);

      return {
        id: generateStateCandidateId(index),
        name: `State ${index + 1}`,
        isGlobal,
        isModal,
        elementCount: candidate.fingerprints.length,
      };
    });
  }

  return [];
}
