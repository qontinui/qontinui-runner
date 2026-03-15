/**
 * UIBridgeStateGraph — Thin wrapper around the shared StateMachineGraphView.
 *
 * Adds drag-and-drop element reassignment and maps transition selection
 * to database IDs.
 */

import dagre from "dagre";
import { StateMachineGraphView } from "@qontinui/workflow-ui/state-machine";
import "@xyflow/react/dist/style.css";
import type {
  StateMachineState,
  StateMachineTransition,
  PathfindingStep,
} from "@qontinui/shared-types";

interface UIBridgeStateGraphProps {
  states: StateMachineState[];
  transitions: StateMachineTransition[];
  selectedStateId: string | null;
  selectedTransitionId: string | null;
  onSelectState: (stateId: string | null) => void;
  onSelectTransition: (transitionId: string | null) => void;
  highlightedPath?: PathfindingStep[];
  onStartElementDrag?: (stateId: string, elementId: string) => void;
  onDragOver?: (event: React.DragEvent) => void;
  onDrop?: (event: React.DragEvent) => void;
  isDragging?: boolean;
  dropTargetStateId?: string | null;
  onDeleteTransition?: (id: string) => void;
  elementThumbnails?: Record<string, string>;
}

export function UIBridgeStateGraph({
  states,
  transitions,
  selectedStateId,
  selectedTransitionId,
  onSelectState,
  onSelectTransition,
  highlightedPath,
  onStartElementDrag,
  onDragOver,
  onDrop,
  isDragging,
  dropTargetStateId,
  onDeleteTransition,
  elementThumbnails,
}: UIBridgeStateGraphProps) {
  return (
    <StateMachineGraphView
      dagre={dagre}
      states={states}
      transitions={transitions}
      selectedStateId={selectedStateId}
      selectedTransitionId={selectedTransitionId}
      onSelectState={onSelectState}
      onSelectTransition={onSelectTransition}
      onDeleteTransition={onDeleteTransition}
      highlightedPath={highlightedPath}
      emptyMessage="No states discovered yet. Use the Discovery tab to discover states."
      onStartElementDrag={onStartElementDrag}
      onDragOver={onDragOver}
      onDrop={onDrop}
      isDragging={isDragging}
      dropTargetStateId={dropTargetStateId}
      resolveTransitionSelectionId={(trans) => trans.id}
      extraShortcutEntries={[
        ["Create transition", "Drag element"],
        ["Move element", "Alt+Drag"],
      ]}
      elementThumbnails={elementThumbnails}
    />
  );
}
