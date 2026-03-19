/**
 * Shared types for State Discovery components.
 *
 * Extracted from StateDiscoveryPanel to break circular dependencies
 * between the discovery panel and its child views.
 */

/** Result from fingerprint state discovery */
export interface FingerprintDiscoveryResult {
  states: Array<{
    stateId: string;
    name: string;
    fingerprintHashes: string[];
    elementIds?: string[];
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
