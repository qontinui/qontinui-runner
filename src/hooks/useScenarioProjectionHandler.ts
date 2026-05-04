/**
 * useScenarioProjectionHandler
 *
 * Section 11 / Phase B2 — frontend half of the runtime-aware scenario
 * projection IPC.
 *
 * Architecture:
 * ```
 * External HTTP Client
 *     | GET /scenarios/current?ir_doc_id=<id>
 *     v
 * Axum handler (scenarios/handlers.rs)
 *     | loads IR from spec_api::storage
 *     | emit("ui-bridge:project-current-scenario-request",
 *     |       { request_id, ir })
 *     v
 * This Hook
 *     | projectCurrentScenario(ir, registryAdapter)
 *     v
 * @qontinui/ui-bridge-auto/regression — pure TS projection
 *     | returns CurrentScenarioProjection (deterministic: false)
 *     v
 * This Hook
 *     | emit("ui-bridge:project-current-scenario-response",
 *     |       { request_id, ok, result, error })
 *     v
 * Axum listener (mcp_api.rs) — delivers to the pending oneshot in
 * `InvokeRequestStore` → HTTP handler returns the projection JSON.
 * ```
 *
 * The projection function itself ships from `@qontinui/ui-bridge-auto`
 * (Phase B1 — exhaustively tested) so this hook is a thin shell around it
 * plus the registry adapter that wraps the UI Bridge SDK's global registry.
 *
 * Why a separate hook from `UIBridgeInvokeHandler`:
 *   - That hook forwards to `invoke(command, args)` (Rust commands), but
 *     the projection is computed in TS — the registry is a JS object in
 *     the webview, not reachable from Rust without round-tripping through
 *     `page/evaluate`.
 *   - Keeping a dedicated channel makes the contract explicit: the Rust
 *     side knows its IPC waits for a TS-computed value, not a Tauri
 *     command result.
 */

import { useEffect } from "react";
import { listen, emit, type UnlistenFn } from "@tauri-apps/api/event";
import {
  projectCurrentScenario,
  type CurrentScenarioProjection,
} from "@qontinui/ui-bridge-auto/regression";
import type { RegistryLike, QueryableElement } from "@qontinui/ui-bridge-auto/runtime";
// Same import path as useStateMachineRegistration so we share the singleton
// globalRegistry — importing from "@qontinui/ui-bridge/core" resolves to a
// separate dist bundle with its own isolated registry, making elements
// invisible to the projection.
import { getGlobalRegistry } from "@qontinui/ui-bridge";
import { createLogger } from "@/lib/logger";
import { getErrorMessage } from "@/lib/utils";

const log = createLogger("ScenarioProjectionHandler");

const REQUEST_EVENT = "ui-bridge:project-current-scenario-request";
const RESPONSE_EVENT = "ui-bridge:project-current-scenario-response";

interface ProjectionRequestPayload {
  request_id: string;
  // The IR document, serialized via serde with `rename_all = "camelCase"` —
  // the TS `IRDocument` shape is the same camelCase, so we forward verbatim.
  ir: unknown;
}

interface ProjectionResponsePayload {
  request_id: string;
  ok: boolean;
  result?: CurrentScenarioProjection;
  error?: string;
}

/**
 * Build a `RegistryLike` adapter on top of the UI Bridge SDK's global
 * element registry. Same shape as `createRegistryAdapter` in
 * `useStateMachineRegistration` — kept in-file so this hook stays
 * self-contained (no implicit cross-hook dependency).
 */
function createRegistryAdapter(): RegistryLike {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const registry = getGlobalRegistry() as any;
  return {
    getAllElements(): QueryableElement[] {
      return registry
        .getAllElements()
        .map(
          (el: {
            id: string;
            element: HTMLElement;
            type: string;
            label?: string;
            getState: () => Record<string, unknown>;
            getIdentifier?: () => Record<string, string | undefined>;
          }) => ({
            id: el.id,
            element: el.element,
            type: el.type,
            label: el.label,
            getState: () => {
              const s = el.getState();
              return {
                visible: s.visible as boolean | undefined,
                enabled: s.enabled as boolean | undefined,
                focused: s.focused as boolean | undefined,
                checked: s.checked as boolean | undefined,
                textContent: s.textContent as string | undefined,
                value: s.value as string | undefined,
                rect: s.rect as DOMRect | undefined,
              };
            },
            getIdentifier: el.getIdentifier
              ? () => {
                  const ident = el.getIdentifier!();
                  return {
                    selector: ident.selector,
                    xpath: ident.xpath,
                    htmlId: ident.htmlId,
                  };
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

/**
 * Install a Tauri listener on `ui-bridge:project-current-scenario-request`
 * that runs `projectCurrentScenario` against the live registry and emits
 * the result back over `ui-bridge:project-current-scenario-response`.
 *
 * Safe to mount once at app root alongside `UIBridgeInvokeHandler`.
 * Multiple concurrent projections are supported (each carries its own
 * `request_id`).
 */
export function useScenarioProjectionHandler(): void {
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let isMounted = true;

    const setupListener = async () => {
      try {
        log.debug(`Setting up ${REQUEST_EVENT} listener`);

        unlisten = await listen<ProjectionRequestPayload>(REQUEST_EVENT, async (event) => {
          if (!isMounted) {
            log.debug("Component unmounted, ignoring projection request");
            return;
          }

          const { request_id, ir } = event.payload;
          log.debug(`Received projection request`, request_id);

          let response: ProjectionResponsePayload;
          try {
            // The Rust side validated the IR exists in storage; we trust
            // its shape here. The pure projection function is what
            // enforces the contract — if the IR is malformed it will
            // throw and we surface the error to the HTTP caller.
            // eslint-disable-next-line @typescript-eslint/no-explicit-any
            const irDoc = ir as any;
            const registry = createRegistryAdapter();
            const result = projectCurrentScenario(irDoc, registry);
            response = {
              request_id,
              ok: true,
              result,
            };
          } catch (err) {
            const message = getErrorMessage(err);
            log.debug(`projectCurrentScenario threw: ${message}`);
            response = {
              request_id,
              ok: false,
              error: message,
            };
          }

          try {
            await emit(RESPONSE_EVENT, response);
          } catch (emitErr) {
            // If emit itself fails the Rust side will hit its timeout —
            // there's no better fallback here. Log and move on.
            console.error(
              "[ScenarioProjectionHandler] Failed to emit projection response:",
              emitErr,
            );
          }
        });

        log.debug("projection-request listener set up successfully");
      } catch (error) {
        console.error(
          "[ScenarioProjectionHandler] Failed to set up projection-request listener:",
          error,
        );
      }
    };

    setupListener();

    return () => {
      log.debug("Cleaning up projection-request listener");
      isMounted = false;
      if (unlisten) {
        unlisten();
      }
    };
  }, []);
}

/**
 * Component wrapper for the hook (matches the `UIBridgeInvokeHandler`
 * pattern so it can be placed alongside it in the JSX tree).
 */
export function ScenarioProjectionHandler(): null {
  useScenarioProjectionHandler();
  return null;
}
