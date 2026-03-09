/**
 * StateMachineGraph — Thin wrapper around the shared StateMachineGraphView.
 *
 * Passes dagre and connects to the runner's state machine builder.
 */

import dagre from "dagre";
import type {
  StateMachineState,
  StateMachineTransition,
  PathfindingStep,
} from "@qontinui/shared-types";
import { StateMachineGraphView } from "@qontinui/workflow-ui/state-machine";
import "@xyflow/react/dist/style.css";

interface StateMachineGraphProps {
  states: StateMachineState[];
  transitions: StateMachineTransition[];
  selectedStateId: string | null;
  selectedTransitionId: string | null;
  onSelectState: (stateId: string | null) => void;
  onSelectTransition: (transitionId: string | null) => void;
  onDeleteTransition?: (id: string) => void;
  highlightedPath?: PathfindingStep[];
}

export function StateMachineGraph({
  states,
  transitions,
  selectedStateId,
  selectedTransitionId,
  onSelectState,
  onSelectTransition,
  onDeleteTransition,
  highlightedPath,
}: StateMachineGraphProps) {
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
      emptyMessage="No states in this config. Import a config or use Discovery to create states."
    />
  );
}
