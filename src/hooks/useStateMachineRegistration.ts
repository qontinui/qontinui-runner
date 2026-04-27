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
// Import from main entry to share the same globalRegistry singleton as UIBridgeProvider.
// Importing from "@qontinui/ui-bridge/core" resolves to a separate dist bundle with
// its own isolated globalRegistry variable, causing elements to be invisible to the engine.
import { getGlobalRegistry } from "@qontinui/ui-bridge";
import { getUIBridgeGlobal } from "./ui-bridge-events/utils";
import { getAllSpecs } from "@/lib/spec-registry";
import { compileStateMachineFromSpecs } from "@/lib/compile-state-machine";
import { persistCompiledStateMachine } from "@/lib/persist-compiled-state-machine";
import { persistQuarantinedCompilation } from "@/lib/persist-quarantined-compilation";
import type { SpecConfig } from "@/lib/spec-prompt-builder";

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

function buildEngineAdapter(
  engine: AutomationEngine,
  domExecutor?: DefaultDOMExecutor,
): StateMachineAPI {
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
    // Prefix match on name
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
      let resolved = resolveStateId(targetStateId);

      // page-* alias: execute the sidebar navigation transition's DOM actions
      // directly (click the sidebar nav item) to navigate to the page.
      if (!resolved && targetStateId.startsWith("page-")) {
        const slug = targetStateId.slice(5); // e.g. "error-monitor"
        const transitionId = `sidebar-to-${slug}`;
        const transitions = allTransitions();
        const sidebarT = transitions.find((t) => t.id === transitionId);
        if (sidebarT && domExecutor) {
          if (sidebarT.actions.length === 0) {
            throw new Error(`Sidebar transition "${transitionId}" has no actions`);
          }
          // Execute each action in the transition (typically a single sidebar click)
          for (const action of sidebarT.actions) {
            const found = domExecutor.findElement(action.target);
            if (!found) {
              throw new Error(`Element not found for sidebar transition "${transitionId}"`);
            }
            await domExecutor.executeAction(found.id, action.action, action.params);
            if (action.waitAfter?.timeout) {
              const ms = Math.min(action.waitAfter.timeout, 5000);
              await new Promise((r) => setTimeout(r, ms));
            }
          }
          // Update state machine state
          const next = new Set(engine.getActiveStates());
          for (const s of sidebarT.exitStates) next.delete(s);
          for (const s of sidebarT.activateStates) next.add(s);
          engine.stateMachine.setActiveStates(next);
          return {
            navigatedTo: targetStateId,
            path: [transitionId],
            totalCost: 1,
            activeStates: Array.from(engine.getActiveStates()),
            strategy: "sidebar-shortcut",
          };
        }

        // Fallback: sidebar transition not compiled — click the nav item directly
        // by searching for a [data-nav-item="<slug>"] element in the DOM.
        if (domExecutor) {
          const navElement = domExecutor.findElement({ attributes: { "data-nav-item": slug } });
          if (navElement) {
            await domExecutor.executeAction(navElement.id, "click");
            await new Promise((r) => setTimeout(r, 2000));
            return {
              navigatedTo: targetStateId,
              path: [],
              totalCost: 1,
              activeStates: Array.from(engine.getActiveStates()),
              strategy: "sidebar-direct-click",
            };
          }
        }
      }

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
    bridge.stateMachine = buildEngineAdapter(engine, executor);
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

  // Load on mount from persisted selection, or auto-compile from bundled specs
  useEffect(() => {
    let configId: string | null = null;
    try {
      configId = localStorage.getItem(SM_SELECTED_CONFIG_KEY);
    } catch {
      /* */
    }
    if (configId) {
      loadAndRegister(configId);
    } else {
      // No persisted config — auto-compile from bundled specs so the engine
      // is always available (enables prompt-home navigation, loadStateMachine, etc.)
      try {
        const inputs = getAllSpecs().map((s) => ({
          specId: s.specId,
          config: s.config as SpecConfig,
        }));
        const result = compileStateMachineFromSpecs(inputs);
        if (!result.compiled) {
          // Quarantined — do NOT register a partial engine. Persist the
          // record so the conflict can be inspected offline.
          console.warn(
            `[StateMachine] Auto-compile quarantined (${result.quarantine.conflicts.length} conflicts); engine not registered`,
          );
          void persistQuarantinedCompilation(result.quarantine);
        } else {
          const { stateMachine, stats } = result;
          const states = (stateMachine.states ?? []) as StateDefinition[];
          const transitions = (stateMachine.transitions ?? []) as TransitionDefinition[];
          if (states.length > 0) {
            engineRef.current = createAndRegisterEngine(states, transitions, engineRef.current);
            // eslint-disable-next-line no-console
            console.info(
              `[StateMachine] Auto-compiled from bundled specs: ${stats.statesCompiled} states, ${stats.transitionsCompiled} transitions`,
            );
            // Best-effort: also persist to the backend DB so the compiled machine
            // is available via the HTTP API (and survives runner restarts).
            void persistCompiledStateMachine(stateMachine);
          }
        }
      } catch (e) {
        console.warn("[StateMachine] Auto-compile from specs failed:", e);
      }
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
