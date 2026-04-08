import { useCallback } from "react";
import { compareVisualRegression } from "@qontinui/ui-bridge/ai";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";

/**
 * Handles: find_media, media_audit, capture_media_snapshot, analyze_media,
 *          capture_element_images, get_element_images, media_compare,
 *          image_diff
 */
export function useMediaEvents(context: Pick<UIBridgeEventContext, "bridgeRef" | "sendResponse">) {
  const { bridgeRef, sendResponse } = context;

  return useCallback(
    async (payload: UIBridgeRequestPayload): Promise<boolean> => {
      const { requestId, type } = payload;
      const currentBridge = bridgeRef.current;

      switch (type) {
        case "find_media": {
          const mediaParams = payload.params ?? payload.body ?? {};
          const discovered = await currentBridge.discover({
            ...(mediaParams as Record<string, unknown>),
            mediaOnly: true,
            includeMedia: true,
            includeHidden: true,
          });
          await sendResponse({
            requestId,
            type,
            success: true,
            data: discovered,
            timestamp: Date.now(),
          });
          return true;
        }

        case "media_audit": {
          const auditType = (payload.params ?? payload.body ?? ({} as Record<string, unknown>))
            ?.auditType;
          const discovered = await currentBridge.discover({
            mediaOnly: true,
            includeMedia: true,
            includeHidden: true,
          });
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { auditType, elements: discovered },
            timestamp: Date.now(),
          });
          return true;
        }

        case "capture_media_snapshot":
        case "analyze_media": {
          // These require browser-side canvas operations — relay to SDK
          await sendResponse({
            requestId,
            type,
            success: true,
            data: { relayRequired: true, command: type, payload },
            timestamp: Date.now(),
          });
          return true;
        }

        case "capture_element_images": {
          // Capture element images directly from the DOM via html2canvas.
          // This avoids MSS screen capture which captures whatever is visible
          // on the monitor (wrong when other windows cover the runner).
          const captureParams = (payload.params ?? payload.body ?? {}) as {
            element_ids?: string[];
          };
          const requestedIds = captureParams.element_ids;

          // Dynamically import html2canvas to keep the main bundle lean
          const { default: html2canvas } = await import("html2canvas");

          // Determine which elements to capture
          const snapshot = await currentBridge.createSnapshotAsync();
          const targetElements = snapshot.elements.filter((el) => {
            if (!el.state.visible) return false;
            const rect = el.state.rect;
            if (!rect || rect.width < 2 || rect.height < 2) return false;
            if (requestedIds && requestedIds.length > 0) {
              return requestedIds.includes(el.id);
            }
            return true;
          });

          const captures: Record<string, { base64_png: string; width: number; height: number }> =
            {};

          // Capture elements via html2canvas in parallel batches to stay
          // within the 10-second IPC timeout. Each batch runs concurrently;
          // batches are processed sequentially to limit DOM contention.
          const BATCH_SIZE = 6;
          for (let batchStart = 0; batchStart < targetElements.length; batchStart += BATCH_SIZE) {
            const batch = targetElements.slice(batchStart, batchStart + BATCH_SIZE);
            const results = await Promise.allSettled(
              batch.map(async (el) => {
                const registered = currentBridge.getElement(el.id);
                const domElement = registered?.element;
                if (!domElement) return null;

                // Resolve the effective background color by walking up the DOM.
                // html2canvas with backgroundColor:null produces transparent PNGs,
                // which makes light/white text invisible when the element inherits
                // a dark background from an ancestor.
                let bgColor: string | null = null;
                let node: Element | null = domElement;
                while (node) {
                  const bg = getComputedStyle(node).backgroundColor;
                  if (bg && bg !== "rgba(0, 0, 0, 0)" && bg !== "transparent") {
                    bgColor = bg;
                    break;
                  }
                  node = node.parentElement;
                }

                const canvas = await html2canvas(domElement, {
                  backgroundColor: bgColor || "#0a0a0b", // fallback to runner's dark bg
                  scale: 1, // Use CSS pixel resolution — avoids DPI offset/clipping bugs
                  logging: false,
                  useCORS: true,
                  // Explicitly set the capture window to the element's bounds to prevent
                  // html2canvas from miscalculating the crop area at non-integer DPI scales
                  x: 0,
                  y: 0,
                  width: domElement.scrollWidth,
                  height: domElement.scrollHeight,
                });

                const dataUrl = canvas.toDataURL("image/png");
                const base64 = dataUrl.replace(/^data:image\/png;base64,/, "");

                return {
                  id: el.id,
                  base64_png: base64,
                  width: canvas.width,
                  height: canvas.height,
                };
              }),
            );

            for (const result of results) {
              if (result.status === "fulfilled" && result.value) {
                const { id, ...data } = result.value;
                captures[id] = data;
              } else if (result.status === "rejected") {
                console.warn(`[UIBridgeEventHandler] Failed to capture element:`, result.reason);
              }
            }
          }

          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              captures,
              element_count: Object.keys(captures).length,
              requested_count: targetElements.length,
              device_pixel_ratio: window.devicePixelRatio || 1,
            },
            timestamp: Date.now(),
          });
          return true;
        }

        case "get_element_images": {
          // Read <img> element src attributes from the DOM without rendering.
          // Much lighter than capture_element_images (no html2canvas).
          const imgParams = (payload.params ?? payload.body ?? {}) as {
            element_id?: string;
            max_images?: number;
            full_src?: boolean;
            image_index?: number;
          };
          const maxImages = imgParams.max_images || 50;
          const fullSrc = imgParams.full_src || false;
          const imageIndex = imgParams.image_index;

          let container: Element = document.body;
          if (imgParams.element_id) {
            const el = currentBridge?.getElement(imgParams.element_id);
            if (el?.element) container = el.element;
          }

          const imgs = container.querySelectorAll("img");
          const results: Array<{
            src: string;
            alt: string;
            width: number;
            height: number;
            naturalWidth: number;
            naturalHeight: number;
            parentId: string;
            visible: boolean;
          }> = [];

          for (const img of Array.from(imgs).slice(0, maxImages)) {
            const rect = img.getBoundingClientRect();
            const visible =
              rect.width > 0 && rect.height > 0 && getComputedStyle(img).visibility !== "hidden";

            // Find nearest meaningful parent ID
            let parentId = "";
            let parent = img.parentElement;
            while (parent && !parentId) {
              if (parent.getAttribute("data-testid") || parent.id) {
                parentId = parent.getAttribute("data-testid") || parent.id;
              }
              parent = parent.parentElement;
            }

            // For data: URIs, truncate unless full_src requested for this specific index
            let src = img.src;
            if (src.startsWith("data:") && !(fullSrc && results.length === imageIndex)) {
              src = src.substring(0, 100) + "...";
            }

            results.push({
              src,
              alt: img.alt || "",
              width: Math.round(rect.width),
              height: Math.round(rect.height),
              naturalWidth: img.naturalWidth,
              naturalHeight: img.naturalHeight,
              parentId,
              visible,
            });
          }

          await sendResponse({
            requestId,
            type,
            success: true,
            data: { images: results, total: results.length },
            timestamp: Date.now(),
          });
          return true;
        }

        case "image_diff": {
          // Pixel-accurate visual regression diff between two arbitrary
          // base64 PNGs (NOT a DOM media-element diff like media_compare).
          // Wraps `compareVisualRegression` from @qontinui/ui-bridge.
          //
          // Body shape:
          //   { a: "data:image/png;base64,..." | "<base64>",
          //     b: "data:image/png;base64,..." | "<base64>",
          //     options?: { pixelThreshold?, failureThreshold?,
          //                 failureThresholdType?, blur? } }
          const diffParams = (payload.params ?? payload.body ?? {}) as {
            a?: string;
            b?: string;
            options?: {
              pixelThreshold?: number;
              failureThreshold?: number;
              failureThresholdType?: "percent" | "pixels";
              blur?: number;
            };
          };

          if (!diffParams.a || !diffParams.b) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: "image_diff requires both `a` and `b` base64 image strings",
              timestamp: Date.now(),
            });
            return true;
          }

          // compareVisualRegression accepts MediaSnapshotData. We only need
          // the `data` field; the rest is metadata for callers that already
          // have a captured snapshot.
          const wrap = (data: string) => ({
            data: data.startsWith("data:") ? data : `data:image/png;base64,${data}`,
            width: 0,
            height: 0,
            mediaType: "image" as const,
            elementId: "",
            timestamp: Date.now(),
          });

          try {
            const result = await compareVisualRegression(
              wrap(diffParams.a),
              wrap(diffParams.b),
              diffParams.options ?? {},
            );
            await sendResponse({
              requestId,
              type,
              success: true,
              data: result,
              timestamp: Date.now(),
            });
          } catch (e) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: e instanceof Error ? e.message : String(e),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "media_compare": {
          const compareParams = (payload.params ?? payload.body ?? {}) as {
            element_ids?: string[];
            baseline?: { images: Array<{ src: string; alt: string }> };
          };

          // Discover current media elements
          const discovered = await currentBridge.discover({
            mediaOnly: true,
            includeMedia: true,
            includeHidden: true,
          });

          // If specific element IDs requested, filter
          const targetIds = compareParams.element_ids;
          const mediaElements = targetIds
            ? discovered.elements.filter((el: { id: string }) => targetIds.includes(el.id))
            : discovered.elements;

          // Gather current media info
          const currentMedia = mediaElements.map(
            (el: { id: string; type: string; label?: string; element?: Element }) => {
              const domEl = el.element;
              let src = "";
              let alt = "";
              if (domEl instanceof HTMLImageElement) {
                src = domEl.src;
                alt = domEl.alt;
              } else if (domEl instanceof HTMLVideoElement) {
                src = domEl.src || domEl.currentSrc;
              }
              return { id: el.id, type: el.type, label: el.label, src, alt };
            },
          );

          const baselineMedia = compareParams.baseline?.images ?? [];
          const currentSrcs = new Set(currentMedia.map((m: { src: string }) => m.src));
          const baselineSrcs = new Set(baselineMedia.map((m) => m.src));
          const added = currentMedia.filter((m: { src: string }) => !baselineSrcs.has(m.src));
          const removed = baselineMedia.filter((m) => !currentSrcs.has(m.src));

          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              current: currentMedia,
              added: added.length,
              removed: removed.length,
              addedMedia: added,
              removedMedia: removed,
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
    [bridgeRef, sendResponse],
  );
}
