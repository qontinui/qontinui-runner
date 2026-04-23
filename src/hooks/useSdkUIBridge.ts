/**
 * useSdkUIBridge Hook
 *
 * Connects to UI Bridge SDK-integrated apps via direct HTTP through the runner.
 * Unlike useExternalUIBridge (legacy extension relay),
 * this communicates directly with SDK apps:
 *
 *   Frontend → POST /ui-bridge/sdk/* → Runner (Rust) → HTTP → SDK App
 *
 * The hook exposes a compatible interface so the ExternalUIBridgeInspector
 * can use either hook interchangeably.
 *
 * This is a composition hook that delegates to sub-hooks in ./sdk-ui-bridge/
 * for SRP compliance.
 */

import { useCallback, useEffect, useRef } from "react";
import {
  useConnection,
  useElements,
  useCommands,
  useSnapshotComponents,
  useScreenshot,
  useAIOperations,
  useCaptureSession,
  useStateExploration,
} from "./sdk-ui-bridge";

import type { SdkAppInfo, UseSdkUIBridgeReturn } from "./sdk-ui-bridge";

// Re-export types that consumers expect from this module
export type {
  SdkAppInfo,
  SdkConnectionInfo,
  ExplorationProgress,
  UseSdkUIBridgeReturn,
} from "./sdk-ui-bridge";

// Re-export consumePendingCaptureScreenshots for callers that import from this module
export { consumePendingCaptureScreenshots } from "./sdk-ui-bridge";

// =============================================================================
// Hook
// =============================================================================

export function useSdkUIBridge(): UseSdkUIBridgeReturn {
  // Shared ref for connectedApp — avoids stale closure in sub-hooks
  // that need the current app info without re-creating callbacks.
  const connectedAppRef = useRef<SdkAppInfo | null>(null);

  // --- Snapshot & Components ---
  const { snapshot, components, refreshSnapshot, refreshComponents, setSnapshot, setComponents } =
    useSnapshotComponents();

  // --- Capture Session ---
  const captureHook = useCaptureSession(
    // fetchElements placeholder — wired below via stable ref
    async () => {
      await fetchElementsRef.current();
    },
  );

  // --- Elements ---
  // Stable ref for fetchElements to break circular dependency:
  // useCaptureSession.startCaptureSession calls fetchElements,
  // and useElements needs captureSessionRef from useCaptureSession.
  const fetchElementsRef = useRef<() => Promise<void>>(async () => {});

  const elementsHook = useElements(
    connectedAppRef,
    captureHook.captureSessionRef,
    captureHook.setCaptureSession,
  );

  // Wire up the stable ref now that elementsHook exists
  useEffect(() => {
    fetchElementsRef.current = elementsHook.fetchElements;
  });

  // --- Connection ---
  const clearElementState = useCallback(() => {
    elementsHook.setElements([]);
    elementsHook.setSelectedElementId(null);
    setSnapshot(null);
    setComponents([]);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [setSnapshot, setComponents]);

  const connectionHook = useConnection(elementsHook.fetchElements, clearElementState);

  // Keep connectedAppRef in sync with connection state
  useEffect(() => {
    connectedAppRef.current = connectionHook.connectedApp;
  });

  // --- Commands ---
  const commandsHook = useCommands(
    elementsHook.elements,
    elementsHook.fetchElements,
    captureHook.captureSessionRef,
  );

  // --- Screenshot ---
  const screenshotHook = useScreenshot();

  // --- AI Operations ---
  const aiHook = useAIOperations();

  // --- State Exploration ---
  const explorationHook = useStateExploration(
    connectionHook.connectedApp,
    elementsHook.fetchElements,
    captureHook.generateCooccurrenceExport,
    captureHook.captureSessionRef,
    captureHook.setCaptureSession,
    elementsHook.setElements,
    () => {
      // setCooccurrenceData clearing is handled internally by captureHook
      // when generateCooccurrenceExport is called. The exploration hook
      // passes null here at session start, which is handled by the
      // capture session's startCaptureSession (which calls setCooccurrenceData(null)).
    },
  );

  return {
    // Connection
    connectionStatus: connectionHook.connectionStatus,
    connectedApp: connectionHook.connectedApp,
    error: connectionHook.error,
    connect: connectionHook.connect,
    disconnect: connectionHook.disconnect,
    connections: connectionHook.connections,
    switchConnection: connectionHook.switchConnection,

    // Elements
    elements: elementsHook.elements,
    selectedElementId: elementsHook.selectedElementId,
    selectedElement: elementsHook.selectedElement,
    selectElement: elementsHook.selectElement,
    refreshElements: elementsHook.refreshElements,
    isLoadingElements: elementsHook.isLoadingElements,

    // Commands
    executeAction: commandsHook.executeAction,
    sendCommand: commandsHook.sendCommand,
    lastCommandResult: commandsHook.lastCommandResult,
    commandHistory: commandsHook.commandHistory,
    clearCommandHistory: commandsHook.clearCommandHistory,

    // Snapshot & Components
    snapshot,
    refreshSnapshot,
    components,
    refreshComponents,

    // AI
    aiSearch: aiHook.aiSearch,
    aiExecute: aiHook.aiExecute,

    // Screenshot
    pageScreenshot: screenshotHook.pageScreenshot,
    isCapturingScreenshot: screenshotHook.isCapturingScreenshot,
    captureScreenshot: screenshotHook.captureScreenshot,
    highlightElement: screenshotHook.highlightElement,

    // Capture Session
    captureSession: captureHook.captureSession,
    startCaptureSession: captureHook.startCaptureSession,
    stopCaptureSession: captureHook.stopCaptureSession,
    cooccurrenceData: captureHook.cooccurrenceData,
    isLoadingCooccurrence: captureHook.isLoadingCooccurrence,
    generateCooccurrenceExport: captureHook.generateCooccurrenceExport,

    // State Exploration
    exploreStates: explorationHook.exploreStates,
    isExploring: explorationHook.isExploring,
    explorationProgress: explorationHook.explorationProgress,
    cancelExploration: explorationHook.cancelExploration,
  };
}
