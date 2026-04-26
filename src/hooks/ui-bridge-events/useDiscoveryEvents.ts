import { useCallback } from "react";
import type { BridgeSnapshot } from "@qontinui/ui-bridge";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";
import { getUIBridgeGlobal } from "./utils";
import { createLogger } from "@/lib/logger";
import { ACTIVE_TAB_STORAGE_KEY } from "@/components/app/tab-types";
import { instanceStorage } from "@/lib/instance-storage";

const logger = createLogger("UIBridgeDiscoveryEvents");

/** Unified type for all state machine methods used across event handlers. */
interface StateMachineAPI {
  // Query methods
  getStates?: () => unknown;
  states?: unknown;
  getActiveStates?: () => unknown;
  activeStates?: unknown;
  getGroups?: () => unknown;
  groups?: unknown;
  getTransitions?: () => unknown;
  transitions?: unknown;
  getState?: (id: string) => unknown;
  // Mutation methods
  activate?: (id: string) => unknown;
  deactivate?: (id: string) => unknown;
  setState?: (id: string, active: boolean) => unknown;
  activateGroup?: (id: string) => unknown;
  deactivateGroup?: (id: string) => unknown;
  canExecuteTransition?: (id: string) => boolean;
  executeTransition?: (id: string) => Promise<unknown> | unknown;
  findPath?: (from: string, to: string) => unknown;
  navigateTo?: (id: string) => Promise<unknown> | unknown;
}

/** Get the stateMachine from the UI Bridge global, cast to the known API. */
function getStateMachine(): StateMachineAPI | undefined {
  return getUIBridgeGlobal()?.stateMachine as StateMachineAPI | undefined;
}

/**
 * Handles: discover, find, get_snapshot, get_component_state,
 *          get_states, get_active_states, get_state_snapshot, get_state,
 *          activate_state, deactivate_state, get_state_groups,
 *          activate_state_group, deactivate_state_group, get_transitions,
 *          can_execute_transition, execute_transition, find_state_path,
 *          navigate_to_state
 */
export function useDiscoveryEvents(
  context: Pick<UIBridgeEventContext, "bridgeRef" | "sendResponse">,
) {
  const { bridgeRef, sendResponse } = context;

  return useCallback(
    async (payload: UIBridgeRequestPayload): Promise<boolean> => {
      const { requestId, type } = payload;
      const currentBridge = bridgeRef.current;

      switch (type) {
        case "discover": {
          // Extract options from nested payload.options or top-level payload fields
          const discoverSource = (payload.options ?? payload) as Record<string, unknown>;
          const discoverOptions: Record<string, unknown> = {};
          const discoverKeys = [
            "interactive_only",
            "interactiveOnly",
            "includeHidden",
            "include_hidden",
            "element_type",
            "types",
            "text",
            "role",
            "label",
            "selector",
            "limit",
          ];
          for (const key of discoverKeys) {
            if (discoverSource[key] !== undefined) {
              const mappedKey =
                key === "interactive_only"
                  ? "interactiveOnly"
                  : key === "include_hidden"
                    ? "includeHidden"
                    : key;
              discoverOptions[mappedKey] = discoverSource[key];
            }
          }
          const result = await currentBridge.discover(discoverOptions);

          // Enrich elements with stableRef if createStableRef is available
          try {
            const { createStableRef } = await import("@qontinui/ui-bridge/core");
            const registry = (
              currentBridge as { registry?: { getElement?: (id: string) => unknown } }
            ).registry;
            if (result?.elements && registry?.getElement && createStableRef) {
              for (const el of result.elements as unknown as Array<Record<string, unknown>>) {
                const reg = registry.getElement(el.id as string) as {
                  element?: Element;
                  id?: string;
                  type?: string;
                  label?: string;
                  actions?: unknown[];
                  getState?: () => unknown;
                } | null;
                if (reg) {
                  try {
                    el.stableRef = createStableRef(reg as never);
                  } catch {
                    /* skip */
                  }
                }
              }
            }
          } catch {
            /* createStableRef not available */
          }

          await sendResponse({
            requestId,
            type,
            success: true,
            data: result,
            timestamp: Date.now(),
          });
          return true;
        }

        case "find": {
          // The Rust backend merges the HTTP request body at the top level
          // of the payload (alongside requestId and type). Extract known
          // FindRequest fields from the payload itself, falling back to
          // nested params/body for backward compatibility.
          const nested = payload.params ?? payload.body;
          const source = (nested && typeof nested === "object" ? nested : payload) as Record<
            string,
            unknown
          >;
          const findOptions: Record<string, unknown> = { includeHidden: true };
          const findKeys = [
            "element_type",
            "types",
            "text",
            "exact_text",
            "role",
            "label",
            "root",
            "selector",
            "interactiveOnly",
            "interactive_only",
            "includeContent",
            "contentOnly",
            "includeHidden",
            "limit",
          ];
          for (const key of findKeys) {
            if (source[key] !== undefined) {
              // Map snake_case interactive_only to camelCase interactiveOnly
              const mappedKey = key === "interactive_only" ? "interactiveOnly" : key;
              findOptions[mappedKey] = source[key];
            }
          }
          const discovered = await currentBridge.discover(findOptions);
          await sendResponse({
            requestId,
            type,
            success: true,
            data: discovered,
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_snapshot": {
          logger.debug("get_snapshot: creating snapshot...");
          // The runner mounts UI Bridge HTTP routes under /ui-bridge/* so
          // override the snapshot's default componentActionBasePath prefix
          // to produce URLs callers can hit directly.
          //
          // Snapshot enrichment (the seven canonical tracker fields — page,
          // modalStack, toasts, relationships, dragDrop, undoRedo, shortcuts)
          // is now handled by the registry's `setEnrichers` slot, wired in
          // `UIBridgeProviderInit` (see SDK Tracker Reshape Phase 1). The
          // runner-specific `availableTabs` + `tabActivation` fields are
          // added via the pluggable enricher registered by `RunnerTabsEnricher`
          // in `App.tsx` (Phase 3).
          const snapshot: BridgeSnapshot = await currentBridge.createSnapshotAsync(50, {
            componentBasePath: "/ui-bridge/control/component",
            // F1 — supply the runner's active-tab provider so cross-tab
            // automation can read both `route` and `activeTab` from a single
            // snapshot. The runner's tab system stores its active tab in
            // instanceStorage under `ACTIVE_TAB_STORAGE_KEY` (the same key
            // the `tabs_list` IPC handler reads), so this matches whatever
            // `tab_activate` last set. Falls back to "prompt-home" — the
            // initial tab the runner shell selects on first launch — when
            // nothing is persisted yet.
            getActiveTab: () => instanceStorage.getItem(ACTIVE_TAB_STORAGE_KEY) ?? "prompt-home",
          });
          logger.debug(`get_snapshot: snapshot created (${snapshot.elements.length} elements)`);

          await sendResponse({
            requestId,
            type,
            success: true,
            data: snapshot,
            timestamp: Date.now(),
          });
          logger.debug(`get_snapshot: response sent (${snapshot.elements.length} elements)`);
          return true;
        }

        // ==================================================================
        // Component State
        // ==================================================================

        case "get_component_state": {
          const componentId = (payload.params?.componentId as string) ?? payload.componentId ?? "";
          if (!componentId) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "componentId is required",
              timestamp: Date.now(),
            });
            return true;
          }

          const component = currentBridge.getComponent(componentId);
          if (!component) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `Component not found: ${componentId}`,
              timestamp: Date.now(),
            });
            return true;
          }

          // Gather state from the component's child elements
          const childStates: Record<string, unknown> = {};
          if (component.elementIds) {
            for (const elId of component.elementIds) {
              const el = currentBridge.getElement(elId);
              if (el) {
                childStates[elId] = el.getState();
              }
            }
          }

          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              id: component.id,
              name: component.name,
              description: component.description,
              mounted: component.mounted,
              elementIds: component.elementIds,
              childStates,
            },
            timestamp: Date.now(),
          });
          return true;
        }

        // ==================================================================
        // State Machine
        // ==================================================================

        case "get_states": {
          const sm = getStateMachine();
          const states = sm?.getStates?.() ?? sm?.states ?? [];
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { states },
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_active_states": {
          const sm = getStateMachine();
          const activeStates = sm?.getActiveStates?.() ?? sm?.activeStates ?? [];
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { states: activeStates },
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_state_snapshot": {
          const sm = getStateMachine();
          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              states: sm?.getStates?.() ?? [],
              activeStates: sm?.getActiveStates?.() ?? [],
              groups: sm?.getGroups?.() ?? [],
              transitions: sm?.getTransitions?.() ?? [],
              timestamp: Date.now(),
            },
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_state": {
          const stateId = (payload.params?.stateId as string) ?? "";
          if (!stateId) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "stateId is required",
              timestamp: Date.now(),
            });
            return true;
          }
          const sm = getStateMachine();
          const state = sm?.getState?.(stateId) ?? null;
          if (state) {
            await sendResponse({
              requestId,
              type,
              success: true,
              data: state,
              timestamp: Date.now(),
            });
          } else {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `State not found: ${stateId}`,
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "activate_state": {
          const activateId = (payload.params?.stateId as string) ?? "";
          if (!activateId) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "stateId is required",
              timestamp: Date.now(),
            });
            return true;
          }
          const sm = getStateMachine();
          try {
            const result = sm?.activate?.(activateId) ?? sm?.setState?.(activateId, true);
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { activated: activateId, result: result ?? true },
              timestamp: Date.now(),
            });
          } catch (err) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: err instanceof Error ? err.message : String(err),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "deactivate_state": {
          const deactivateId = (payload.params?.stateId as string) ?? "";
          if (!deactivateId) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "stateId is required",
              timestamp: Date.now(),
            });
            return true;
          }
          const sm = getStateMachine();
          try {
            const result = sm?.deactivate?.(deactivateId) ?? sm?.setState?.(deactivateId, false);
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { deactivated: deactivateId, result: result ?? true },
              timestamp: Date.now(),
            });
          } catch (err) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: err instanceof Error ? err.message : String(err),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "get_state_groups": {
          const sm = getStateMachine();
          const groups = sm?.getGroups?.() ?? sm?.groups ?? [];
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { groups },
            timestamp: Date.now(),
          });
          return true;
        }

        case "activate_state_group": {
          const groupId = (payload.params?.groupId as string) ?? "";
          if (!groupId) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "groupId is required",
              timestamp: Date.now(),
            });
            return true;
          }
          const sm = getStateMachine();
          try {
            const result = sm?.activateGroup?.(groupId);
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { activated: groupId, result: result ?? true },
              timestamp: Date.now(),
            });
          } catch (err) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: err instanceof Error ? err.message : String(err),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "deactivate_state_group": {
          const deactivateGroupId = (payload.params?.groupId as string) ?? "";
          if (!deactivateGroupId) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "groupId is required",
              timestamp: Date.now(),
            });
            return true;
          }
          const sm = getStateMachine();
          try {
            const result = sm?.deactivateGroup?.(deactivateGroupId);
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { deactivated: deactivateGroupId, result: result ?? true },
              timestamp: Date.now(),
            });
          } catch (err) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: err instanceof Error ? err.message : String(err),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "get_transitions": {
          const sm = getStateMachine();
          const transitions = sm?.getTransitions?.() ?? sm?.transitions ?? [];
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { transitions },
            timestamp: Date.now(),
          });
          return true;
        }

        case "can_execute_transition": {
          const transitionId = (payload.params?.transitionId as string) ?? "";
          if (!transitionId) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "transitionId is required",
              timestamp: Date.now(),
            });
            return true;
          }
          const sm = getStateMachine();
          const canExecute = sm?.canExecuteTransition?.(transitionId) ?? false;
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { canExecute, transitionId },
            timestamp: Date.now(),
          });
          return true;
        }

        case "execute_transition": {
          const execTransitionId = (payload.params?.transitionId as string) ?? "";
          if (!execTransitionId) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "transitionId is required",
              timestamp: Date.now(),
            });
            return true;
          }
          const sm = getStateMachine();
          try {
            const result = await Promise.resolve(sm?.executeTransition?.(execTransitionId));
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { executed: execTransitionId, result: result ?? true },
              timestamp: Date.now(),
            });
          } catch (err) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: err instanceof Error ? err.message : String(err),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "find_state_path": {
          const pathParams = payload.params ?? payload.body ?? {};
          const fromState = (pathParams.from as string) ?? "";
          const toState = (pathParams.to as string) ?? "";
          if (!fromState || !toState) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "from and to state IDs are required",
              timestamp: Date.now(),
            });
            return true;
          }
          const sm = getStateMachine();
          const path = sm?.findPath?.(fromState, toState) ?? null;
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { from: fromState, to: toState, path },
            timestamp: Date.now(),
          });
          return true;
        }

        case "navigate_to_state": {
          const navParams = payload.params ?? payload.body ?? {};
          const targetState = (navParams.targetState as string) ?? (navParams.to as string) ?? "";
          if (!targetState) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "targetState is required",
              timestamp: Date.now(),
            });
            return true;
          }
          const sm = getStateMachine();
          if (!sm?.navigateTo) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "No state machine registered. Define states and transitions first.",
              timestamp: Date.now(),
            });
            return true;
          }
          try {
            const result = await Promise.resolve(sm.navigateTo(targetState));
            await sendResponse({
              requestId,
              type,
              success: true,
              data: { navigatedTo: targetState, result: result ?? false },
              timestamp: Date.now(),
            });
          } catch (err) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: err instanceof Error ? err.message : String(err),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        default:
          return false;
      }
    },
    [bridgeRef, sendResponse],
  );
}
