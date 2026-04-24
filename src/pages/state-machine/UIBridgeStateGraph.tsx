/**
 * UIBridgeStateGraph — Thin wrapper around the shared StateMachineGraphView.
 *
 * Adds drag-and-drop element reassignment and maps transition selection
 * to database IDs.
 */

import * as dagre from "@dagrejs/dagre";
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
  /** User-chosen chunk labels keyed by chunk id (chunked view). */
  chunkLabels?: Map<string, string>;
  /** Save a chunk label override. Empty string reverts to auto-derived name. */
  onSaveChunkLabel?: (chunkId: string, label: string) => void;
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
  chunkLabels,
  onSaveChunkLabel,
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
      chunkLabels={chunkLabels}
      onSaveChunkLabel={onSaveChunkLabel}
    />
  );
}
