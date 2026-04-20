/**
 * Connected UI Bridge Inspector
 *
 * Wrapper component that connects UIBridgeInspectorPanel to the real
 * UI Bridge data via the useUIBridge hook.
 */

import { useState, useCallback, useEffect, useRef } from "react";
import { useUIBridge } from "ui-bridge";
import type { RegisteredElement } from "ui-bridge";
import { UIBridgeInspectorPanel } from "./UIBridgeInspectorPanel";
import type { UIBridgeElement, UIBridgeEvent, UIBridgeSnapshot } from "./inspector-types";
import { ElementOverlay } from "./ElementOverlay";
import { ElementPicker } from "./ElementPicker";

/**
 * Convert a RegisteredElement from ui-bridge to our UIBridgeElement format
 */
function convertElement(element: RegisteredElement): UIBridgeElement {
  const state = element.getState();
  const identifier = element.getIdentifier();

  return {
    id: element.id,
    tagName: identifier.xpath.split("/").pop()?.split("[")[0] || "unknown",
    type: element.type,
    bounds: {
      x: state.rect.x,
      y: state.rect.y,
      width: state.rect.width,
      height: state.rect.height,
    },
    visible: state.visible,
    enabled: state.enabled,
    focused: state.focused,
    value: state.value,
    text: element.label,
    label: element.label,
    parent: null, // Could be extracted from DOM if needed
    children: [], // Could be extracted from DOM if needed
    actions: element.actions,
  };
}

/**
 * Connected wrapper for UIBridgeInspectorPanel
 */
export function ConnectedUIBridgeInspector() {
  const bridge = useUIBridge();
  const [selectedElementId, setSelectedElementId] = useState<string | null>(null);
  const [events, setEvents] = useState<UIBridgeEvent[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pickerActive, setPickerActive] = useState(false);
  const [overlaysEnabled, setOverlaysEnabled] = useState(false);
  const eventIdRef = useRef(0);

  // Convert bridge data to snapshot format
  const snapshot: UIBridgeSnapshot | null = bridge.available
    ? {
        elements: bridge.elements.map(convertElement),
        states: [], // UI Bridge doesn't have explicit states yet
        transitions: [], // UI Bridge doesn't have transitions yet
        activeStates: [],
        // eslint-disable-next-line react-hooks/purity
        timestamp: Date.now(),
      }
    : null;

  // Add an event to the timeline
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
      setEvents((prev) => [newEvent, ...prev].slice(0, 100)); // Keep last 100 events
    },
    [],
  );

  // Handle refresh
  const handleRefresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      // Trigger a discovery to refresh the element list
      const result = await bridge.discover();
      addEvent("element_discovered", {
        result: { count: result.elements.length },
        durationMs: 0,
      });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      addEvent("error", {
        success: false,
        errorMessage: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setLoading(false);
    }
  }, [bridge, addEvent]);

  // Handle element selection
  const handleSelectElement = useCallback(
    (elementId: string | null) => {
      setSelectedElementId(elementId);
      if (elementId) {
        addEvent("element_registered", { elementId });
      }
    },
    [addEvent],
  );

  // Handle action execution
  const handleExecuteAction = useCallback(
    async (elementId: string, action: string, params?: Record<string, unknown>) => {
      const startTime = Date.now();
      try {
        const result = await bridge.executeAction(elementId, {
          action,
          params,
        });

        addEvent("action_executed", {
          elementId,
          action,
          params,
          result: result as unknown as Record<string, unknown>,
          durationMs: Date.now() - startTime,
          success: result.success,
          errorMessage: result.error,
        });

        if (!result.success) {
          throw new Error(result.error || "Action failed");
        }
      } catch (err) {
        addEvent("error", {
          elementId,
          action,
          params,
          durationMs: Date.now() - startTime,
          success: false,
          errorMessage: err instanceof Error ? err.message : String(err),
        });
        throw err;
      }
    },
    [bridge, addEvent],
  );

  // Handle picker toggle
  const handleTogglePicker = useCallback((enabled: boolean) => {
    setPickerActive(enabled);
  }, []);

  // Handle overlays toggle
  const handleToggleOverlays = useCallback((enabled: boolean) => {
    setOverlaysEnabled(enabled);
  }, []);

  // Handle element pick from picker mode
  const handlePickElement = useCallback(
    (elementId: string) => {
      setPickerActive(false);
      setSelectedElementId(elementId);
      addEvent("element_picked", { elementId });
    },
    [addEvent],
  );

  // Track previous availability to only log once per connection
  const prevAvailable = useRef(bridge.available);

  // Log connection status changes - only when transitioning TO available
  useEffect(() => {
    if (bridge.available && !prevAvailable.current) {
      addEvent("navigation_completed", {
        result: { elementCount: bridge.elements.length },
      });
    }
    prevAvailable.current = bridge.available;
  }, [bridge.available, bridge.elements.length, addEvent]);

  return (
    <>
      <UIBridgeInspectorPanel
        targetUrl={window.location.href}
        connected={bridge.available}
        onRefresh={handleRefresh}
        snapshot={snapshot}
        events={events}
        loading={loading}
        error={error}
        selectedElementId={selectedElementId}
        onSelectElement={handleSelectElement}
        onExecuteAction={handleExecuteAction}
        onTogglePicker={handleTogglePicker}
        pickerActive={pickerActive}
        onToggleOverlays={handleToggleOverlays}
        overlaysEnabled={overlaysEnabled}
      />
      {overlaysEnabled && snapshot && (
        <ElementOverlay
          elements={snapshot.elements}
          selectedElementId={selectedElementId}
          onSelectElement={(id) => setSelectedElementId(id)}
        />
      )}
      {pickerActive && snapshot && (
        <ElementPicker
          elements={snapshot.elements}
          onPick={handlePickElement}
          onCancel={() => setPickerActive(false)}
        />
      )}
    </>
  );
}

export default ConnectedUIBridgeInspector;
