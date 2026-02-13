/**
 * SDK UI Bridge Inspector
 *
 * Full-featured inspector for SDK-integrated apps. Connects via direct HTTP
 * through the runner (useSdkUIBridge hook) and composes existing panels:
 *   - Elements (tree view)
 *   - Search (fuzzy/exact/attribute matching)
 *   - Descriptions (heuristic + AI element descriptions)
 *   - Natural Language (NL action execution)
 *   - Raw API (Postman-like command interface)
 *   - Screenshot (page capture viewer)
 */

import { useState, useMemo, useCallback } from "react";
import {
  Layers,
  Search,
  Bot,
  MessageSquare,
  Terminal,
  Image,
  Plug,
  PlugZap,
  Loader2,
  RefreshCw,
  AlertCircle,
  Wifi,
  WifiOff,
} from "lucide-react";
import { useSdkUIBridge } from "../../hooks/useSdkUIBridge";
import type { UIBridgeElement } from "./UIBridgeInspectorPanel";
import type { ExternalElement, PageContext } from "../../types/ui-bridge-types";
import { ElementTreeView } from "./ElementTreeView";
import { SearchComparisonPanel } from "./SearchComparisonPanel";
import { ElementDescriptionPanel } from "./ElementDescriptionPanel";
import { NaturalLanguagePanel } from "./NaturalLanguagePanel";
import { RawApiPanel } from "./RawApiPanel";
import { cn } from "../../lib/utils";

type InspectorTab = "elements" | "search" | "descriptions" | "nl" | "raw-api" | "screenshot";

/**
 * Convert an ExternalElement to UIBridgeElement for the ElementTreeView.
 * UIBridgeElement is a subset of ExternalElement, so this is mostly a pass-through.
 */
function convertToTreeElement(el: ExternalElement): UIBridgeElement {
  return {
    id: el.id,
    tagName: el.tagName,
    type: el.type,
    bounds: {
      x: el.bounds.x,
      y: el.bounds.y,
      width: el.bounds.width,
      height: el.bounds.height,
    },
    visible: el.visible,
    enabled: el.enabled,
    focused: el.focused,
    value: el.value,
    text: el.text,
    label: el.label,
    parent: el.parent,
    children: el.children,
    actions: el.actions,
    accessibility: el.accessibility,
    role: el.role,
    accessibleName: el.accessibleName,
    is_expanded: el.is_expanded,
    is_pressed: el.is_pressed,
    is_selected: el.is_selected,
    is_required: el.is_required,
    is_readonly: el.is_readonly,
    is_interactive: el.is_interactive,
    ref: el.ref,
    isCrossOrigin: el.isCrossOrigin,
  };
}

export function SdkUIBridgeInspector() {
  const sdk = useSdkUIBridge();

  const [activeTab, setActiveTab] = useState<InspectorTab>("elements");
  const [urlInput, setUrlInput] = useState("");

  // Convert elements for ElementTreeView
  const treeElements = useMemo(() => sdk.elements.map(convertToTreeElement), [sdk.elements]);

  // Build page context for panels that need it
  const pageContext = useMemo((): PageContext | null => {
    if (sdk.connectionStatus !== "connected" || !sdk.connectedApp) return null;
    return {
      url: sdk.connectedApp.port ? `http://localhost:${sdk.connectedApp.port}` : "",
      title: sdk.connectedApp.appName || "SDK App",
      elements: sdk.elements,
      timestamp: Date.now(),
    };
  }, [sdk.connectionStatus, sdk.connectedApp, sdk.elements]);

  // Handle connect
  const handleConnect = useCallback(async () => {
    if (!urlInput.trim()) return;
    await sdk.connect(urlInput.trim());
  }, [urlInput, sdk.connect]);

  // Handle disconnect
  const handleDisconnect = useCallback(async () => {
    await sdk.disconnect();
    setUrlInput("");
  }, [sdk.disconnect]);

  // Handle element selection forwarded to SDK
  const handleSelectElement = useCallback(
    (elementId: string | null) => {
      sdk.selectElement(elementId);
    },
    [sdk.selectElement],
  );

  const isConnected = sdk.connectionStatus === "connected";
  const isConnecting = sdk.connectionStatus === "connecting";

  const tabs: { id: InspectorTab; label: string; icon: React.ReactNode }[] = [
    { id: "elements", label: "Elements", icon: <Layers className="h-3.5 w-3.5" /> },
    { id: "search", label: "Search", icon: <Search className="h-3.5 w-3.5" /> },
    { id: "descriptions", label: "Describe", icon: <Bot className="h-3.5 w-3.5" /> },
    { id: "nl", label: "NL", icon: <MessageSquare className="h-3.5 w-3.5" /> },
    { id: "raw-api", label: "Raw API", icon: <Terminal className="h-3.5 w-3.5" /> },
    { id: "screenshot", label: "Screenshot", icon: <Image className="h-3.5 w-3.5" /> },
  ];

  return (
    <div className="flex h-full flex-col overflow-hidden">
      {/* Header: URL input + connection controls */}
      <div className="flex-shrink-0 border-b border-border bg-surface-raised px-4 py-3">
        <div className="flex items-center gap-3">
          <div className="flex-1 flex items-center gap-2">
            <input
              type="text"
              value={urlInput}
              onChange={(e) => setUrlInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !isConnected) handleConnect();
              }}
              placeholder="http://localhost:3001"
              disabled={isConnected}
              className="flex-1 rounded-md border border-border bg-surface-canvas px-3 py-1.5 text-sm text-text-primary placeholder:text-text-muted focus:border-brand-primary/50 focus:outline-none disabled:opacity-60"
            />

            {isConnected ? (
              <button
                onClick={handleDisconnect}
                className="flex items-center gap-1.5 rounded-md bg-error/10 px-3 py-1.5 text-xs font-medium text-error hover:bg-error/20 transition-colors"
              >
                <PlugZap className="h-3.5 w-3.5" />
                Disconnect
              </button>
            ) : (
              <button
                onClick={handleConnect}
                disabled={isConnecting || !urlInput.trim()}
                className="flex items-center gap-1.5 rounded-md bg-brand-primary px-3 py-1.5 text-xs font-medium text-white hover:bg-brand-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
              >
                {isConnecting ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <Plug className="h-3.5 w-3.5" />
                )}
                {isConnecting ? "Connecting..." : "Connect"}
              </button>
            )}
          </div>

          {/* Connection status badge */}
          <div className="flex items-center gap-2">
            {isConnected && sdk.connectedApp && (
              <span className="flex items-center gap-1.5 text-xs text-brand-success">
                <Wifi className="h-3 w-3" />
                {sdk.connectedApp.appName || "Connected"}
              </span>
            )}
            {sdk.connectionStatus === "error" && (
              <span className="flex items-center gap-1.5 text-xs text-error">
                <WifiOff className="h-3 w-3" />
                Error
              </span>
            )}
            {isConnected && (
              <button
                onClick={() => sdk.refreshElements()}
                disabled={sdk.isLoadingElements}
                className="flex h-7 w-7 items-center justify-center rounded text-text-muted hover:text-text-primary hover:bg-surface-hover transition-colors"
                title="Refresh elements"
              >
                <RefreshCw className={cn("h-3.5 w-3.5", sdk.isLoadingElements && "animate-spin")} />
              </button>
            )}
          </div>
        </div>

        {/* Error message */}
        {sdk.error && (
          <div className="mt-2 flex items-center gap-2 text-xs text-error">
            <AlertCircle className="h-3.5 w-3.5 flex-shrink-0" />
            {sdk.error}
          </div>
        )}

        {/* Element count */}
        {isConnected && (
          <div className="mt-1.5 text-[10px] text-text-muted">
            {sdk.isLoadingElements ? (
              <span className="flex items-center gap-1">
                <Loader2 className="h-3 w-3 animate-spin" />
                Loading elements...
              </span>
            ) : (
              `${sdk.elements.length} elements`
            )}
          </div>
        )}
      </div>

      {/* Tab bar */}
      <div className="flex-shrink-0 flex gap-1 border-b border-border px-3 py-2 bg-surface-raised/50">
        {tabs.map((tab) => (
          <button
            key={tab.id}
            onClick={() => setActiveTab(tab.id)}
            className={cn(
              "flex items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium transition-colors",
              activeTab === tab.id
                ? "bg-brand-primary/10 text-brand-primary"
                : "text-text-muted hover:bg-surface-hover hover:text-text-primary",
            )}
          >
            {tab.icon}
            {tab.label}
          </button>
        ))}
      </div>

      {/* Tab content */}
      <div className="flex-1 min-h-0 overflow-auto">
        {activeTab === "elements" && (
          <ElementTreeView
            elements={treeElements}
            states={[]}
            activeStates={[]}
            selectedElementId={sdk.selectedElementId}
            onSelectElement={handleSelectElement}
            loading={sdk.isLoadingElements}
            screenshotData={sdk.pageScreenshot?.data}
          />
        )}

        {activeTab === "search" && (
          <div className="h-full p-3">
            <SearchComparisonPanel
              elements={sdk.elements}
              onSelectElement={(id) => sdk.selectElement(id)}
              onHighlightElement={(id) => sdk.highlightElement(id)}
              disabled={!isConnected}
            />
          </div>
        )}

        {activeTab === "descriptions" && (
          <div className="h-full p-3">
            <ElementDescriptionPanel
              elements={sdk.elements}
              selectedElement={sdk.selectedElement}
              pageContext={pageContext}
              onSelectElement={(id) => sdk.selectElement(id)}
              disabled={!isConnected}
            />
          </div>
        )}

        {activeTab === "nl" && (
          <div className="h-full p-3">
            <NaturalLanguagePanel
              elements={sdk.elements}
              onSelectElement={(id) => sdk.selectElement(id)}
              onHighlightElement={(id) => sdk.highlightElement(id)}
              onExecuteAction={sdk.executeAction}
              pageContext={pageContext}
              disabled={!isConnected}
            />
          </div>
        )}

        {activeTab === "raw-api" && (
          <div className="h-full p-3">
            <RawApiPanel
              onSendCommand={sdk.sendCommand}
              lastResult={sdk.lastCommandResult}
              commandHistory={sdk.commandHistory}
              onClearHistory={sdk.clearCommandHistory}
              disabled={!isConnected}
            />
          </div>
        )}

        {activeTab === "screenshot" && (
          <div className="h-full p-4">
            <div className="space-y-4">
              <div className="flex items-center gap-3">
                <button
                  onClick={() => sdk.captureScreenshot()}
                  disabled={!isConnected || sdk.isCapturingScreenshot}
                  className="flex items-center gap-2 rounded-md bg-brand-primary px-4 py-2 text-sm font-medium text-white hover:bg-brand-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                >
                  {sdk.isCapturingScreenshot ? (
                    <Loader2 className="h-4 w-4 animate-spin" />
                  ) : (
                    <Image className="h-4 w-4" />
                  )}
                  {sdk.isCapturingScreenshot ? "Capturing..." : "Capture Screenshot"}
                </button>
                {sdk.pageScreenshot && (
                  <span className="text-xs text-text-muted">
                    {sdk.pageScreenshot.viewport.width} x {sdk.pageScreenshot.viewport.height}
                    {" | "}
                    {new Date(sdk.pageScreenshot.capturedAt).toLocaleTimeString()}
                  </span>
                )}
              </div>

              {sdk.pageScreenshot ? (
                <div className="rounded-lg border border-border overflow-hidden bg-surface-canvas">
                  <img
                    src={`data:image/png;base64,${sdk.pageScreenshot.data}`}
                    alt="Page screenshot"
                    className="w-full h-auto"
                  />
                </div>
              ) : (
                <div className="flex flex-col items-center justify-center py-16 text-text-muted">
                  <Image className="h-12 w-12 opacity-30 mb-3" />
                  <p className="text-sm">No screenshot captured yet</p>
                  <p className="text-xs mt-1">
                    {isConnected
                      ? 'Click "Capture Screenshot" to take a page snapshot'
                      : "Connect to an SDK app first"}
                  </p>
                </div>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

export default SdkUIBridgeInspector;
