/**
 * useElementDrag Hook
 *
 * Handles drag-and-drop element reassignment and transition creation on the
 * state machine graph. Port of the web's useElementDrag hook.
 */

import { useState, useCallback, useEffect } from "react";
import type {
  StateMachineState,
  StateMachineTransition,
  StateMachineTransitionCreate,
  TransitionAction,
} from "@qontinui/shared-types";
import type { ShowToastFn } from "./useToast";
import { createLogger } from "@/lib/logger";

const log = createLogger("useElementDrag");

interface ElementDragData {
  sourceStateId: string;
  elementId: string;
  isMoveOperation: boolean;
}

interface UseElementDragOptions {
  states: StateMachineState[];
  transitions: StateMachineTransition[];
  onCreateTransition: (data: StateMachineTransitionCreate) => Promise<unknown>;
  onUpdateState: (stateId: string, elementIds: string[]) => Promise<void>;
  showToast?: ShowToastFn;
}

/** Derive a readable action name and action type from an element ID */
function deriveAction(elementId: string): {
  name: string;
  action: TransitionAction;
} {
  // URL/navigation elements → navigate action
  if (elementId.startsWith("url:") || elementId.startsWith("nav:")) {
    const label = elementId.includes(":") ? elementId.split(":").slice(1).join(":") : elementId;
    return {
      name: `Navigate to ${label}`,
      action: { type: "navigate" as const, url: label },
    };
  }

  // Text inputs → type action
  if (elementId.startsWith("text:")) {
    const label = elementId.includes(":") ? elementId.split(":").slice(1).join(":") : elementId;
    return {
      name: `Type in ${label}`,
      action: { type: "type" as const, target: elementId, text: "" },
    };
  }

  // Role-based selectors that include "select", "option", "listbox", "combobox" → select action
  if (elementId.startsWith("role:")) {
    const roleLabel = elementId.slice(5).toLowerCase();
    if (/select|option|listbox|combobox|dropdown/i.test(roleLabel)) {
      const label = elementId.split(":").slice(1).join(":");
      return {
        name: `Select ${label}`,
        action: { type: "select" as const, target: elementId },
      };
    }
  }

  // Default → click action
  const label = elementId.includes(":") ? elementId.split(":").slice(1).join(":") : elementId;
  return {
    name: `Click ${label}`,
    action: { type: "click" as const, target: elementId },
  };
}

/** Check if a transition between two states already exists */
function findExistingTransition(
  transitions: StateMachineTransition[],
  sourceStateId: string,
  targetStateId: string,
): StateMachineTransition | undefined {
  return transitions.find(
    (t) => t.from_states.includes(sourceStateId) && t.activate_states.includes(targetStateId),
  );
}

export function useElementDrag({
  states,
  transitions,
  onCreateTransition,
  onUpdateState,
  showToast,
}: UseElementDragOptions) {
  const [dragData, setDragData] = useState<{
    sourceStateId: string;
    elementId: string;
  } | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const [dropTargetStateId, setDropTargetStateId] = useState<string | null>(null);

  // Clean up drag state when drag ends
  useEffect(() => {
    const handleDragEnd = () => {
      setIsDragging(false);
      setDropTargetStateId(null);
      setDragData(null);
    };
    window.addEventListener("dragend", handleDragEnd);
    return () => window.removeEventListener("dragend", handleDragEnd);
  }, []);

  const handleStartElementDrag = useCallback((stateId: string, elementId: string) => {
    setDragData({ sourceStateId: stateId, elementId });
    setIsDragging(true);
  }, []);

  const handleElementDropOnState = useCallback(
    async (targetStateId: string, dragInfo: ElementDragData) => {
      const { sourceStateId, elementId, isMoveOperation } = dragInfo;

      if (targetStateId === sourceStateId) {
        const msg = isMoveOperation
          ? "Cannot move element to the same state"
          : "Cannot create a transition to the same state";
        showToast?.(msg, "error");
        console.warn(msg);
        return;
      }

      const sourceState = states.find((s) => s.state_id === sourceStateId);
      const targetState = states.find((s) => s.state_id === targetStateId);

      if (!sourceState || !targetState) {
        showToast?.("Could not find states", "error");
        console.warn("Could not find states");
        return;
      }

      if (isMoveOperation) {
        const updatedSourceElements = sourceState.element_ids.filter((eid) => eid !== elementId);
        const updatedTargetElements = targetState.element_ids.includes(elementId)
          ? targetState.element_ids
          : [...targetState.element_ids, elementId];

        await onUpdateState(sourceState.id, updatedSourceElements);
        await onUpdateState(targetState.id, updatedTargetElements);

        const moveMsg = `Moved "${elementId}" from "${sourceState.name}" to "${targetState.name}"`;
        showToast?.(moveMsg, "success");
        log.debug(moveMsg);
      } else {
        const existing = findExistingTransition(transitions, sourceStateId, targetStateId);
        if (existing) {
          const existMsg = `A transition from "${sourceState.name}" to "${targetState.name}" already exists: "${existing.name}"`;
          showToast?.(existMsg, "error");
          console.warn(existMsg);
          setDragData(null);
          return;
        }

        const { name: transitionName, action } = deriveAction(elementId);
        const exitStates = sourceStateId !== targetStateId ? [sourceStateId] : [];

        const transitionData: StateMachineTransitionCreate = {
          name: transitionName,
          from_states: [sourceStateId],
          activate_states: [targetStateId],
          exit_states: exitStates,
          actions: [action],
          path_cost: 1.0,
          stays_visible: false,
        };

        await onCreateTransition(transitionData);
        const createMsg = `Created transition "${transitionName}" from "${sourceState.name}" to "${targetState.name}"`;
        showToast?.(createMsg, "success");
        log.debug(createMsg);
      }

      setDragData(null);
    },
    [states, transitions, onCreateTransition, onUpdateState, showToast],
  );

  const handleDragOver = useCallback(
    (event: React.DragEvent) => {
      const hasData = event.dataTransfer.types.includes("application/ui-bridge-element-drag");
      if (hasData) {
        event.preventDefault();
        event.dataTransfer.dropEffect = event.altKey ? "move" : "link";

        let target = event.target as HTMLElement;
        let hoveredStateId: string | null = null;
        while (target && !hoveredStateId) {
          const nodeId = target.getAttribute("data-id");
          if (nodeId && states.some((s) => s.state_id === nodeId)) {
            hoveredStateId = nodeId;
            break;
          }
          target = target.parentElement as HTMLElement;
        }
        setDropTargetStateId(hoveredStateId);
      }
    },
    [states],
  );

  const handleDrop = useCallback(
    (event: React.DragEvent) => {
      event.preventDefault();
      setIsDragging(false);
      setDropTargetStateId(null);

      const dragDataStr = event.dataTransfer.getData("application/ui-bridge-element-drag");
      if (!dragDataStr) return;

      try {
        const parsed: ElementDragData = JSON.parse(dragDataStr);

        let target = event.target as HTMLElement;
        let targetStateId: string | null = null;

        while (target && !targetStateId) {
          const nodeId = target.getAttribute("data-id");
          if (nodeId && states.some((s) => s.state_id === nodeId)) {
            targetStateId = nodeId;
            break;
          }
          target = target.parentElement as HTMLElement;
        }

        if (targetStateId) {
          handleElementDropOnState(targetStateId, parsed);
        } else {
          setDragData(null);
        }
      } catch {
        setDragData(null);
      }
    },
    [states, handleElementDropOnState],
  );

  return {
    dragData,
    isDragging,
    dropTargetStateId,
    handleStartElementDrag,
    handleElementDropOnState,
    handleDragOver,
    handleDrop,
  };
}
