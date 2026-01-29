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

import { useState, useCallback, useEffect } from "react";
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
} from "lucide-react";
import { ConnectionPanel } from "./ConnectionPanel";
import { RawApiPanel } from "./RawApiPanel";
import { ElementTreeView } from "./ElementTreeView";
import { EventTimelineView } from "./EventTimelineView";
import { ActionExecutorView } from "./ActionExecutorView";
import { SearchComparisonPanel } from "./SearchComparisonPanel";
import { ElementDescriptionPanel } from "./ElementDescriptionPanel";
import { NaturalLanguagePanel } from "./NaturalLanguagePanel";
import { useExternalUIBridge } from "../../hooks/useExternalUIBridge";
import type {
  UIBridgeElement,
  UIBridgeEvent,
  UIBridgeSnapshot,
} from "./UIBridgeInspectorPanel";

type TabId = "elements" | "search" | "describe" | "nl" | "events" | "actions" | "api";

/**
 * Convert external element to UIBridgeElement format for existing components
 */
function convertToUIBridgeElement(element: ReturnType<typeof useExternalUIBridge>["elements"][number]): UIBridgeElement {
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
  };
}

export function ExternalUIBridgeInspector() {
  const bridge = useExternalUIBridge();

  const [activeTab, setActiveTab] = useState<TabId>("elements");
  const [events, setEvents] = useState<UIBridgeEvent[]>([]);
  const [eventId, setEventId] = useState(0);
  const [pickerActive, setPickerActive] = useState(false);
  const [overlaysEnabled, setOverlaysEnabled] = useState(false);
  const [copiedId, setCopiedId] = useState<string | null>(null);

  // Convert elements for existing components
  const uiBridgeElements = bridge.elements.map(convertToUIBridgeElement);

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
      details: Partial<Omit<UIBridgeEvent, "id" | "timestamp" | "eventType">>
    ) => {
      const newEvent: UIBridgeEvent = {
        id: eventId + 1,
        timestamp: Date.now(),
        eventType,
        success: true,
        ...details,
      };
      setEventId((prev) => prev + 1);
      setEvents((prev) => [newEvent, ...prev].slice(0, 100));
    },
    [eventId]
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
    [bridge, addEvent]
  );

  // Handle action execution
  const handleExecuteAction = useCallback(
    async (
      elementId: string,
      action: string,
      params?: Record<string, unknown>
    ) => {
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
    [bridge, addEvent]
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
    [bridge, addEvent]
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

  // Log connection status changes
  useEffect(() => {
    if (bridge.connectionStatus === "connected") {
      addEvent("navigation_completed", {
        result: {
          elementCount: bridge.elements.length,
          url: bridge.connectedTabInfo?.url,
        },
      });
    }
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
            <RefreshCw
              className={`w-4 h-4 ${bridge.isLoadingElements ? "animate-spin" : ""}`}
            />
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
            {overlaysEnabled ? (
              <EyeOff className="w-4 h-4" />
            ) : (
              <Eye className="w-4 h-4" />
            )}
          </Button>
          <div className="flex-1" />
          <div className="text-xs text-muted-foreground">
            {bridge.elements.length} elements
          </div>
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
                <ElementTreeView
                  elements={uiBridgeElements}
                  states={[]}
                  activeStates={[]}
                  selectedElementId={bridge.selectedElementId}
                  onSelectElement={handleSelectElement}
                  loading={bridge.isLoadingElements}
                />

                {/* Enhanced element details with copy */}
                {bridge.selectedElement && (
                  <div className="mt-2 p-3 bg-muted/30 rounded-md border border-border/50">
                    <div className="flex items-center justify-between mb-2">
                      <div className="flex items-center gap-2">
                        <Badge variant="default">{bridge.selectedElement.type}</Badge>
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

                    <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
                      <div className="text-muted-foreground">Tag:</div>
                      <div className="font-mono">{bridge.selectedElement.tagName}</div>

                      {bridge.selectedElement.label && (
                        <>
                          <div className="text-muted-foreground">Label:</div>
                          <div className="truncate">{bridge.selectedElement.label}</div>
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

                    {/* Quick actions */}
                    <div className="flex gap-2 mt-3 pt-2 border-t border-border/50">
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() =>
                          bridge.highlightElement(bridge.selectedElement!.id)
                        }
                      >
                        <Eye className="w-3.5 h-3.5 mr-1" />
                        Highlight
                      </Button>
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => setActiveTab("actions")}
                      >
                        <Play className="w-3.5 h-3.5 mr-1" />
                        Execute Action
                      </Button>
                    </div>
                  </div>
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
            disabled={bridge.connectionStatus !== "connected"}
          />
        )}

        {activeTab === "events" && (
          <EventTimelineView events={events} loading={false} />
        )}

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
    </div>
  );
}

export default ExternalUIBridgeInspector;
