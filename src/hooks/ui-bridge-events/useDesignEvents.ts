import { useCallback } from "react";
import {
  getElementDesignData,
  captureResponsiveSnapshots,
  captureStateVariations,
  runStyleAudit,
} from "@qontinui/ui-bridge";
import type {
  ElementDesignData,
  DesignRegistryLike,
  InteractionStateName,
  StyleGuideConfig,
} from "@qontinui/ui-bridge";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";
import type { RegisteredElement } from "./types";
import { createLogger } from "@/lib/logger";

const logger = createLogger("UIBridgeDesignEvents");

/**
 * Iterates over bridge elements, resolves their DOM nodes, and collects
 * design data for each one. Shared by design_get_snapshot, design_evaluate,
 * design_evaluate_baseline, design_evaluate_diff, and design_run_audit.
 */
function collectDesignData(
  elements: readonly RegisteredElement[],
  options?: {
    targetIds?: string[];
    includePseudoElements?: boolean;
  },
): ElementDesignData[] {
  const filtered = options?.targetIds
    ? elements.filter((el) => options.targetIds!.includes(el.id))
    : elements;

  const result: ElementDesignData[] = [];
  for (const el of filtered) {
    let domEl: HTMLElement | null = el.element instanceof HTMLElement ? el.element : null;
    if (!domEl) {
      const selector = el.getIdentifier?.()?.selector;
      if (selector) {
        domEl = document.querySelector<HTMLElement>(selector);
      }
    }
    if (domEl) {
      result.push(
        getElementDesignData(domEl, {
          elementId: el.id,
          label: el.label,
          type: el.type,
          includePseudoElements: options?.includePseudoElements,
        }),
      );
    }
  }
  return result;
}

/**
 * Handles: design_get_snapshot, design_get_element_styles, design_get_state_styles,
 *          design_get_responsive, design_run_audit, design_load_style_guide,
 *          design_get_style_guide, design_clear_style_guide,
 *          design_evaluate, design_evaluate_baseline, design_evaluate_contexts,
 *          design_evaluate_diff
 */
export function useDesignEvents(
  context: Pick<UIBridgeEventContext, "bridgeRef" | "sendResponse" | "loadedStyleGuideRef">,
) {
  const { bridgeRef, sendResponse, loadedStyleGuideRef } = context;

  return useCallback(
    async (payload: UIBridgeRequestPayload): Promise<boolean> => {
      const { requestId, type } = payload;
      const currentBridge = bridgeRef.current;

      switch (type) {
        case "design_get_snapshot": {
          const allElements = currentBridge.elements;
          const targetIds = payload.elementIds as string[] | undefined;

          if (allElements.length === 0) {
            logger.debug("design_get_snapshot: no registered elements in bridge");
          }

          const designData = collectDesignData(allElements, {
            targetIds,
            includePseudoElements: payload.includePseudoElements,
          });

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { elements: designData, timestamp: Date.now() },
            timestamp: Date.now(),
          });
          return true;
        }

        case "design_get_element_styles": {
          const { elementId: styleElementId } = payload;
          if (!styleElementId) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "elementId is required",
              timestamp: Date.now(),
            });
            return true;
          }

          const styleElement = currentBridge.getElement(styleElementId);
          let styleDomEl: HTMLElement | null = null;
          if (styleElement) {
            styleDomEl = styleElement.element instanceof HTMLElement ? styleElement.element : null;
            if (!styleDomEl) {
              const sel = styleElement.getIdentifier?.()?.selector;
              if (sel) {
                styleDomEl = document.querySelector<HTMLElement>(sel);
              }
            }
          }
          if (!styleDomEl) {
            // Try finding element by data-testid as last resort
            styleDomEl = document.querySelector<HTMLElement>(`[data-testid="${styleElementId}"]`);
          }

          if (!styleDomEl) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: `Element not found in DOM: ${styleElementId}`,
              timestamp: Date.now(),
            });
            return true;
          }

          const elementDesignData = getElementDesignData(styleDomEl, {
            elementId: styleElementId,
            label: styleElement?.label,
            type: styleElement?.type,
            includePseudoElements: true,
          });

          await sendResponse({
            requestId,
            type,
            success: true,
            data: elementDesignData,
            timestamp: Date.now(),
          });
          return true;
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

        case "design_get_responsive": {
          const registry = {
            getAllElements: () => currentBridge.elements,
          };
          const viewports = (payload.viewports ??
            payload.params?.viewports ?? { mobile: 375, tablet: 768, desktop: 1280 }) as Record<
            string,
            number
          >;
          const snapshots = await captureResponsiveSnapshots(
            registry as DesignRegistryLike,
            viewports,
          );

          await sendResponse({
            requestId,
            type,
            success: true,
            data: snapshots,
            timestamp: Date.now(),
          });
          return true;
        }

        case "design_run_audit": {
          const auditElements = currentBridge.elements;
          const auditTargetIds =
            payload.elementIds ?? (payload.params?.elementIds as string[] | undefined);

          if (auditElements.length === 0) {
            logger.debug("design_run_audit: no registered elements in bridge");
          }

          const auditDesignData = collectDesignData(auditElements, {
            targetIds: auditTargetIds,
          });

          const guide =
            payload.guide ??
            (payload.params?.guide as StyleGuideConfig | undefined) ??
            loadedStyleGuideRef.current;
          if (!guide) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error:
                "No style guide provided or loaded. Load one with design_load_style_guide first, or pass a guide in the request.",
              timestamp: Date.now(),
            });
            return true;
          }

          const report = runStyleAudit(auditDesignData, guide);
          await sendResponse({
            requestId,
            type,
            success: true,
            data: report,
            timestamp: Date.now(),
          });
          return true;
        }

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

        // ==================================================================
        // Design Evaluation
        // ==================================================================

        case "design_evaluate": {
          const evalParams = payload.params ?? payload.body ?? {};
          const evalTargetIds = evalParams.elementIds as string[] | undefined;

          const evalDesignData = collectDesignData(currentBridge.elements, {
            targetIds: evalTargetIds,
            includePseudoElements: true,
          });

          const evalGuide =
            (evalParams.guide as StyleGuideConfig | undefined) ?? loadedStyleGuideRef.current;
          const evalReport = evalGuide ? runStyleAudit(evalDesignData, evalGuide) : null;

          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              elements: evalDesignData,
              audit: evalReport,
              criteria: evalParams.criteria ?? null,
              timestamp: Date.now(),
            },
            timestamp: Date.now(),
          });
          return true;
        }

        case "design_evaluate_baseline": {
          const baselineParams = payload.params ?? payload.body ?? {};

          const baselineDesignData = collectDesignData(currentBridge.elements, {
            includePseudoElements: true,
          });

          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              baseline: {
                elements: baselineDesignData,
                capturedAt: Date.now(),
                url: window.location.href,
                title: document.title,
                label: baselineParams.label ?? "baseline",
              },
            },
            timestamp: Date.now(),
          });
          return true;
        }

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
          const diffParams = payload.params ?? payload.body ?? {};
          const baselineData = diffParams.baseline as
            | { elements?: ElementDesignData[] }
            | undefined;

          // Capture current design data
          const currentDesignData = collectDesignData(currentBridge.elements, {
            includePseudoElements: true,
          });

          // Simple diff: compare element counts and IDs
          const baselineIds = new Set((baselineData?.elements ?? []).map((e) => e.elementId));
          const currentIds = new Set(currentDesignData.map((e) => e.elementId));
          const added = currentDesignData.filter((e) => !baselineIds.has(e.elementId));
          const removed = (baselineData?.elements ?? []).filter(
            (e) => !currentIds.has(e.elementId),
          );
          const shared = currentDesignData.filter((e) => baselineIds.has(e.elementId));

          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              added: added.length,
              removed: removed.length,
              unchanged: shared.length,
              addedElements: added.map((e) => e.elementId),
              removedElements: removed.map((e) => e.elementId),
              current: { elementCount: currentDesignData.length },
              baseline: { elementCount: baselineData?.elements?.length ?? 0 },
              timestamp: Date.now(),
            },
            timestamp: Date.now(),
          });
          return true;
        }

        default:
          return false;
      }
    },
    [bridgeRef, sendResponse, loadedStyleGuideRef],
  );
}
