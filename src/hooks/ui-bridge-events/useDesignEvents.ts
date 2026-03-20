import { useCallback } from "react";
import {
  getElementDesignData,
  captureResponsiveSnapshots,
  captureStateVariations,
  runStyleAudit,
} from "ui-bridge";
import type {
  ElementDesignData,
  DesignRegistryLike,
  InteractionStateName,
  StyleGuideConfig,
} from "ui-bridge";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";

/**
 * Handles: design_get_snapshot, design_get_element_styles, design_get_state_styles,
 *          design_get_responsive, design_run_audit, design_load_style_guide,
 *          design_get_style_guide, design_clear_style_guide
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

          const designData: ElementDesignData[] = [];

          if (allElements.length > 0) {
            // Use registered elements from bridge
            const filtered = targetIds
              ? allElements.filter((el) => targetIds.includes(el.id))
              : allElements;

            for (const el of filtered) {
              let domEl: HTMLElement | null =
                el.element instanceof HTMLElement ? el.element : null;
              if (!domEl) {
                const selector = el.getIdentifier?.()?.selector;
                if (selector) {
                  domEl = document.querySelector<HTMLElement>(selector);
                }
              }
              if (domEl) {
                designData.push(
                  getElementDesignData(domEl, {
                    elementId: el.id,
                    label: el.label,
                    type: el.type,
                    includePseudoElements: payload.includePseudoElements,
                  }),
                );
              }
            }
          } else {
            // No registered elements available — nothing to report
            console.log(
              "[UIBridgeEventHandler] design_get_snapshot: no registered elements in bridge",
            );
          }

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
            styleDomEl =
              styleElement.element instanceof HTMLElement ? styleElement.element : null;
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
            stateDomEl =
              stateElement.element instanceof HTMLElement ? stateElement.element : null;
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

          const auditDesignData: ElementDesignData[] = [];
          if (auditElements.length > 0) {
            const auditFiltered = auditTargetIds
              ? auditElements.filter((el) => auditTargetIds.includes(el.id))
              : auditElements;
            for (const el of auditFiltered) {
              let domEl: HTMLElement | null =
                el.element instanceof HTMLElement ? el.element : null;
              if (!domEl) {
                const sel = el.getIdentifier?.()?.selector;
                if (sel) {
                  domEl = document.querySelector<HTMLElement>(sel);
                }
              }
              if (domEl) {
                auditDesignData.push(
                  getElementDesignData(domEl, {
                    elementId: el.id,
                    label: el.label,
                    type: el.type,
                  }),
                );
              }
            }
          } else {
            // No registered elements available — nothing to audit
            console.log(
              "[UIBridgeEventHandler] design_run_audit: no registered elements in bridge",
            );
          }

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

        default:
          return false;
      }
    },
    [bridgeRef, sendResponse, loadedStyleGuideRef],
  );
}
