/**
 * External UI Bridge Inspector
 *
 * Main inspector component that connects to an external browser via the
 * UI Bridge HTTP API. Unlike ConnectedUIBridgeInspector which shows the
 * runner's own UI, this shows elements from the target browser.
 *
 * Features:
 * - Browser tab connection
 * - Element discovery and tree view
 * - Action execution
 * - Raw API testing (Postman-like)
 * - Command history
 */

import { useState, useCallback, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Badge } from "../ui/Badge";
import { Button } from "../ui/Button";
import {
  Layers,
  Activity,
  Play,
  Terminal,
  RefreshCw,
  Eye,
  EyeOff,
  Crosshair,
  Copy,
  Check,
  Sparkles,
  Bot,
  MessageSquare,
  FileText,
  Pointer,
  Keyboard,
  ChevronDown,
  ChevronRight,
  Accessibility,
  Code,
  Palette,
  Link,
  Image,
  Database,
  Fingerprint,
  MapPin,
  Repeat,
  Hash,
  Circle,
  Square,
  Download,
  Network,
  Compass,
  Loader2,
  Maximize2,
  Monitor,
} from "lucide-react";
import { ConnectionPanel } from "./ConnectionPanel";
import { RawApiPanel } from "./RawApiPanel";
import { ElementTreeView } from "./ElementTreeView";
import { EventTimelineView } from "./EventTimelineView";
import { ActionExecutorView } from "./ActionExecutorView";
import { SearchComparisonPanel } from "./SearchComparisonPanel";
import { ElementDescriptionPanel } from "./ElementDescriptionPanel";
import { NaturalLanguagePanel } from "./NaturalLanguagePanel";
import { ImageViewerModal } from "./ImageViewerModal";
import { StateDiscoveryPanel } from "./StateDiscoveryPanel";
import {
  ExplorationConfigDialog,
  type ExplorationConfig,
  type ExplorationResult,
} from "./ExplorationConfigDialog";
import { useExternalUIBridge } from "../../hooks/useExternalUIBridge";
import { useElementThumbnails } from "../../hooks/useElementThumbnails";
import { cropPreview } from "../../lib/thumbnail-cropper";
import { generateAIContext } from "../../lib/ui-bridge";
import type { UIBridgeElement, UIBridgeEvent, UIBridgeSnapshot } from "./UIBridgeInspectorPanel";

type TabId = "elements" | "states" | "search" | "describe" | "nl" | "events" | "actions" | "api";

/**
 * Convert external element to UIBridgeElement format for existing components
 */
function convertToUIBridgeElement(
  element: ReturnType<typeof useExternalUIBridge>["elements"][number],
): UIBridgeElement {
  return {
    id: element.id,
    tagName: element.tagName,
    type: element.type,
    bounds: {
      x: element.bounds.x,
      y: element.bounds.y,
      width: element.bounds.width,
      height: element.bounds.height,
    },
    visible: element.visible,
    enabled: element.enabled,
    focused: element.focused,
    value: element.value,
    text: element.text,
    label: element.label,
    parent: element.parent || null,
    children: element.children || [],
    actions: element.actions,
    // Accessibility properties
    accessibility: element.accessibility,
    role: element.role,
    accessibleName: element.accessibleName,
    is_expanded: element.is_expanded,
    is_pressed: element.is_pressed,
    is_selected: element.is_selected,
    is_required: element.is_required,
    is_readonly: element.is_readonly,
    is_interactive: element.is_interactive,
    ref: element.ref,
  };
}

export function ExternalUIBridgeInspector() {
  const bridge = useExternalUIBridge();

  const [activeTab, setActiveTab] = useState<TabId>("elements");
  const [events, setEvents] = useState<UIBridgeEvent[]>([]);
  const eventIdRef = useRef(0);
  const [pickerActive, setPickerActive] = useState(false);
  const [overlaysEnabled, setOverlaysEnabled] = useState(false);
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [aiContextCopied, setAiContextCopied] = useState(false);
  const [accessibilityExpanded, setAccessibilityExpanded] = useState(false);
  const [extractionExpanded, setExtractionExpanded] = useState(true);
  const [fingerprintExpanded, setFingerprintExpanded] = useState(true);
  const [copiedSelector, setCopiedSelector] = useState<string | null>(null);
  const [showElementPreview, setShowElementPreview] = useState(false);
  const [largePreviewData, setLargePreviewData] = useState<string | null>(null);
  const [isLoadingPreview, setIsLoadingPreview] = useState(false);
  const [detailPanePreview, setDetailPanePreview] = useState<string | null>(null);

  // Fingerprint state discovery
  const [isRunningDiscovery, setIsRunningDiscovery] = useState(false);
  const [discoveryResult, setDiscoveryResult] = useState<
    import("./StateDiscoveryPanel").FingerprintDiscoveryResult | null
  >(null);
  const [_discoveryError, setDiscoveryError] = useState<string | null>(null);

  // Auto-exploration state
  const [isExploring, setIsExploring] = useState(false);
  const [isStopping, setIsStopping] = useState(false);
  const [wasStopped, setWasStopped] = useState(false);
  const [_explorationProgress, setExplorationProgress] = useState<string | null>(null);
  const [explorationResult, setExplorationResult] = useState<ExplorationResult | null>(null);
  const [showExplorationDialog, setShowExplorationDialog] = useState(false);

  // Convert elements for existing components
  const uiBridgeElements = bridge.elements.map(convertToUIBridgeElement);

  // Generate thumbnails from screenshot
  const {
    thumbnails,
    isProcessing: isProcessingThumbnails,
    progress: thumbnailProgress,
  } = useElementThumbnails(bridge.elements, bridge.pageScreenshot?.data ?? null);

  // Generate larger preview for detail pane when element is selected
  useEffect(() => {
    if (!bridge.selectedElement || !bridge.pageScreenshot?.data) {
      setDetailPanePreview(null);
      return;
    }

    let cancelled = false;

    const generatePreview = async () => {
      try {
        // Crop a medium-sized preview (256px) for the detail pane
        const preview = await cropPreview(
          bridge.pageScreenshot!.data,
          bridge.selectedElement!.bounds,
          256,
        );
        if (!cancelled && preview) {
          setDetailPanePreview(preview);
        }
      } catch (err) {
        console.error("[ExternalUIBridgeInspector] Failed to generate detail pane preview:", err);
      }
    };

    generatePreview();

    return () => {
      cancelled = true;
    };
  }, [bridge.selectedElement, bridge.pageScreenshot]);

  // Create snapshot for element tree (prefixed with _ as it's for future use)
  const _snapshot: UIBridgeSnapshot | null =
    bridge.connectionStatus === "connected"
      ? {
          elements: uiBridgeElements,
          states: [],
          transitions: [],
          activeStates: [],
          timestamp: Date.now(),
        }
      : null;

  // Add event to timeline
  const addEvent = useCallback(
    (
      eventType: string,
      details: Partial<Omit<UIBridgeEvent, "id" | "timestamp" | "eventType">>,
    ) => {
      const newEvent: UIBridgeEvent = {
        id: ++eventIdRef.current,
        timestamp: Date.now(),
        eventType,
        success: true,
        ...details,
      };
      setEvents((prev) => [newEvent, ...prev].slice(0, 100));
    },
    [],
  );

  // Handle refresh
  const handleRefresh = useCallback(async () => {
    await bridge.refreshElements();
    addEvent("element_discovered", {
      result: { count: bridge.elements.length },
    });
  }, [bridge, addEvent]);

  // Handle element selection
  const handleSelectElement = useCallback(
    (elementId: string | null) => {
      bridge.selectElement(elementId);
      if (elementId) {
        addEvent("element_selected", { elementId });
      }
    },
    [bridge, addEvent],
  );

  // Handle action execution
  const handleExecuteAction = useCallback(
    async (elementId: string, action: string, params?: Record<string, unknown>) => {
      const startTime = Date.now();

      const result = await bridge.executeAction(elementId, action, params);

      addEvent("action_executed", {
        elementId,
        action,
        params,
        result: result as unknown as Record<string, unknown>,
        durationMs: Date.now() - startTime,
        success: result.success,
        errorMessage: result.error,
      });

      return result;
    },
    [bridge, addEvent],
  );

  // Handle picker toggle
  const handleTogglePicker = useCallback(
    async (enabled: boolean) => {
      setPickerActive(enabled);
      if (enabled) {
        await bridge.enablePicker();
        addEvent("picker_enabled", {});
      } else {
        bridge.disablePicker();
        addEvent("picker_disabled", {});
      }
    },
    [bridge, addEvent],
  );

  // Handle overlays toggle
  const handleToggleOverlays = useCallback((enabled: boolean) => {
    setOverlaysEnabled(enabled);
    // In a full implementation, this would send a command to show/hide overlays
  }, []);

  // Handle copy element ID
  const handleCopyId = useCallback(async (elementId: string) => {
    try {
      await navigator.clipboard.writeText(elementId);
      setCopiedId(elementId);
      setTimeout(() => setCopiedId(null), 2000);
    } catch {
      // Ignore errors
    }
  }, []);

  // Handle copy selector/xpath
  const handleCopySelector = useCallback(async (value: string, type: string) => {
    try {
      await navigator.clipboard.writeText(value);
      setCopiedSelector(type);
      setTimeout(() => setCopiedSelector(null), 2000);
    } catch {
      // Ignore errors
    }
  }, []);

  // Handle opening the large preview modal
  const handleOpenPreview = useCallback(async () => {
    if (!bridge.selectedElement || !bridge.pageScreenshot?.data) {
      return;
    }

    setIsLoadingPreview(true);
    setShowElementPreview(true);

    try {
      // Crop a larger preview (max 600px) from the original screenshot
      const largePreview = await cropPreview(
        bridge.pageScreenshot.data,
        bridge.selectedElement.bounds,
        600,
      );
      setLargePreviewData(largePreview);
    } catch (err) {
      console.error("[ExternalUIBridgeInspector] Failed to crop large preview:", err);
      // Fall back to thumbnail
      setLargePreviewData(thumbnails.get(bridge.selectedElement.id) ?? null);
    } finally {
      setIsLoadingPreview(false);
    }
  }, [bridge.selectedElement, bridge.pageScreenshot?.data, thumbnails]);

  // Handle closing the preview modal
  const handleClosePreview = useCallback(() => {
    setShowElementPreview(false);
    setLargePreviewData(null);
  }, []);

  // Handle export AI context
  const handleExportAIContext = useCallback(async () => {
    try {
      const aiContext = generateAIContext(bridge.elements, {
        pageTitle: bridge.connectedTabInfo?.title,
        pageUrl: bridge.connectedTabInfo?.url,
        interactiveOnly: true,
      });
      await navigator.clipboard.writeText(aiContext);
      setAiContextCopied(true);
      setTimeout(() => setAiContextCopied(false), 2000);
      addEvent("ai_context_exported", {
        result: { elementCount: bridge.elements.length },
      });
    } catch {
      // Ignore errors
    }
  }, [bridge.elements, bridge.connectedTabInfo, addEvent]);

  // Handle running fingerprint state discovery via Python library
  const handleRunDiscovery = useCallback(
    async (cooccurrenceData: import("../../hooks/useExternalUIBridge").CooccurrenceExport) => {
      setIsRunningDiscovery(true);
      setDiscoveryError(null);

      try {
        // Call the Tauri command to run discovery via Python
        const response = await invoke<{
          success: boolean;
          message?: string;
          data?: import("./StateDiscoveryPanel").FingerprintDiscoveryResult;
        }>("ui_bridge_discover_states_from_fingerprints", {
          cooccurrenceExport: cooccurrenceData,
          config: {
            minCooccurrenceRate: 0.95,
          },
        });

        if (response.success && response.data) {
          setDiscoveryResult(response.data);
          addEvent("fingerprint_discovery_completed", {
            result: {
              statesFound: response.data.states.length,
              transitionsFound: response.data.transitions.length,
            },
          });
          return response.data;
        } else {
          const errorMsg = response.message || "Discovery failed";
          setDiscoveryError(errorMsg);
          addEvent("fingerprint_discovery_failed", { result: { error: errorMsg } });
          return null;
        }
      } catch (err) {
        const errorMsg = err instanceof Error ? err.message : "Unknown error";
        setDiscoveryError(errorMsg);
        addEvent("fingerprint_discovery_failed", { result: { error: errorMsg } });
        console.error("[ExternalUIBridgeInspector] Discovery failed:", err);
        return null;
      } finally {
        setIsRunningDiscovery(false);
      }
    },
    [addEvent],
  );

  // Handle stopping exploration
  const handleStopExploration = useCallback(async () => {
    setIsStopping(true);
    try {
      addEvent("exploration_stopping", { result: {} });
      await invoke("ui_bridge_stop_exploration");
      // The exploration will complete with partial results
      // The finally block in handleRunExploration will handle cleanup
    } catch (err) {
      console.error("[ExternalUIBridgeInspector] Failed to stop exploration:", err);
      // Even if stop fails, the exploration might still be running
    }
  }, [addEvent]);

  // Handle running auto-exploration with config
  const handleRunExploration = useCallback(
    async (config: ExplorationConfig): Promise<ExplorationResult | null> => {
      setIsExploring(true);
      setIsStopping(false);
      setWasStopped(false);
      setExplorationProgress("Starting exploration...");

      try {
        addEvent("exploration_started", { result: { config } });

        // Call the Tauri command to run exploration via Python
        const response = await invoke<{
          success: boolean;
          message?: string;
          data?: {
            exploration_id: string;
            elements_discovered: number;
            elements_explored: number;
            errors: string[];
            state_discovery_result?: import("./StateDiscoveryPanel").FingerprintDiscoveryResult;
            cooccurrence_export?: unknown;
          };
        }>("ui_bridge_run_exploration", {
          runnerUrl: "http://localhost:9876",
          config: {
            maxDepth: config.maxDepth,
            maxElementsPerPage: config.maxElementsPerPage,
            maxTotalElements: config.maxTotalElements,
            actionDelayMs: config.actionDelayMs,
            blockedKeywords: config.blockedKeywords,
            safeKeywords: config.safeKeywords,
            captureScreenshots: config.captureScreenshots,
          },
        });

        if (response.success && response.data) {
          const result: ExplorationResult = {
            explorationId: response.data.exploration_id,
            elementsDiscovered: response.data.elements_discovered,
            elementsExplored: response.data.elements_explored,
            errors: response.data.errors,
            stateDiscoveryResult: response.data.state_discovery_result,
          };
          setExplorationResult(result);

          // Also set discovery result if available
          if (response.data.state_discovery_result) {
            setDiscoveryResult(response.data.state_discovery_result);
          }

          addEvent("exploration_completed", {
            result: {
              elementsDiscovered: result.elementsDiscovered,
              elementsExplored: result.elementsExplored,
              statesFound: result.stateDiscoveryResult?.states.length ?? 0,
              errors: result.errors.length,
            },
          });

          // Refresh elements after exploration
          await bridge.refreshElements();

          // Switch to States tab if we discovered states
          if (result.stateDiscoveryResult && result.stateDiscoveryResult.states.length > 0) {
            setActiveTab("states");
          }

          return result;
        } else {
          const errorMsg = response.message || "Exploration failed";
          addEvent("exploration_failed", { result: { error: errorMsg } });
          const result: ExplorationResult = {
            explorationId: "",
            elementsDiscovered: 0,
            elementsExplored: 0,
            errors: [errorMsg],
          };
          setExplorationResult(result);
          return result;
        }
      } catch (err) {
        const errorMsg = err instanceof Error ? err.message : "Unknown error";
        addEvent("exploration_failed", { result: { error: errorMsg } });
        console.error("[ExternalUIBridgeInspector] Exploration failed:", err);
        const result: ExplorationResult = {
          explorationId: "",
          elementsDiscovered: 0,
          elementsExplored: 0,
          errors: [errorMsg],
        };
        setExplorationResult(result);
        return result;
      } finally {
        // If we were stopping, mark the result as partial
        if (isStopping) {
          setWasStopped(true);
        }
        setIsExploring(false);
        setIsStopping(false);
        setExplorationProgress(null);
      }
    },
    [addEvent, bridge, isStopping],
  );

  // Track previous connection status to only log once per connection
  const prevConnectionStatus = useRef(bridge.connectionStatus);

  // Log connection status changes - only when transitioning TO connected
  useEffect(() => {
    if (bridge.connectionStatus === "connected" && prevConnectionStatus.current !== "connected") {
      addEvent("navigation_completed", {
        result: {
          elementCount: bridge.elements.length,
          url: bridge.connectedTabInfo?.url,
        },
      });
    }
    prevConnectionStatus.current = bridge.connectionStatus;
  }, [bridge.connectionStatus, bridge.elements.length, bridge.connectedTabInfo?.url, addEvent]);

  // Get selected element for action executor
  const selectedUIBridgeElement = bridge.selectedElementId
    ? uiBridgeElements.find((el) => el.id === bridge.selectedElementId)
    : undefined;

  const tabs: { id: TabId; label: string; icon: React.ReactNode; badge?: number }[] = [
    {
      id: "elements",
      label: "Elements",
      icon: <Layers className="w-4 h-4" />,
      badge: bridge.elements.length,
    },
    {
      id: "states",
      label: "States",
      icon: <Network className="w-4 h-4" />,
      badge: bridge.cooccurrenceData?.stateCandidates.length,
    },
    {
      id: "search",
      label: "Search",
      icon: <Sparkles className="w-4 h-4" />,
    },
    {
      id: "describe",
      label: "Describe",
      icon: <Bot className="w-4 h-4" />,
    },
    {
      id: "nl",
      label: "Natural Language",
      icon: <MessageSquare className="w-4 h-4" />,
    },
    {
      id: "events",
      label: "Events",
      icon: <Activity className="w-4 h-4" />,
      badge: events.length,
    },
    { id: "actions", label: "Actions", icon: <Play className="w-4 h-4" /> },
    { id: "api", label: "Raw API", icon: <Terminal className="w-4 h-4" /> },
  ];

  return (
    <div className="flex flex-col h-full">
      {/* Connection Panel */}
      <ConnectionPanel
        connectionStatus={bridge.connectionStatus}
        isExtensionConnected={bridge.isExtensionConnected}
        connectedTabInfo={bridge.connectedTabInfo}
        browserTabs={bridge.browserTabs}
        isLoadingTabs={bridge.isLoadingTabs}
        error={bridge.error}
        elementCount={bridge.elements.length}
        onRefreshTabs={bridge.refreshTabs}
        onConnectToTab={bridge.connectToTab}
        onDisconnect={bridge.disconnect}
        onCheckExtension={bridge.checkExtensionStatus}
      />

      {/* Toolbar */}
      {bridge.connectionStatus === "connected" && (
        <div className="flex items-center gap-2 mb-3">
          <Button
            variant="outline"
            size="sm"
            onClick={handleRefresh}
            disabled={bridge.isLoadingElements}
            title="Refresh elements"
          >
            <RefreshCw className={`w-4 h-4 ${bridge.isLoadingElements ? "animate-spin" : ""}`} />
          </Button>
          <Button
            variant={pickerActive ? "primary" : "outline"}
            size="sm"
            onClick={() => handleTogglePicker(!pickerActive)}
            title={pickerActive ? "Stop picker" : "Start element picker"}
          >
            <Crosshair className="w-4 h-4" />
          </Button>
          <Button
            variant={overlaysEnabled ? "primary" : "outline"}
            size="sm"
            onClick={() => handleToggleOverlays(!overlaysEnabled)}
            title={overlaysEnabled ? "Hide overlays" : "Show element overlays"}
          >
            {overlaysEnabled ? <EyeOff className="w-4 h-4" /> : <Eye className="w-4 h-4" />}
          </Button>

          {/* Screenshot Mode Toggle */}
          <div className="h-4 w-px bg-border mx-1" />
          <Button
            variant={bridge.useFullPageCapture ? "primary" : "outline"}
            size="sm"
            onClick={() => bridge.setUseFullPageCapture(!bridge.useFullPageCapture)}
            disabled={bridge.isCapturingFullPage}
            title={
              bridge.useFullPageCapture
                ? "Full-page capture enabled (slower, captures entire page)"
                : "Viewport capture (faster, captures only visible area)"
            }
          >
            {bridge.isCapturingFullPage ? (
              <Loader2 className="w-4 h-4 animate-spin" />
            ) : bridge.useFullPageCapture ? (
              <Maximize2 className="w-4 h-4" />
            ) : (
              <Monitor className="w-4 h-4" />
            )}
          </Button>
          {bridge.fullPageScreenshot && bridge.useFullPageCapture && (
            <span className="text-[10px] text-muted-foreground">
              {bridge.fullPageScreenshot.totalWidth}x{bridge.fullPageScreenshot.totalHeight}
            </span>
          )}

          {/* Full-page capture progress indicator */}
          {bridge.fullPageCaptureProgress && (
            <div className="flex items-center gap-2 px-2 py-1 bg-muted/50 rounded-md text-xs">
              <Loader2 className="w-3 h-3 animate-spin text-primary" />
              <span className="text-muted-foreground">
                {bridge.fullPageCaptureProgress.phase === "capturing"
                  ? `Capturing: ${bridge.fullPageCaptureProgress.currentTile}/${bridge.fullPageCaptureProgress.totalTiles}`
                  : bridge.fullPageCaptureProgress.phase === "stitching"
                    ? "Stitching tiles..."
                    : "Completing..."}
              </span>
              {bridge.fullPageCaptureProgress.totalTiles > 0 && (
                <div className="w-16 h-1.5 bg-muted rounded-full overflow-hidden">
                  <div
                    className="h-full bg-primary transition-all duration-150"
                    style={{
                      width: `${(bridge.fullPageCaptureProgress.currentTile / bridge.fullPageCaptureProgress.totalTiles) * 100}%`,
                    }}
                  />
                </div>
              )}
            </div>
          )}

          <Button
            variant="outline"
            size="sm"
            onClick={handleExportAIContext}
            disabled={bridge.elements.length === 0}
            title="Export AI context to clipboard"
          >
            {aiContextCopied ? (
              <Check className="w-4 h-4 text-green-500" />
            ) : (
              <FileText className="w-4 h-4" />
            )}
            <span className="ml-1.5 text-xs">{aiContextCopied ? "Copied!" : "AI Context"}</span>
          </Button>

          {/* Capture Session Controls */}
          <div className="h-4 w-px bg-border mx-1" />
          {bridge.captureSession.active ? (
            <>
              <Button
                variant="danger"
                size="sm"
                onClick={async () => {
                  const session = await bridge.endCaptureSession();
                  if (session) {
                    // Copy to clipboard as JSON
                    await navigator.clipboard.writeText(JSON.stringify(session, null, 2));
                    console.log("[Session] Ended and copied to clipboard:", session);
                  }
                }}
                title="Stop capture session and copy data to clipboard"
              >
                <Square className="w-3 h-3 mr-1" />
                <span className="text-xs">Stop</span>
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={async () => {
                  const session = await bridge.exportCaptureSession();
                  if (session) {
                    await navigator.clipboard.writeText(JSON.stringify(session, null, 2));
                    console.log("[Session] Exported to clipboard:", session);
                  }
                }}
                title="Export raw session data to clipboard"
              >
                <Download className="w-3 h-3" />
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={async () => {
                  const cooccurrence = await bridge.generateCooccurrenceExport();
                  if (cooccurrence) {
                    await navigator.clipboard.writeText(JSON.stringify(cooccurrence, null, 2));
                    console.log(
                      "[Session] Co-occurrence export:",
                      cooccurrence.allFingerprints.length,
                      "fingerprints,",
                      cooccurrence.stateCandidates.length,
                      "state candidates",
                    );
                  }
                }}
                title="Export co-occurrence analysis for state discovery"
              >
                <Network className="w-3 h-3" />
              </Button>
              <div className="flex items-center gap-1 text-xs">
                <Circle className="w-2 h-2 fill-red-500 text-red-500 animate-pulse" />
                <span className="text-muted-foreground">
                  {bridge.captureSession.captureCount || 0} caps
                </span>
                <span className="text-muted-foreground">·</span>
                <span className="text-muted-foreground">
                  {bridge.captureSession.actionCount || 0} acts
                </span>
                <span className="text-muted-foreground">·</span>
                <span className="text-muted-foreground">
                  {bridge.captureSession.uniqueFingerprints || 0} fps
                </span>
              </div>
            </>
          ) : (
            <Button
              variant="outline"
              size="sm"
              onClick={() => bridge.startCaptureSession()}
              title="Start capture session for state machine discovery"
            >
              <Circle className="w-3 h-3 mr-1 text-red-500" />
              <span className="text-xs">Capture</span>
            </Button>
          )}

          {/* Auto-Explore Button */}
          <div className="h-4 w-px bg-border mx-1" />
          <Button
            variant={isExploring ? "primary" : "outline"}
            size="sm"
            onClick={() => setShowExplorationDialog(true)}
            disabled={bridge.connectionStatus !== "connected" || isExploring}
            title="Automatically explore interactive elements and discover states"
          >
            {isExploring ? (
              <>
                <Loader2 className="w-3 h-3 mr-1 animate-spin" />
                <span className="text-xs">Exploring...</span>
              </>
            ) : (
              <>
                <Compass className="w-3 h-3 mr-1" />
                <span className="text-xs">Auto-Explore</span>
              </>
            )}
          </Button>
          {explorationResult && !isExploring && (
            <button
              onClick={() => setShowExplorationDialog(true)}
              className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
              title="Click to view details or run again"
            >
              <span>
                {explorationResult.elementsExplored}/{explorationResult.elementsDiscovered} explored
              </span>
              {explorationResult.stateDiscoveryResult && (
                <>
                  <span>·</span>
                  <span className="text-primary">
                    {explorationResult.stateDiscoveryResult.states.length} states
                  </span>
                </>
              )}
              {explorationResult.errors.length > 0 && (
                <>
                  <span>·</span>
                  <span className="text-destructive">{explorationResult.errors.length} errors</span>
                </>
              )}
            </button>
          )}

          <div className="flex-1" />
          <div className="text-xs text-muted-foreground">{bridge.elements.length} elements</div>
        </div>
      )}

      {/* Tabs */}
      <div className="flex gap-1 mb-3 p-1 bg-muted/30 rounded-lg">
        {tabs.map((tab) => (
          <button
            key={tab.id}
            onClick={() => setActiveTab(tab.id)}
            className={`flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
              activeTab === tab.id
                ? "bg-primary text-primary-foreground"
                : "text-muted-foreground hover:text-foreground hover:bg-muted/50"
            }`}
          >
            {tab.icon}
            {tab.label}
            {tab.badge !== undefined && tab.badge > 0 && (
              <Badge
                variant={activeTab === tab.id ? "default" : "muted"}
                className="text-[10px] px-1 py-0 ml-1"
              >
                {tab.badge}
              </Badge>
            )}
          </button>
        ))}
      </div>

      {/* Tab Content */}
      <div className="flex-1 overflow-hidden">
        {activeTab === "elements" && (
          <div className="h-full flex flex-col">
            {bridge.connectionStatus === "connected" ? (
              <>
                {/* Full-page capture progress indicator */}
                {bridge.isCapturingFullPage && (
                  <div className="flex items-center gap-2 mb-2 px-2 py-1.5 bg-blue-500/10 border border-blue-500/20 rounded-md text-xs text-blue-400">
                    <Loader2 className="w-3 h-3 animate-spin" />
                    <Maximize2 className="w-3 h-3" />
                    <span>Capturing full page screenshot...</span>
                  </div>
                )}
                {/* Thumbnail generation progress indicator */}
                {isProcessingThumbnails && thumbnailProgress && !bridge.isCapturingFullPage && (
                  <div className="flex items-center gap-2 mb-2 px-2 py-1.5 bg-muted/50 rounded-md text-xs text-muted-foreground">
                    <Loader2 className="w-3 h-3 animate-spin" />
                    <span>
                      Generating thumbnails: {thumbnailProgress.processed}/{thumbnailProgress.total}
                    </span>
                    <div className="flex-1 h-1 bg-muted rounded-full overflow-hidden">
                      <div
                        className="h-full bg-primary transition-all duration-150"
                        style={{ width: `${thumbnailProgress.percentage}%` }}
                      />
                    </div>
                    <span className="font-mono">{thumbnailProgress.percentage}%</span>
                  </div>
                )}
                <ElementTreeView
                  elements={uiBridgeElements}
                  states={[]}
                  activeStates={[]}
                  selectedElementId={bridge.selectedElementId}
                  onSelectElement={handleSelectElement}
                  loading={bridge.isLoadingElements}
                  thumbnails={thumbnails}
                  isLoadingThumbnails={
                    isProcessingThumbnails ||
                    bridge.isCapturingScreenshot ||
                    bridge.isCapturingFullPage
                  }
                  screenshotData={bridge.pageScreenshot?.data}
                />

                {/* Enhanced element details with copy */}
                {bridge.selectedElement && (
                  <div className="mt-2 p-3 bg-muted/30 rounded-md border border-border/50 overflow-y-auto max-h-[400px]">
                    {/* Header with type, ref, and ID */}
                    <div className="flex items-center justify-between mb-2">
                      <div className="flex items-center gap-2">
                        <Badge variant="default">{bridge.selectedElement.type}</Badge>
                        {bridge.selectedElement.ref && (
                          <Badge variant="info" className="font-mono">
                            {bridge.selectedElement.ref}
                          </Badge>
                        )}
                        {bridge.selectedElement.role && (
                          <Badge
                            variant={
                              bridge.selectedElement.accessibility?.hasExplicitRole
                                ? "purple"
                                : "muted"
                            }
                            title={
                              bridge.selectedElement.accessibility?.hasExplicitRole
                                ? "Explicit ARIA role"
                                : "Implicit role from element type"
                            }
                          >
                            {bridge.selectedElement.role}
                            <span className="ml-1 opacity-60 text-[9px]">
                              (
                              {bridge.selectedElement.accessibility?.hasExplicitRole
                                ? "explicit"
                                : "implicit"}
                              )
                            </span>
                          </Badge>
                        )}
                        <span className="font-mono text-sm truncate">
                          {bridge.selectedElement.id}
                        </span>
                      </div>
                      <button
                        className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
                        onClick={() => handleCopyId(bridge.selectedElement!.id)}
                      >
                        {copiedId === bridge.selectedElement.id ? (
                          <>
                            <Check className="w-3 h-3" />
                            Copied
                          </>
                        ) : (
                          <>
                            <Copy className="w-3 h-3" />
                            Copy ID
                          </>
                        )}
                      </button>
                    </div>

                    {/* Basic properties grid */}
                    <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
                      <div className="text-muted-foreground">Tag:</div>
                      <div className="font-mono">{bridge.selectedElement.tagName}</div>

                      {bridge.selectedElement.label && (
                        <>
                          <div className="text-muted-foreground">Label:</div>
                          <div className="truncate">{bridge.selectedElement.label}</div>
                        </>
                      )}

                      {/* Show accessible name if different from label/text */}
                      {bridge.selectedElement.accessibleName &&
                        bridge.selectedElement.accessibleName !== bridge.selectedElement.label &&
                        bridge.selectedElement.accessibleName !== bridge.selectedElement.text && (
                          <>
                            <div className="text-muted-foreground">Accessible Name:</div>
                            <div className="truncate text-blue-400">
                              {bridge.selectedElement.accessibleName}
                            </div>
                          </>
                        )}

                      {bridge.selectedElement.text && (
                        <>
                          <div className="text-muted-foreground">Text:</div>
                          <div className="truncate">{bridge.selectedElement.text}</div>
                        </>
                      )}

                      {bridge.selectedElement.value !== undefined && (
                        <>
                          <div className="text-muted-foreground">Value:</div>
                          <div className="font-mono truncate">
                            {bridge.selectedElement.value || "(empty)"}
                          </div>
                        </>
                      )}

                      {bridge.selectedElement.placeholder && (
                        <>
                          <div className="text-muted-foreground">Placeholder:</div>
                          <div className="truncate">{bridge.selectedElement.placeholder}</div>
                        </>
                      )}

                      <div className="text-muted-foreground">Visible:</div>
                      <div>{bridge.selectedElement.visible ? "Yes" : "No"}</div>

                      <div className="text-muted-foreground">Enabled:</div>
                      <div>{bridge.selectedElement.enabled ? "Yes" : "No"}</div>

                      <div className="text-muted-foreground">Has data-ui-id:</div>
                      <div>{bridge.selectedElement.hasUiId ? "Yes" : "No"}</div>

                      <div className="text-muted-foreground">Position:</div>
                      <div className="font-mono">
                        {bridge.selectedElement.bounds.x.toFixed(0)},{" "}
                        {bridge.selectedElement.bounds.y.toFixed(0)}
                      </div>

                      <div className="text-muted-foreground">Size:</div>
                      <div className="font-mono">
                        {bridge.selectedElement.bounds.width.toFixed(0)} x{" "}
                        {bridge.selectedElement.bounds.height.toFixed(0)}
                      </div>

                      {bridge.selectedElement.actions.length > 0 && (
                        <>
                          <div className="text-muted-foreground">Actions:</div>
                          <div className="flex flex-wrap gap-1">
                            {bridge.selectedElement.actions.map((action) => (
                              <Badge key={action} variant="muted" className="text-[10px]">
                                {action}
                              </Badge>
                            ))}
                          </div>
                        </>
                      )}
                    </div>

                    {/* Accessibility States Section - Collapsible */}
                    <div className="mt-3 pt-2 border-t border-border/50">
                      <button
                        className="flex items-center gap-2 text-xs font-medium text-muted-foreground hover:text-foreground w-full"
                        onClick={() => setAccessibilityExpanded(!accessibilityExpanded)}
                      >
                        {accessibilityExpanded ? (
                          <ChevronDown className="w-3.5 h-3.5" />
                        ) : (
                          <ChevronRight className="w-3.5 h-3.5" />
                        )}
                        <Accessibility className="w-3.5 h-3.5" />
                        Accessibility States
                        {/* Quick indicators */}
                        <div className="flex gap-1 ml-auto">
                          {bridge.selectedElement.is_interactive && (
                            <Badge variant="success" size="sm" title="Interactive element">
                              <Pointer className="w-3 h-3" />
                            </Badge>
                          )}
                          {bridge.selectedElement.accessibility?.isKeyboardAccessible && (
                            <Badge variant="info" size="sm" title="Keyboard accessible">
                              <Keyboard className="w-3 h-3" />
                            </Badge>
                          )}
                        </div>
                      </button>

                      {accessibilityExpanded && (
                        <div className="mt-2 pl-5 space-y-2">
                          {/* Interaction States */}
                          <div className="flex flex-wrap gap-1.5">
                            <Badge
                              variant={bridge.selectedElement.is_interactive ? "success" : "muted"}
                              size="sm"
                            >
                              <Pointer className="w-3 h-3 mr-1" />
                              Interactive: {bridge.selectedElement.is_interactive ? "Yes" : "No"}
                            </Badge>

                            {bridge.selectedElement.is_expanded !== undefined && (
                              <Badge
                                variant={bridge.selectedElement.is_expanded ? "success" : "muted"}
                                size="sm"
                              >
                                Expanded: {bridge.selectedElement.is_expanded ? "Yes" : "No"}
                              </Badge>
                            )}

                            {bridge.selectedElement.is_pressed !== undefined && (
                              <Badge
                                variant={bridge.selectedElement.is_pressed ? "success" : "muted"}
                                size="sm"
                              >
                                Pressed: {bridge.selectedElement.is_pressed ? "Yes" : "No"}
                              </Badge>
                            )}

                            {bridge.selectedElement.is_selected !== undefined && (
                              <Badge
                                variant={bridge.selectedElement.is_selected ? "success" : "muted"}
                                size="sm"
                              >
                                Selected: {bridge.selectedElement.is_selected ? "Yes" : "No"}
                              </Badge>
                            )}
                          </div>

                          {/* Form States */}
                          {(bridge.selectedElement.is_required ||
                            bridge.selectedElement.is_readonly) && (
                            <div className="flex flex-wrap gap-1.5">
                              {bridge.selectedElement.is_required && (
                                <Badge variant="warning" size="sm">
                                  Required
                                </Badge>
                              )}
                              {bridge.selectedElement.is_readonly && (
                                <Badge variant="muted" size="sm">
                                  Read-only
                                </Badge>
                              )}
                            </div>
                          )}

                          {/* Keyboard Accessibility */}
                          <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs mt-2">
                            {bridge.selectedElement.accessibility?.tabIndex !== undefined && (
                              <>
                                <div className="text-muted-foreground">Tab Index:</div>
                                <div className="font-mono">
                                  {bridge.selectedElement.accessibility.tabIndex}
                                </div>
                              </>
                            )}

                            <div className="text-muted-foreground">In Tab Order:</div>
                            <div className="flex items-center gap-1">
                              {bridge.selectedElement.accessibility?.isInTabOrder ? (
                                <>
                                  <Check className="w-3 h-3 text-green-500" />
                                  <span className="text-green-500">Yes</span>
                                </>
                              ) : (
                                <span className="text-muted-foreground">No</span>
                              )}
                            </div>

                            <div className="text-muted-foreground">Keyboard Accessible:</div>
                            <div className="flex items-center gap-1">
                              {bridge.selectedElement.accessibility?.isKeyboardAccessible ? (
                                <>
                                  <Keyboard className="w-3 h-3 text-blue-400" />
                                  <span className="text-blue-400">Yes</span>
                                </>
                              ) : (
                                <span className="text-muted-foreground">No</span>
                              )}
                            </div>

                            {/* ARIA attributes */}
                            {bridge.selectedElement.accessibility?.ariaLabel && (
                              <>
                                <div className="text-muted-foreground">aria-label:</div>
                                <div className="truncate">
                                  {bridge.selectedElement.accessibility.ariaLabel}
                                </div>
                              </>
                            )}

                            {bridge.selectedElement.accessibility?.ariaLabelledBy && (
                              <>
                                <div className="text-muted-foreground">aria-labelledby:</div>
                                <div className="font-mono truncate">
                                  {bridge.selectedElement.accessibility.ariaLabelledBy}
                                </div>
                              </>
                            )}

                            {bridge.selectedElement.accessibility?.ariaDescribedBy && (
                              <>
                                <div className="text-muted-foreground">aria-describedby:</div>
                                <div className="font-mono truncate">
                                  {bridge.selectedElement.accessibility.ariaDescribedBy}
                                </div>
                              </>
                            )}

                            {bridge.selectedElement.accessibility?.accessibleDescription && (
                              <>
                                <div className="text-muted-foreground">Description:</div>
                                <div className="truncate">
                                  {bridge.selectedElement.accessibility.accessibleDescription}
                                </div>
                              </>
                            )}

                            {bridge.selectedElement.accessibility?.ariaLive &&
                              bridge.selectedElement.accessibility.ariaLive !== "off" && (
                                <>
                                  <div className="text-muted-foreground">aria-live:</div>
                                  <div>
                                    <Badge variant="info" size="sm">
                                      {bridge.selectedElement.accessibility.ariaLive}
                                    </Badge>
                                  </div>
                                </>
                              )}

                            {bridge.selectedElement.accessibility?.ariaHidden && (
                              <>
                                <div className="text-muted-foreground">aria-hidden:</div>
                                <div className="text-yellow-500">true</div>
                              </>
                            )}
                          </div>
                        </div>
                      )}
                    </div>

                    {/* Extraction Data Section - Collapsible */}
                    <div className="mt-3 pt-2 border-t border-border/50">
                      <button
                        className="flex items-center gap-2 text-xs font-medium text-muted-foreground hover:text-foreground w-full"
                        onClick={() => setExtractionExpanded(!extractionExpanded)}
                      >
                        {extractionExpanded ? (
                          <ChevronDown className="w-3.5 h-3.5" />
                        ) : (
                          <ChevronRight className="w-3.5 h-3.5" />
                        )}
                        <Code className="w-3.5 h-3.5" />
                        Extraction Data
                        {/* Quick indicators */}
                        <div className="flex gap-1 ml-auto">
                          {bridge.selectedElement.selector && (
                            <Badge variant="success" size="sm" title="Has CSS Selector">
                              CSS
                            </Badge>
                          )}
                          {bridge.selectedElement.xpath && (
                            <Badge variant="info" size="sm" title="Has XPath">
                              XPath
                            </Badge>
                          )}
                        </div>
                      </button>

                      {extractionExpanded && (
                        <div className="mt-2 pl-5 space-y-3">
                          {/* Selectors */}
                          <div className="space-y-2">
                            {bridge.selectedElement.selector && (
                              <div className="space-y-1">
                                <div className="flex items-center justify-between">
                                  <span className="text-xs text-muted-foreground flex items-center gap-1">
                                    <Code className="w-3 h-3" />
                                    CSS Selector:
                                  </span>
                                  <button
                                    className="flex items-center gap-1 text-[10px] text-muted-foreground hover:text-foreground"
                                    onClick={() =>
                                      handleCopySelector(
                                        bridge.selectedElement!.selector!,
                                        "selector",
                                      )
                                    }
                                  >
                                    {copiedSelector === "selector" ? (
                                      <>
                                        <Check className="w-2.5 h-2.5" />
                                        Copied
                                      </>
                                    ) : (
                                      <>
                                        <Copy className="w-2.5 h-2.5" />
                                        Copy
                                      </>
                                    )}
                                  </button>
                                </div>
                                <div className="font-mono text-[10px] bg-muted/50 px-2 py-1 rounded break-all select-all">
                                  {bridge.selectedElement.selector}
                                </div>
                              </div>
                            )}

                            {bridge.selectedElement.xpath && (
                              <div className="space-y-1">
                                <div className="flex items-center justify-between">
                                  <span className="text-xs text-muted-foreground flex items-center gap-1">
                                    <Code className="w-3 h-3" />
                                    XPath:
                                  </span>
                                  <button
                                    className="flex items-center gap-1 text-[10px] text-muted-foreground hover:text-foreground"
                                    onClick={() =>
                                      handleCopySelector(bridge.selectedElement!.xpath!, "xpath")
                                    }
                                  >
                                    {copiedSelector === "xpath" ? (
                                      <>
                                        <Check className="w-2.5 h-2.5" />
                                        Copied
                                      </>
                                    ) : (
                                      <>
                                        <Copy className="w-2.5 h-2.5" />
                                        Copy
                                      </>
                                    )}
                                  </button>
                                </div>
                                <div className="font-mono text-[10px] bg-muted/50 px-2 py-1 rounded break-all select-all">
                                  {bridge.selectedElement.xpath}
                                </div>
                              </div>
                            )}
                          </div>

                          {/* CSS Classes */}
                          {bridge.selectedElement.classes &&
                            bridge.selectedElement.classes.length > 0 && (
                              <div className="space-y-1">
                                <span className="text-xs text-muted-foreground flex items-center gap-1">
                                  <Palette className="w-3 h-3" />
                                  CSS Classes:
                                </span>
                                <div className="flex flex-wrap gap-1">
                                  {bridge.selectedElement.classes.map((cls, i) => (
                                    <Badge
                                      key={i}
                                      variant="muted"
                                      className="text-[10px] font-mono"
                                    >
                                      .{cls}
                                    </Badge>
                                  ))}
                                </div>
                              </div>
                            )}

                          {/* Link/Image Attributes */}
                          {(bridge.selectedElement.href ||
                            bridge.selectedElement.src ||
                            bridge.selectedElement.alt) && (
                            <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
                              {bridge.selectedElement.href && (
                                <>
                                  <div className="text-muted-foreground flex items-center gap-1">
                                    <Link className="w-3 h-3" />
                                    href:
                                  </div>
                                  <div
                                    className="font-mono truncate text-blue-400"
                                    title={bridge.selectedElement.href}
                                  >
                                    {bridge.selectedElement.href}
                                  </div>
                                </>
                              )}
                              {bridge.selectedElement.src && (
                                <>
                                  <div className="text-muted-foreground flex items-center gap-1">
                                    <Image className="w-3 h-3" />
                                    src:
                                  </div>
                                  <div
                                    className="font-mono truncate"
                                    title={bridge.selectedElement.src}
                                  >
                                    {bridge.selectedElement.src}
                                  </div>
                                </>
                              )}
                              {bridge.selectedElement.alt && (
                                <>
                                  <div className="text-muted-foreground">alt:</div>
                                  <div className="truncate">{bridge.selectedElement.alt}</div>
                                </>
                              )}
                              {bridge.selectedElement.title && (
                                <>
                                  <div className="text-muted-foreground">title:</div>
                                  <div className="truncate">{bridge.selectedElement.title}</div>
                                </>
                              )}
                            </div>
                          )}

                          {/* Input Attributes */}
                          {(bridge.selectedElement.inputType || bridge.selectedElement.name) && (
                            <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
                              {bridge.selectedElement.inputType && (
                                <>
                                  <div className="text-muted-foreground">Input Type:</div>
                                  <div className="font-mono">
                                    {bridge.selectedElement.inputType}
                                  </div>
                                </>
                              )}
                              {bridge.selectedElement.name && (
                                <>
                                  <div className="text-muted-foreground">Name:</div>
                                  <div className="font-mono">{bridge.selectedElement.name}</div>
                                </>
                              )}
                            </div>
                          )}

                          {/* Data Attributes */}
                          {bridge.selectedElement.dataAttributes &&
                            Object.keys(bridge.selectedElement.dataAttributes).length > 0 && (
                              <div className="space-y-1">
                                <span className="text-xs text-muted-foreground flex items-center gap-1">
                                  <Database className="w-3 h-3" />
                                  Data Attributes:
                                </span>
                                <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs bg-muted/30 p-2 rounded">
                                  {Object.entries(bridge.selectedElement.dataAttributes).map(
                                    ([key, value]) => (
                                      <div key={key} className="contents">
                                        <div className="text-muted-foreground font-mono">
                                          data-{key}:
                                        </div>
                                        <div className="font-mono truncate" title={value}>
                                          {value}
                                        </div>
                                      </div>
                                    ),
                                  )}
                                </div>
                              </div>
                            )}

                          {/* Computed Styles */}
                          {bridge.selectedElement.computedStyle && (
                            <div className="space-y-1">
                              <span className="text-xs text-muted-foreground flex items-center gap-1">
                                <Palette className="w-3 h-3" />
                                Computed Styles:
                              </span>
                              <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs bg-muted/30 p-2 rounded">
                                {bridge.selectedElement.computedStyle.color && (
                                  <>
                                    <div className="text-muted-foreground">Color:</div>
                                    <div className="font-mono flex items-center gap-1">
                                      <span
                                        className="w-3 h-3 rounded border border-border/50"
                                        style={{
                                          backgroundColor:
                                            bridge.selectedElement.computedStyle.color,
                                        }}
                                      />
                                      {bridge.selectedElement.computedStyle.color}
                                    </div>
                                  </>
                                )}
                                {bridge.selectedElement.computedStyle.backgroundColor &&
                                  bridge.selectedElement.computedStyle.backgroundColor !==
                                    "rgba(0, 0, 0, 0)" && (
                                    <>
                                      <div className="text-muted-foreground">Background:</div>
                                      <div className="font-mono flex items-center gap-1">
                                        <span
                                          className="w-3 h-3 rounded border border-border/50"
                                          style={{
                                            backgroundColor:
                                              bridge.selectedElement.computedStyle.backgroundColor,
                                          }}
                                        />
                                        {bridge.selectedElement.computedStyle.backgroundColor}
                                      </div>
                                    </>
                                  )}
                                {bridge.selectedElement.computedStyle.fontSize && (
                                  <>
                                    <div className="text-muted-foreground">Font Size:</div>
                                    <div className="font-mono">
                                      {bridge.selectedElement.computedStyle.fontSize}
                                    </div>
                                  </>
                                )}
                                {bridge.selectedElement.computedStyle.fontWeight && (
                                  <>
                                    <div className="text-muted-foreground">Font Weight:</div>
                                    <div className="font-mono">
                                      {bridge.selectedElement.computedStyle.fontWeight}
                                    </div>
                                  </>
                                )}
                                {bridge.selectedElement.computedStyle.display && (
                                  <>
                                    <div className="text-muted-foreground">Display:</div>
                                    <div className="font-mono">
                                      {bridge.selectedElement.computedStyle.display}
                                    </div>
                                  </>
                                )}
                                {bridge.selectedElement.computedStyle.position &&
                                  bridge.selectedElement.computedStyle.position !== "static" && (
                                    <>
                                      <div className="text-muted-foreground">Position:</div>
                                      <div className="font-mono">
                                        {bridge.selectedElement.computedStyle.position}
                                      </div>
                                    </>
                                  )}
                                {bridge.selectedElement.computedStyle.zIndex &&
                                  bridge.selectedElement.computedStyle.zIndex !== "auto" && (
                                    <>
                                      <div className="text-muted-foreground">Z-Index:</div>
                                      <div className="font-mono">
                                        {bridge.selectedElement.computedStyle.zIndex}
                                      </div>
                                    </>
                                  )}
                                {bridge.selectedElement.computedStyle.opacity &&
                                  bridge.selectedElement.computedStyle.opacity !== "1" && (
                                    <>
                                      <div className="text-muted-foreground">Opacity:</div>
                                      <div className="font-mono">
                                        {bridge.selectedElement.computedStyle.opacity}
                                      </div>
                                    </>
                                  )}
                              </div>
                            </div>
                          )}

                          {/* Page Context */}
                          {(bridge.selectedElement.sourceUrl ||
                            bridge.selectedElement.viewportWidth) && (
                            <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
                              {bridge.selectedElement.sourceUrl && (
                                <>
                                  <div className="text-muted-foreground">Source URL:</div>
                                  <div
                                    className="font-mono truncate text-blue-400"
                                    title={bridge.selectedElement.sourceUrl}
                                  >
                                    {bridge.selectedElement.sourceUrl}
                                  </div>
                                </>
                              )}
                              {bridge.selectedElement.viewportWidth &&
                                bridge.selectedElement.viewportHeight && (
                                  <>
                                    <div className="text-muted-foreground">Viewport:</div>
                                    <div className="font-mono">
                                      {bridge.selectedElement.viewportWidth} x{" "}
                                      {bridge.selectedElement.viewportHeight}
                                    </div>
                                  </>
                                )}
                              {bridge.selectedElement.depth !== undefined && (
                                <>
                                  <div className="text-muted-foreground">DOM Depth:</div>
                                  <div className="font-mono">{bridge.selectedElement.depth}</div>
                                </>
                              )}
                              {bridge.selectedElement.tagIndex !== undefined && (
                                <>
                                  <div className="text-muted-foreground">Tag Index:</div>
                                  <div className="font-mono">{bridge.selectedElement.tagIndex}</div>
                                </>
                              )}
                            </div>
                          )}
                        </div>
                      )}
                    </div>

                    {/* Fingerprint Section - Collapsible */}
                    {bridge.selectedElement.fingerprint && (
                      <div className="mt-3 pt-2 border-t border-border/50">
                        <button
                          className="flex items-center gap-2 text-xs font-medium text-muted-foreground hover:text-foreground w-full"
                          onClick={() => setFingerprintExpanded(!fingerprintExpanded)}
                        >
                          {fingerprintExpanded ? (
                            <ChevronDown className="w-3.5 h-3.5" />
                          ) : (
                            <ChevronRight className="w-3.5 h-3.5" />
                          )}
                          <Fingerprint className="w-3.5 h-3.5" />
                          Element Fingerprint
                          {/* Quick indicators */}
                          <div className="flex gap-1 ml-auto">
                            <Badge variant="default" size="sm" className="font-mono text-[9px]">
                              {bridge.selectedElement.fingerprint.hash}
                            </Badge>
                            {bridge.selectedElement.fingerprint.isRepeating && (
                              <Badge variant="info" size="sm" title="Repeating element">
                                <Repeat className="w-3 h-3" />
                              </Badge>
                            )}
                          </div>
                        </button>

                        {fingerprintExpanded && (
                          <div className="mt-2 pl-5 space-y-3">
                            {/* Hash with copy button */}
                            <div className="space-y-1">
                              <div className="flex items-center justify-between">
                                <span className="text-xs text-muted-foreground flex items-center gap-1">
                                  <Hash className="w-3 h-3" />
                                  Fingerprint Hash:
                                </span>
                                <button
                                  className="flex items-center gap-1 text-[10px] text-muted-foreground hover:text-foreground"
                                  onClick={() =>
                                    handleCopySelector(
                                      bridge.selectedElement!.fingerprint!.hash,
                                      "fingerprint",
                                    )
                                  }
                                >
                                  {copiedSelector === "fingerprint" ? (
                                    <>
                                      <Check className="w-2.5 h-2.5" />
                                      Copied
                                    </>
                                  ) : (
                                    <>
                                      <Copy className="w-2.5 h-2.5" />
                                      Copy
                                    </>
                                  )}
                                </button>
                              </div>
                              <div className="font-mono text-sm bg-muted/50 px-2 py-1 rounded select-all">
                                {bridge.selectedElement.fingerprint.hash}
                              </div>
                            </div>

                            {/* Structural Identity */}
                            <div className="space-y-1">
                              <span className="text-xs text-muted-foreground font-medium">
                                Structural Identity
                              </span>
                              <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs bg-muted/30 p-2 rounded">
                                <div className="text-muted-foreground">Path:</div>
                                <div
                                  className="font-mono text-[10px] truncate"
                                  title={bridge.selectedElement.fingerprint.structuralPath}
                                >
                                  {bridge.selectedElement.fingerprint.structuralPath}
                                </div>

                                <div className="text-muted-foreground flex items-center gap-1">
                                  <MapPin className="w-3 h-3" />
                                  Zone:
                                </div>
                                <div>
                                  <Badge variant="muted" size="sm">
                                    {bridge.selectedElement.fingerprint.positionZone}
                                  </Badge>
                                </div>

                                <div className="text-muted-foreground">Landmark:</div>
                                <div className="flex items-center gap-1">
                                  <Badge variant="purple" size="sm">
                                    {bridge.selectedElement.fingerprint.landmarkContext}
                                  </Badge>
                                  {bridge.selectedElement.fingerprint.landmarkLabel && (
                                    <span className="text-[10px] text-muted-foreground truncate">
                                      ({bridge.selectedElement.fingerprint.landmarkLabel})
                                    </span>
                                  )}
                                </div>
                              </div>
                            </div>

                            {/* Semantic Identity */}
                            <div className="space-y-1">
                              <span className="text-xs text-muted-foreground font-medium">
                                Semantic Identity
                              </span>
                              <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs bg-muted/30 p-2 rounded">
                                <div className="text-muted-foreground">Role:</div>
                                <div className="font-mono">
                                  {bridge.selectedElement.fingerprint.role}
                                </div>

                                <div className="text-muted-foreground">Tag:</div>
                                <div className="font-mono">
                                  {bridge.selectedElement.fingerprint.tagName}
                                </div>

                                {bridge.selectedElement.fingerprint.accessibleName && (
                                  <>
                                    <div className="text-muted-foreground">Name (normalized):</div>
                                    <div
                                      className="truncate"
                                      title={bridge.selectedElement.fingerprint.accessibleName}
                                    >
                                      {bridge.selectedElement.fingerprint.accessibleName}
                                    </div>
                                  </>
                                )}
                              </div>
                            </div>

                            {/* Visual Identity */}
                            <div className="space-y-1">
                              <span className="text-xs text-muted-foreground font-medium">
                                Visual Identity
                              </span>
                              <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs bg-muted/30 p-2 rounded">
                                <div className="text-muted-foreground">Size Category:</div>
                                <div>
                                  <Badge variant="muted" size="sm">
                                    {bridge.selectedElement.fingerprint.sizeCategory}
                                  </Badge>
                                </div>

                                <div className="text-muted-foreground">Relative Position:</div>
                                <div className="font-mono">
                                  top:{" "}
                                  {(
                                    bridge.selectedElement.fingerprint.relativePosition.top * 100
                                  ).toFixed(0)}
                                  %, left:{" "}
                                  {(
                                    bridge.selectedElement.fingerprint.relativePosition.left * 100
                                  ).toFixed(0)}
                                  %
                                </div>
                              </div>
                            </div>

                            {/* Repeat Pattern */}
                            {bridge.selectedElement.fingerprint.isRepeating &&
                              bridge.selectedElement.fingerprint.repeatPattern && (
                                <div className="space-y-1">
                                  <span className="text-xs text-muted-foreground font-medium flex items-center gap-1">
                                    <Repeat className="w-3 h-3" />
                                    Repeat Pattern
                                  </span>
                                  <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs bg-blue-500/10 p-2 rounded border border-blue-500/20">
                                    <div className="text-muted-foreground">Type:</div>
                                    <div>
                                      <Badge variant="info" size="sm">
                                        {bridge.selectedElement.fingerprint.repeatPattern.type}
                                      </Badge>
                                    </div>

                                    <div className="text-muted-foreground">Position:</div>
                                    <div className="font-mono">
                                      {bridge.selectedElement.fingerprint.repeatPattern.index + 1}{" "}
                                      of {bridge.selectedElement.fingerprint.repeatPattern.count}
                                    </div>

                                    {bridge.selectedElement.fingerprint.repeatPattern
                                      .itemSelector && (
                                      <>
                                        <div className="text-muted-foreground">Item Selector:</div>
                                        <div
                                          className="font-mono text-[10px] truncate"
                                          title={
                                            bridge.selectedElement.fingerprint.repeatPattern
                                              .itemSelector
                                          }
                                        >
                                          {
                                            bridge.selectedElement.fingerprint.repeatPattern
                                              .itemSelector
                                          }
                                        </div>
                                      </>
                                    )}

                                    {bridge.selectedElement.fingerprint.repeatPattern
                                      .containerTag && (
                                      <>
                                        <div className="text-muted-foreground">Container:</div>
                                        <div className="font-mono">
                                          &lt;
                                          {
                                            bridge.selectedElement.fingerprint.repeatPattern
                                              .containerTag
                                          }
                                          &gt;
                                        </div>
                                      </>
                                    )}

                                    {bridge.selectedElement.fingerprint.repeatPattern
                                      .containerRole && (
                                      <>
                                        <div className="text-muted-foreground">Container Role:</div>
                                        <div className="font-mono">
                                          {
                                            bridge.selectedElement.fingerprint.repeatPattern
                                              .containerRole
                                          }
                                        </div>
                                      </>
                                    )}
                                  </div>
                                </div>
                              )}
                          </div>
                        )}
                      </div>
                    )}

                    {/* Element Preview Thumbnail */}
                    {bridge.selectedElement &&
                      (detailPanePreview || thumbnails.get(bridge.selectedElement.id)) && (
                        <div className="mt-3 pt-2 border-t border-border/50">
                          <div className="text-xs font-semibold mb-1.5">Preview</div>
                          <img
                            src={`data:image/png;base64,${detailPanePreview || thumbnails.get(bridge.selectedElement.id)}`}
                            alt="Element preview"
                            className="max-h-64 max-w-64 object-contain rounded border border-border cursor-pointer hover:border-primary transition-colors"
                            onClick={handleOpenPreview}
                            title="Click to view full size"
                          />
                        </div>
                      )}

                    {/* Quick actions */}
                    <div className="flex gap-2 mt-3 pt-2 border-t border-border/50">
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => bridge.highlightElement(bridge.selectedElement!.id)}
                      >
                        <Eye className="w-3.5 h-3.5 mr-1" />
                        Highlight
                      </Button>
                      <Button variant="outline" size="sm" onClick={() => setActiveTab("actions")}>
                        <Play className="w-3.5 h-3.5 mr-1" />
                        Execute Action
                      </Button>
                    </div>
                  </div>
                )}

                {/* Full image preview modal */}
                {bridge.selectedElement && showElementPreview && (
                  <ImageViewerModal
                    imageData={largePreviewData || thumbnails.get(bridge.selectedElement.id) || ""}
                    title={`Preview: ${bridge.selectedElement.id}${isLoadingPreview ? " (loading...)" : ""}`}
                    isOpen={showElementPreview}
                    onClose={handleClosePreview}
                  />
                )}
              </>
            ) : (
              <div className="flex flex-col items-center justify-center h-full text-muted-foreground text-sm gap-2">
                <Layers className="w-8 h-8 opacity-50" />
                <p>Connect to a browser tab to view elements</p>
              </div>
            )}
          </div>
        )}

        {activeTab === "states" && (
          <StateDiscoveryPanel
            cooccurrenceData={bridge.cooccurrenceData}
            isLoading={bridge.isLoadingCooccurrence}
            sessionActive={bridge.captureSession.active}
            captureCount={bridge.captureSession.captureCount || 0}
            onRefresh={bridge.generateCooccurrenceExport}
            onSelectFingerprint={(hash) => {
              // Find element with this fingerprint hash and select it
              const element = bridge.elements.find((el) => el.fingerprint?.hash === hash);
              if (element) {
                handleSelectElement(element.id);
                setActiveTab("elements");
              }
            }}
            disabled={bridge.connectionStatus !== "connected"}
            onRunDiscovery={handleRunDiscovery}
            isRunningDiscovery={isRunningDiscovery}
            discoveryResult={discoveryResult}
          />
        )}

        {activeTab === "search" && (
          <SearchComparisonPanel
            elements={bridge.elements}
            onSelectElement={handleSelectElement}
            onHighlightElement={bridge.highlightElement}
            disabled={bridge.connectionStatus !== "connected"}
          />
        )}

        {activeTab === "describe" && (
          <ElementDescriptionPanel
            elements={bridge.elements}
            selectedElement={bridge.selectedElement}
            pageContext={bridge.pageContext}
            onSelectElement={handleSelectElement}
            disabled={bridge.connectionStatus !== "connected"}
          />
        )}

        {activeTab === "nl" && (
          <NaturalLanguagePanel
            elements={bridge.elements}
            onSelectElement={handleSelectElement}
            onHighlightElement={bridge.highlightElement}
            onExecuteAction={handleExecuteAction}
            pageContext={bridge.pageContext}
            disabled={bridge.connectionStatus !== "connected"}
          />
        )}

        {activeTab === "events" && <EventTimelineView events={events} loading={false} />}

        {activeTab === "actions" && (
          <ActionExecutorView
            element={selectedUIBridgeElement}
            onExecuteAction={handleExecuteAction}
            disabled={bridge.connectionStatus !== "connected" || !selectedUIBridgeElement}
          />
        )}

        {activeTab === "api" && (
          <RawApiPanel
            onSendCommand={bridge.sendCommand}
            lastResult={bridge.lastCommandResult}
            commandHistory={bridge.commandHistory}
            onClearHistory={bridge.clearCommandHistory}
            disabled={false} // Raw API can work even without connection to a tab
          />
        )}
      </div>

      {/* Exploration Config Dialog */}
      <ExplorationConfigDialog
        isOpen={showExplorationDialog}
        onClose={() => setShowExplorationDialog(false)}
        onRunExploration={handleRunExploration}
        onStopExploration={handleStopExploration}
        isExploring={isExploring}
        isStopping={isStopping}
        lastResult={explorationResult}
        wasStopped={wasStopped}
      />
    </div>
  );
}

export default ExternalUIBridgeInspector;
