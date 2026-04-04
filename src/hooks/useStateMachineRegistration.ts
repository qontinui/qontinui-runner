/**
 * useStateMachineRegistration
 *
 * App-level hook that loads the selected state machine config from the database
 * and registers a StateMachineAPI adapter on `window.__UI_BRIDGE__.stateMachine`.
 *
 * This enables all UI Bridge IPC state machine operations (navigate_to_state,
 * get_states, get_active_states, find_state_path, etc.) to work on the Runner itself.
 */

import { useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  StateMachineConfigFull,
  StateMachineState,
  StateMachineTransition,
} from "@qontinui/shared-types";
import { getUIBridgeGlobal } from "./ui-bridge-events/utils";
import { findPath } from "./stateMachinePathfinder";

const SM_SELECTED_CONFIG_KEY = "qontinui-runner-sm-selected-config";

interface StateMachineAdapter {
  // Query
  getStates: () => StateMachineState[];
  getActiveStates: () => string[];
  getTransitions: () => StateMachineTransition[];
  getState: (id: string) => StateMachineState | null;
  getGroups: () => never[];
  // Mutation
  activate: (id: string) => boolean;
  deactivate: (id: string) => boolean;
  setState: (id: string, active: boolean) => boolean;
  activateGroup: (id: string) => boolean;
  deactivateGroup: (id: string) => boolean;
  // Transitions
  canExecuteTransition: (id: string) => boolean;
  executeTransition: (id: string) => Promise<unknown>;
  // Navigation
  findPath: (from: string, to: string) => unknown;
  navigateTo: (id: string) => Promise<unknown>;
}

function buildAdapter(config: StateMachineConfigFull): StateMachineAdapter {
  const activeStates = new Set<string>();
  const { states, transitions } = config;

  // Build lookup maps
  const stateById = new Map<string, StateMachineState>();
  const stateByName = new Map<string, StateMachineState>();
  for (const s of states) {
    stateById.set(s.state_id, s);
    stateByName.set(s.name, s);
  }

  const transitionById = new Map<string, StateMachineTransition>();
  for (const t of transitions) transitionById.set(t.transition_id, t);

  /** Resolve a state by ID, exact name, or name prefix — returns its state_id. */
  const resolveStateId = (idOrName: string): string | null => {
    if (stateById.has(idOrName)) return idOrName;
    const byName = stateByName.get(idOrName);
    if (byName) return byName.state_id;
    // Fuzzy: match by prefix (e.g., "AI Chat" matches "AI Chat (10 elements)")
    const lower = idOrName.toLowerCase();
    for (const s of states) {
      if (s.name.toLowerCase().startsWith(lower)) return s.state_id;
    }
    return null;
  };

  const adapter: StateMachineAdapter = {
    // --- Query methods ---
    getStates: () => states,
    getActiveStates: () => [...activeStates],
    getTransitions: () => transitions,
    getState: (id) => {
      const resolved = resolveStateId(id);
      return resolved ? (stateById.get(resolved) ?? null) : null;
    },
    getGroups: () => [],

    // --- Mutation methods ---
    activate: (id) => {
      const resolved = resolveStateId(id);
      if (!resolved) return false;
      activeStates.add(resolved);
      return true;
    },
    deactivate: (id) => {
      const resolved = resolveStateId(id) ?? id;
      activeStates.delete(resolved);
      return true;
    },
    setState: (id, active) => {
      const resolved = resolveStateId(id) ?? id;
      if (active) activeStates.add(resolved);
      else activeStates.delete(resolved);
      return true;
    },
    activateGroup: () => false,
    deactivateGroup: () => false,

    // --- Transition methods ---
    canExecuteTransition: (id) => {
      const t = transitionById.get(id);
      if (!t) return false;
      return t.from_states.length === 0 || t.from_states.every((s) => activeStates.has(s));
    },

    executeTransition: async (id) => {
      const t = transitionById.get(id);
      if (!t) throw new Error(`Transition not found: ${id}`);
      if (!adapter.canExecuteTransition(id)) {
        throw new Error(`Preconditions not met for transition: ${id}`);
      }
      // Apply state changes
      for (const s of t.exit_states) activeStates.delete(s);
      for (const s of t.activate_states) activeStates.add(s);
      return { executed: id, activeStates: [...activeStates] };
    },

    // --- Pathfinding ---
    findPath: (from, to) => {
      const resolvedFrom = resolveStateId(from) ?? from;
      const resolvedTo = resolveStateId(to) ?? to;
      const fromSet = new Set<string>();
      fromSet.add(resolvedFrom);
      const result = findPath(fromSet, resolvedTo, transitions);
      if (!result) return null;
      return {
        path: result.transitions.map((t) => ({
          transition_id: t.transition_id,
          name: t.name,
          from_states: t.from_states,
          activate_states: t.activate_states,
        })),
        totalCost: result.totalCost,
      };
    },

    // --- Navigation ---
    navigateTo: async (targetStateId) => {
      const resolvedId = resolveStateId(targetStateId);
      if (!resolvedId) throw new Error(`State not found: ${targetStateId}`);
      targetStateId = resolvedId;

      if (activeStates.has(targetStateId)) {
        return { navigatedTo: targetStateId, path: [], alreadyActive: true };
      }

      const result = findPath(activeStates, targetStateId, transitions);
      if (!result) throw new Error(`No path to state: ${targetStateId}`);

      const executedPath: string[] = [];
      for (const t of result.transitions) {
        // Apply state changes for each transition in the path
        for (const s of t.exit_states) activeStates.delete(s);
        for (const s of t.activate_states) activeStates.add(s);
        executedPath.push(t.transition_id);
      }

      return {
        navigatedTo: targetStateId,
        path: executedPath,
        totalCost: result.totalCost,
        activeStates: [...activeStates],
      };
    },
  };

  return adapter;
}

export function useStateMachineRegistration(): void {
  const adapterRef = useRef<StateMachineAdapter | null>(null);

  const loadAndRegister = useCallback(async (configId: string | null) => {
    const bridge = getUIBridgeGlobal();

    if (!configId) {
      // Unregister
      adapterRef.current = null;
      if (bridge) delete bridge.stateMachine;
      return;
    }

    try {
      const config = await invoke<StateMachineConfigFull | null>("sm_get_config", { id: configId });
      if (!config) {
        adapterRef.current = null;
        if (bridge) delete bridge.stateMachine;
        return;
      }

      const adapter = buildAdapter(config);
      adapterRef.current = adapter;

      if (bridge) {
        bridge.stateMachine = adapter;
      }
    } catch {
      // Config load failed (e.g., deleted) — silently unregister
      adapterRef.current = null;
      if (bridge) delete bridge.stateMachine;
    }
  }, []);

  // Load on mount from persisted selection
  useEffect(() => {
    let configId: string | null = null;
    try {
      configId = localStorage.getItem(SM_SELECTED_CONFIG_KEY);
    } catch {
      /* */
    }
    if (configId) {
      loadAndRegister(configId);
    }
  }, [loadAndRegister]);

  // Listen for config changes from the UI Bridge States page
  useEffect(() => {
    const handler = (e: CustomEvent<{ configId: string | null }>) => {
      loadAndRegister(e.detail.configId);
    };
    window.addEventListener("sm-config-changed", handler);
    return () => {
      window.removeEventListener("sm-config-changed", handler);
      // Cleanup on unmount
      const bridge = getUIBridgeGlobal();
      if (bridge) delete bridge.stateMachine;
      adapterRef.current = null;
    };
  }, [loadAndRegister]);
}
