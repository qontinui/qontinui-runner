/**
 * useStateMachineRegistration
 *
 * App-level hook that loads the selected state machine config from the database,
 * creates an AutomationEngine (from ui-bridge-auto), and registers it on
 * `window.__UI_BRIDGE__.stateMachine`.
 *
 * The engine provides physical UI interaction (clicking sidebar items, etc.),
 * event-driven state detection, pathfinding, and self-healing — all backed by
 * the UI Bridge SDK's live element registry.
 */

import { useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  StateMachineConfigFull,
  StateMachineState,
  StateMachineTransition,
} from "@qontinui/shared-types";
import {
  AutomationEngine,
  DefaultDOMExecutor,
  navigate as navigatePath,
  type RegistryLike,
  type StateDefinition,
  type TransitionDefinition,
  type TransitionAction as EngineTransitionAction,
  type ElementQuery,
} from "@qontinui/ui-bridge-auto";
import { getGlobalRegistry } from "@qontinui/ui-bridge/core";
import { getUIBridgeGlobal } from "./ui-bridge-events/utils";

const SM_SELECTED_CONFIG_KEY = "qontinui-runner-sm-selected-config";

// ---------------------------------------------------------------------------
// DB → Engine format converters
// ---------------------------------------------------------------------------

/** Convert a DB StateMachineState to an engine StateDefinition. */
function toStateDefinition(s: StateMachineState): StateDefinition {
  const meta = s.extra_metadata as Record<string, unknown> | undefined;
  // Static builder stores requiredElements in extra_metadata
  const requiredElements = (meta?.requiredElements as ElementQuery[]) ?? [];
  return {
    id: s.state_id,
    name: s.name,
    requiredElements,
    group: (meta?.group as string) || undefined,
    pathCost: s.confidence < 0.5 ? 2 : 1,
  };
}

/** Convert a DB StateMachineTransition to an engine TransitionDefinition. */
function toTransitionDefinition(t: StateMachineTransition): TransitionDefinition {
  // The static builder stores actions in ui-bridge-auto format already.
  // DB actions are typed as TransitionAction[] from shared-types but stored as raw JSON.
  const actions: EngineTransitionAction[] = (t.actions ?? []).map((a) => {
    const raw = a as unknown as Record<string, unknown>;
    return {
      target: (raw.target ?? {}) as ElementQuery,
      action: (raw.action ?? raw.type ?? "click") as string,
      params: raw.params as Record<string, unknown> | undefined,
      waitAfter: raw.waitAfter as EngineTransitionAction["waitAfter"],
    };
  });

  return {
    id: t.transition_id,
    name: t.name,
    fromStates: t.from_states,
    activateStates: t.activate_states,
    exitStates: t.exit_states,
    actions,
    pathCost: t.path_cost || 1,
  };
}

// ---------------------------------------------------------------------------
// Registry adapter — bridges UI Bridge SDK → ui-bridge-auto RegistryLike
// ---------------------------------------------------------------------------

function createRegistryAdapter(): RegistryLike {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const registry = getGlobalRegistry() as any;
  return {
    getAllElements() {
      return registry
        .getAllElements()
        .map(
          (el: {
            id: string;
            element: HTMLElement;
            type: string;
            label?: string;
            getState: () => Record<string, unknown>;
            getIdentifier: () => Record<string, string | undefined>;
          }) => ({
            id: el.id,
            element: el.element,
            type: el.type,
            label: el.label,
            getState: () => {
              const s = el.getState();
              return {
                visible: s.visible,
                enabled: s.enabled,
                focused: s.focused,
                checked: s.checked,
                textContent: s.textContent,
                value: s.value,
                rect: s.rect,
              };
            },
            getIdentifier: el.getIdentifier
              ? () => {
                  const ident = el.getIdentifier();
                  return { selector: ident.selector, xpath: ident.xpath, htmlId: ident.htmlId };
                }
              : undefined,
          }),
        );
    },
    on(type, listener) {
      return registry.on(type, listener as (...args: unknown[]) => void);
    },
  };
}

// ---------------------------------------------------------------------------
// State machine API adapter — wraps AutomationEngine for IPC handlers
// ---------------------------------------------------------------------------

interface StateMachineAPI {
  getStates: () => unknown;
  getActiveStates: () => string[];
  getTransitions: () => unknown;
  getState: (id: string) => unknown;
  getGroups: () => never[];
  activate: (id: string) => boolean;
  deactivate: (id: string) => boolean;
  setState: (id: string, active: boolean) => boolean;
  activateGroup: (id: string) => boolean;
  deactivateGroup: (id: string) => boolean;
  canExecuteTransition: (id: string) => boolean;
  executeTransition: (id: string) => Promise<unknown>;
  findPath: (from: string, to: string) => unknown;
  navigateTo: (id: string) => Promise<unknown>;
}

function buildEngineAdapter(engine: AutomationEngine): StateMachineAPI {
  const allDefs = () => engine.stateMachine.getAllStateDefinitions();
  const allTransitions = () => engine.stateMachine.getTransitionDefinitions();

  /** Resolve a state by ID or name prefix. */
  const resolveStateId = (idOrName: string): string | null => {
    const defs = allDefs();
    // Exact ID match
    if (defs.some((d) => d.id === idOrName)) return idOrName;
    // Exact name match
    const byName = defs.find((d) => d.name === idOrName);
    if (byName) return byName.id;
    // Prefix match
    const lower = idOrName.toLowerCase();
    const byPrefix = defs.find((d) => d.name.toLowerCase().startsWith(lower));
    return byPrefix?.id ?? null;
  };

  return {
    getStates: () => allDefs(),
    getActiveStates: () => Array.from(engine.getActiveStates()),
    getTransitions: () => allTransitions(),
    getState: (id) => {
      const resolved = resolveStateId(id);
      return resolved ? (allDefs().find((d) => d.id === resolved) ?? null) : null;
    },
    getGroups: () => [],

    activate: (id) => {
      const resolved = resolveStateId(id);
      if (!resolved) return false;
      const next = new Set(engine.getActiveStates());
      next.add(resolved);
      engine.stateMachine.setActiveStates(next);
      return true;
    },
    deactivate: (id) => {
      const resolved = resolveStateId(id) ?? id;
      const next = new Set(engine.getActiveStates());
      next.delete(resolved);
      engine.stateMachine.setActiveStates(next);
      return true;
    },
    setState: (id, active) => {
      const resolved = resolveStateId(id) ?? id;
      const next = new Set(engine.getActiveStates());
      if (active) next.add(resolved);
      else next.delete(resolved);
      engine.stateMachine.setActiveStates(next);
      return true;
    },
    activateGroup: () => false,
    deactivateGroup: () => false,

    canExecuteTransition: (id) => {
      const t = allTransitions().find((d) => d.id === id);
      if (!t) return false;
      const active = engine.getActiveStates();
      return t.fromStates.length === 0 || t.fromStates.some((s) => active.has(s));
    },

    executeTransition: async (id) => {
      const t = allTransitions().find((d) => d.id === id);
      if (!t) throw new Error(`Transition not found: ${id}`);
      const next = new Set(engine.getActiveStates());
      for (const s of t.exitStates) next.delete(s);
      for (const s of t.activateStates) next.add(s);
      engine.stateMachine.setActiveStates(next);
      return { executed: id, activeStates: Array.from(engine.getActiveStates()) };
    },

    findPath: (_from, to) => {
      const resolvedTo = resolveStateId(to) ?? to;
      const active = engine.getActiveStates();
      const transitions = engine.stateMachine.getTransitionDefinitions();
      try {
        const result = navigatePath(active, resolvedTo, transitions);
        return {
          path: result.path.map((t) => ({
            id: t.id,
            name: t.name,
            fromStates: t.fromStates,
            activateStates: t.activateStates,
          })),
          totalCost: result.totalCost,
        };
      } catch {
        return null;
      }
    },

    navigateTo: async (targetStateId) => {
      const resolved = resolveStateId(targetStateId);
      if (!resolved) throw new Error(`State not found: ${targetStateId}`);
      const result = await engine.navigateToState(resolved);
      return {
        navigatedTo: resolved,
        path: result.path.map((t) => t.id),
        totalCost: result.totalCost,
        activeStates: Array.from(result.targetsReached),
        strategy: result.strategy,
      };
    },
  };
}

// ---------------------------------------------------------------------------
// Engine lifecycle
// ---------------------------------------------------------------------------

/**
 * Create and configure an AutomationEngine, register it on __UI_BRIDGE__,
 * and return it. Disposes any previous engine.
 */
function createAndRegisterEngine(
  stateDefs: StateDefinition[],
  transitionDefs: TransitionDefinition[],
  previousEngine: AutomationEngine | null,
): AutomationEngine {
  if (previousEngine) previousEngine.stateDetector.dispose();

  const registryAdapter = createRegistryAdapter();
  const executor = new DefaultDOMExecutor(registryAdapter);
  const engine = new AutomationEngine({
    registry: registryAdapter,
    executor,
    enableHighlights: false,
    enableReliabilityTracking: true,
  });

  engine.defineStates(stateDefs);
  engine.defineTransitions(transitionDefs);
  engine.detectActiveStates();
  engine.stateDetector.dispose();

  const bridge = getUIBridgeGlobal();
  if (bridge) {
    bridge.stateMachine = buildEngineAdapter(engine);
    // Expose a simple JSON loading function on the bridge for easy testing.
    // Usage: window.__UI_BRIDGE__.loadStateMachine(jsonString)
    bridge.loadStateMachine = (json: string) => {
      try {
        const data = JSON.parse(json);
        const states = (data.states ?? []) as StateDefinition[];
        const transitions = (data.transitions ?? []) as TransitionDefinition[];
        const newEngine = createAndRegisterEngine(states, transitions, engine);
        return {
          success: true,
          states: states.length,
          transitions: transitions.length,
          active: Array.from(newEngine.getActiveStates()),
        };
      } catch (e) {
        return { success: false, error: String(e) };
      }
    };
  }

  return engine;
}

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

export function useStateMachineRegistration(): void {
  const engineRef = useRef<AutomationEngine | null>(null);

  const loadAndRegister = useCallback(async (configId: string | null) => {
    const bridge = getUIBridgeGlobal();

    if (!configId) {
      if (engineRef.current) engineRef.current.stateDetector.dispose();
      engineRef.current = null;
      if (bridge) delete bridge.stateMachine;
      return;
    }

    try {
      const config = await invoke<StateMachineConfigFull | null>("sm_get_config", { id: configId });
      if (!config) {
        if (bridge) delete bridge.stateMachine;
        return;
      }

      const stateDefs = config.states.map(toStateDefinition);
      const transitionDefs = config.transitions.map(toTransitionDefinition);
      engineRef.current = createAndRegisterEngine(stateDefs, transitionDefs, engineRef.current);
    } catch {
      if (bridge) delete bridge.stateMachine;
      engineRef.current = null;
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
      if (engineRef.current) {
        engineRef.current.stateDetector.dispose();
        engineRef.current = null;
      }
      const bridge = getUIBridgeGlobal();
      if (bridge) delete bridge.stateMachine;
    };
  }, [loadAndRegister]);
}
