import { useCallback } from "react";
import { compareVisualRegression } from "@qontinui/ui-bridge/ai";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";
import { describeThrown } from "@/lib/utils";

/**
 * Handles: image_diff — the frontend half of the live
 * `POST /ui-bridge/ai/image-diff` (`mcp/ui_bridge/ai_analyze.rs`).
 *
 * The other media cases (find_media, media_audit, capture_media_snapshot,
 * analyze_media, capture_element_images, get_element_images,
 * capture_single_element, media_compare) were deleted with the dead
 * `/ui-bridge/sdk/ai/media/*` proxies: nothing in the runner sent them
 * (plan 2026-10-13-image-generation-and-visual-judgment, Phase 6). Image
 * capture and analysis go through `/ui-bridge/vision/*`.
 */
export function useMediaEvents(context: Pick<UIBridgeEventContext, "sendResponse">) {
  const { sendResponse } = context;

  return useCallback(
    async (payload: UIBridgeRequestPayload): Promise<boolean> => {
      const { requestId, type } = payload;

      switch (type) {
        case "image_diff": {
          // Pixel-accurate visual regression diff between two arbitrary
          // base64 PNGs (NOT a DOM media-element diff).
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
              failureThresholdType?: "percent" | "pixel";
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

          // compareVisualRegression calls loadBase64Image() which ALWAYS
          // prepends "data:image/png;base64," to the .data field. So we
          // must strip any existing data-URL prefix to avoid double-
          // prefixing ("data:...data:...") which produces a 0x0 image.
          const stripDataUrl = (s: string) => s.replace(/^data:image\/[^;]+;base64,/, "");

          const wrap = (data: string) => ({
            data: stripDataUrl(data),
            width: 0,
            height: 0,
            mediaType: "image/png" as const,
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
              error: describeThrown(e, "image_diff failed"),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        default:
          return false;
      }
    },
    [sendResponse],
  );
}
