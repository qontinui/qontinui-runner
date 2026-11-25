import { useCallback, useRef } from "react";
import {
  captureQualityBaseline,
  captureStateVariations,
  DesignRequestError,
  designAuditOf,
  designFailureCode,
  designSnapshotOf,
  diffRegisteredDesignTokens,
  diffQualityBaseline,
  elementStylesOf,
  evaluateRegisteredQuality,
  inferRegisteredStyleGuide,
  qualityFailureCode,
  responsiveSnapshotsOf,
  styleInferenceFailureCode,
  viewportConstraintsOf,
} from "@qontinui/ui-bridge";
import type { InteractionStateName, SnapshotBaseline, StyleGuideConfig } from "@qontinui/ui-bridge";
import { describeThrown } from "@/lib/utils";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";

/**
 * Body of a request routed through the Rust `{params: body}` wrapping. Every
 * design route this hook answers through the shared runtime sends its HTTP
 * body as `params` (`mcp/ui_bridge/design.rs`, `design_eval.rs`, `page.rs`),
 * so `params` is the ONE place a field is read from.
 */
function paramsOf(payload: UIBridgeRequestPayload): Record<string, unknown> {
  return payload.params ?? {};
}

/**
 * Handles: design_get_snapshot, design_get_element_styles, design_get_state_styles,
 *          design_get_responsive, design_run_audit, design_load_style_guide,
 *          design_get_style_guide, design_clear_style_guide,
 *          design_infer_style_guide, design_diff_tokens,
 *          design_evaluate, design_evaluate_baseline, design_evaluate_contexts,
 *          design_evaluate_diff, set_viewport_constraints
 *
 * The design-inspection, quality-evaluation and viewport-constraint requests
 * run through ui-bridge's shared request runtime (`specs/design-request`,
 * `specs/quality-request`) — the same functions the SDK's own server handlers
 * and in-page command dispatcher call — so the runner window answers with one
 * model whichever transport reached it: bounded and reported selections, the
 * real quality diff, and viewport constraints serialized with responsive
 * captures under the root-width lock. A request the runtime refuses answers
 * `{success:false, error, code}` with the runtime's own failure code, which
 * `wrap_ipc_result` maps onto a typed HTTP error (`TYPED_FRONTEND_CODES`).
 *
 * `design_get_state_styles` and `design_evaluate_contexts` stay inline: the
 * SDK has no shared request function for either.
 */
export function useDesignEvents(
  context: Pick<UIBridgeEventContext, "bridgeRef" | "sendResponse" | "loadedStyleGuideRef">,
) {
  const { bridgeRef, sendResponse, loadedStyleGuideRef } = context;
  // The baseline `design_evaluate_diff` compares against, as the SDK's server
  // keeps its `savedBaseline`: captured by `design_evaluate_baseline`, held
  // for the life of the window.
  const qualityBaselineRef = useRef<SnapshotBaseline | null>(null);

  return useCallback(
    async (payload: UIBridgeRequestPayload): Promise<boolean> => {
      const { requestId, type } = payload;
      const currentBridge = bridgeRef.current;

      /**
       * Answer with what `run` returns, or — when it throws — with the error's
       * message and the failure code `failureCode` assigns it. The response is
       * sent outside the `try`, so a failed send is never re-reported as a
       * request failure.
       */
      const answer = async (
        run: () => unknown,
        failureCode: (err: unknown) => string,
      ): Promise<boolean> => {
        let outcome: { ok: true; data: unknown } | { ok: false; error: string; code: string };
        try {
          outcome = { ok: true, data: await run() };
        } catch (err) {
          outcome = {
            ok: false,
            error: describeThrown(err, `${type} failed`),
            code: failureCode(err),
          };
        }
        await sendResponse(
          outcome.ok
            ? { requestId, type, success: true, data: outcome.data, timestamp: Date.now() }
            : {
                requestId,
                type,
                success: false,
                error: outcome.error,
                code: outcome.code,
                timestamp: Date.now(),
              },
        );
        return true;
      };
      const design = (routeCode: string) => (err: unknown) => designFailureCode(err, routeCode);
      const inference = (routeCode: string) => (err: unknown) =>
        styleInferenceFailureCode(err, routeCode);
      const quality = (routeCode: string) => (err: unknown) => qualityFailureCode(err, routeCode);

      switch (type) {
        case "design_get_snapshot":
          return answer(
            () => designSnapshotOf(currentBridge.elements, payload.params),
            design("DESIGN_SNAPSHOT_ERROR"),
          );

        case "design_get_element_styles": {
          const id = paramsOf(payload).elementId;
          return answer(() => {
            if (typeof id !== "string" || id === "") {
              throw new DesignRequestError("elementId is required", "INVALID_REQUEST");
            }
            return elementStylesOf(id, currentBridge.getElement(id));
          }, design("DESIGN_STYLES_ERROR"));
        }

        case "design_get_state_styles": {
          const { elementId: stateElementId } = payload;
          if (!stateElementId) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "elementId is required",
              timestamp: Date.now(),
            });
            return true;
          }

          const stateElement = currentBridge.getElement(stateElementId);
          let stateDomEl: HTMLElement | null = null;
          if (stateElement) {
            stateDomEl = stateElement.element instanceof HTMLElement ? stateElement.element : null;
            if (!stateDomEl) {
              const sel = stateElement.getIdentifier?.()?.selector;
              if (sel) {
                stateDomEl = document.querySelector<HTMLElement>(sel);
              }
            }
          }
          if (!stateDomEl) {
            stateDomEl = document.querySelector<HTMLElement>(`[data-testid="${stateElementId}"]`);
          }

          if (!stateDomEl) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `Element not found in DOM: ${stateElementId}`,
              timestamp: Date.now(),
            });
            return true;
          }

          const states =
            (payload.params?.states as InteractionStateName[] | undefined) ?? undefined;
          const stateVariations = await captureStateVariations(stateDomEl, states);

          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              elementId: stateElementId,
              states: stateVariations,
            },
            timestamp: Date.now(),
          });
          return true;
        }

        case "design_get_responsive":
          // The registry is read inside the root-width lock, once this
          // capture's turn comes — not when the request arrived.
          return answer(
            () => responsiveSnapshotsOf(() => bridgeRef.current.elements, payload.params),
            design("RESPONSIVE_SNAPSHOT_ERROR"),
          );

        case "design_run_audit":
          // The request's own `guide` wins inside `designAuditOf`; the loaded
          // one is the fallback, and with neither the heuristics run.
          return answer(
            () =>
              designAuditOf(currentBridge.elements, payload.params, loadedStyleGuideRef.current),
            design("DESIGN_AUDIT_ERROR"),
          );

        case "design_load_style_guide": {
          const guideToLoad =
            payload.guide ?? (payload.params?.guide as StyleGuideConfig | undefined);
          if (!guideToLoad) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "guide is required",
              timestamp: Date.now(),
            });
            return true;
          }
          loadedStyleGuideRef.current = guideToLoad;
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { loaded: true },
            timestamp: Date.now(),
          });
          return true;
        }

        case "design_get_style_guide": {
          await sendResponse({
            requestId,
            type,
            success: true,
            data: loadedStyleGuideRef.current,
            timestamp: Date.now(),
          });
          return true;
        }

        case "design_clear_style_guide": {
          loadedStyleGuideRef.current = null;
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { cleared: true },
            timestamp: Date.now(),
          });
          return true;
        }

        case "design_infer_style_guide":
          return answer(
            () => inferRegisteredStyleGuide(currentBridge.elements, payload.params),
            inference("STYLE_INFERENCE_ERROR"),
          );

        case "design_diff_tokens":
          // `loadedStyleGuideRef` backs the request's `loadedStyleGuide` source.
          return answer(
            () =>
              diffRegisteredDesignTokens(
                currentBridge.elements,
                payload.params,
                loadedStyleGuideRef.current,
              ),
            inference("TOKEN_DRIFT_ERROR"),
          );

        // ==================================================================
        // Design Evaluation
        // ==================================================================

        case "design_evaluate": {
          const guide =
            (paramsOf(payload).guide as StyleGuideConfig | undefined) ??
            loadedStyleGuideRef.current;
          return answer(
            () => evaluateRegisteredQuality(currentBridge.elements, payload.params, guide),
            quality("QUALITY_EVALUATION_ERROR"),
          );
        }

        case "design_evaluate_baseline":
          return answer(() => {
            const baseline = captureQualityBaseline(currentBridge.elements, payload.params);
            qualityBaselineRef.current = baseline;
            return { saved: true, elementCount: baseline.elements.length };
          }, quality("SAVE_BASELINE_ERROR"));

        case "design_evaluate_contexts": {
          // Return current design evaluation contexts (viewport, theme, etc.)
          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              contexts: {
                viewport: {
                  width: window.innerWidth,
                  height: window.innerHeight,
                  devicePixelRatio: window.devicePixelRatio,
                },
                theme: {
                  prefersDark: window.matchMedia("(prefers-color-scheme: dark)").matches,
                  prefersReducedMotion: window.matchMedia("(prefers-reduced-motion: reduce)")
                    .matches,
                },
                url: window.location.href,
                title: document.title,
                styleGuideLoaded: loadedStyleGuideRef.current !== null,
              },
              timestamp: Date.now(),
            },
            timestamp: Date.now(),
          });
          return true;
        }

        case "design_evaluate_diff": {
          const baseline = qualityBaselineRef.current;
          if (!baseline) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "No baseline saved. Call design_evaluate_baseline first.",
              code: "NO_BASELINE",
              timestamp: Date.now(),
            });
            return true;
          }
          return answer(
            () => diffQualityBaseline(baseline, currentBridge.elements, payload.params),
            quality("DIFF_BASELINE_ERROR"),
          );
        }

        case "set_viewport_constraints":
          // Serialized with responsive captures under the root-width lock, and
          // narrows the root exactly as they do; `constrainedWidth` is measured.
          return answer(
            () => viewportConstraintsOf(payload.params),
            design("VIEWPORT_CONSTRAINTS_ERROR"),
          );

        default:
          return false;
      }
    },
    [bridgeRef, sendResponse, loadedStyleGuideRef],
  );
}
