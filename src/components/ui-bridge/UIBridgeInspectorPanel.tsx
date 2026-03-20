/**
 * UI Bridge Inspector Panel
 *
 * Main container for UI Bridge inspection features including:
 * - Element tree viewer
 * - State management visualization
 * - Event timeline
 * - Action executor
 */

import { useState, useCallback } from "react";
import { Card, CardHeader, CardTitle, CardContent } from "../ui/Card";
import { Button } from "../ui/Button";
import { Badge } from "../ui/Badge";
import { ElementTreeView } from "./ElementTreeView";
import { EventTimelineView } from "./EventTimelineView";
import { ActionExecutorView } from "./ActionExecutorView";
import { Layers, Activity, Play, RefreshCw, Eye, EyeOff, Crosshair } from "lucide-react";
import type {
  UIBridgeElement,
  UIBridgeState,
  UIBridgeTransition,
  UIBridgeEvent,
  UIBridgeSnapshot,
} from "./inspector-types";

export type { UIBridgeElement, UIBridgeState, UIBridgeTransition, UIBridgeEvent, UIBridgeSnapshot };

type TabId = "elements" | "events" | "actions";

// Stable empty array for default prop to avoid creating new references on each render
const EMPTY_EVENTS: UIBridgeEvent[] = [];

interface UIBridgeInspectorPanelProps {
  /** Target page URL for connection */
  targetUrl?: string;
  /** Whether the panel is connected to a page */
  connected?: boolean;
  /** Callback when refresh is requested */
  onRefresh?: () => void;
  /** Current snapshot data */
  snapshot?: UIBridgeSnapshot | null;
  /** Recent events */
  events?: UIBridgeEvent[];
  /** Loading state */
  loading?: boolean;
  /** Error message */
  error?: string | null;
  /** Selected element ID */
  selectedElementId?: string | null;
  /** Callback when element is selected */
  onSelectElement?: (elementId: string | null) => void;
  /** Callback when action is executed */
  onExecuteAction?: (
    elementId: string,
    action: string,
    params?: Record<string, unknown>,
  ) => Promise<void>;
  /** Callback when picker mode is toggled */
  onTogglePicker?: (enabled: boolean) => void;
  /** Whether picker mode is active */
  pickerActive?: boolean;
  /** Callback when overlays are toggled */
  onToggleOverlays?: (enabled: boolean) => void;
  /** Whether overlays are shown */
  overlaysEnabled?: boolean;
}

export function UIBridgeInspectorPanel({
  targetUrl,
  connected = false,
  onRefresh,
  snapshot,
  events = EMPTY_EVENTS,
  loading = false,
  error,
  selectedElementId,
  onSelectElement,
  onExecuteAction,
  onTogglePicker,
  pickerActive = false,
  onToggleOverlays,
  overlaysEnabled = false,
}: UIBridgeInspectorPanelProps) {
  const [activeTab, setActiveTab] = useState<TabId>("elements");

  const handleRefresh = useCallback(() => {
    onRefresh?.();
  }, [onRefresh]);

  const handleTogglePicker = useCallback(() => {
    onTogglePicker?.(!pickerActive);
  }, [onTogglePicker, pickerActive]);

  const handleToggleOverlays = useCallback(() => {
    onToggleOverlays?.(!overlaysEnabled);
  }, [onToggleOverlays, overlaysEnabled]);

  const selectedElement = snapshot?.elements.find((el) => el.id === selectedElementId);

  const tabs: { id: TabId; label: string; icon: React.ReactNode }[] = [
    { id: "elements", label: "Elements", icon: <Layers className="w-4 h-4" /> },
    { id: "events", label: "Events", icon: <Activity className="w-4 h-4" /> },
    { id: "actions", label: "Actions", icon: <Play className="w-4 h-4" /> },
  ];

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <Card variant="borderless" className="mb-3">
        <CardHeader className="py-2">
          <CardTitle className="flex items-center gap-2">
            <Layers className="w-5 h-5 text-primary" />
            UI Bridge Inspector
          </CardTitle>
          <div className="flex items-center gap-2">
            <Badge variant={connected ? "success" : "muted"}>
              {connected ? "Connected" : "Disconnected"}
            </Badge>
          </div>
        </CardHeader>
        {targetUrl && (
          <CardContent className="pt-0 pb-2">
            <p className="text-xs text-muted-foreground truncate">{targetUrl}</p>
          </CardContent>
        )}
      </Card>

      {/* Toolbar */}
      <div className="flex items-center gap-2 mb-3">
        <Button
          variant="outline"
          size="sm"
          onClick={handleRefresh}
          disabled={loading || !connected}
          title="Refresh elements"
        >
          <RefreshCw className={`w-4 h-4 ${loading ? "animate-spin" : ""}`} />
        </Button>
        <Button
          variant={pickerActive ? "primary" : "outline"}
          size="sm"
          onClick={handleTogglePicker}
          disabled={!connected}
          title={pickerActive ? "Stop picker" : "Start element picker"}
        >
          <Crosshair className="w-4 h-4" />
        </Button>
        <Button
          variant={overlaysEnabled ? "primary" : "outline"}
          size="sm"
          onClick={handleToggleOverlays}
          disabled={!connected}
          title={overlaysEnabled ? "Hide overlays" : "Show element overlays"}
        >
          {overlaysEnabled ? <EyeOff className="w-4 h-4" /> : <Eye className="w-4 h-4" />}
        </Button>
        <div className="flex-1" />
        <div className="text-xs text-muted-foreground">
          {snapshot?.elements.length ?? 0} elements
          {snapshot?.activeStates.length ? ` | ${snapshot.activeStates.length} active states` : ""}
        </div>
      </div>

      {/* Error message */}
      {error && (
        <div className="mb-3 p-2 bg-destructive/10 border border-destructive/30 rounded-md text-sm text-destructive">
          {error}
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
          </button>
        ))}
      </div>

      {/* Tab Content */}
      <div className="flex-1 overflow-hidden">
        {activeTab === "elements" && (
          <ElementTreeView
            elements={snapshot?.elements ?? []}
            states={snapshot?.states ?? []}
            activeStates={snapshot?.activeStates ?? []}
            selectedElementId={selectedElementId}
            onSelectElement={onSelectElement}
            loading={loading}
          />
        )}
        {activeTab === "events" && <EventTimelineView events={events} loading={loading} />}
        {activeTab === "actions" && (
          <ActionExecutorView
            element={selectedElement}
            onExecuteAction={onExecuteAction}
            disabled={!connected || !selectedElement}
          />
        )}
      </div>
    </div>
  );
}

export default UIBridgeInspectorPanel;
