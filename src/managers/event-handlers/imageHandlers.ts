/**
 * Image Handlers
 *
 * Handles image recognition events:
 * - image_recognition: Pattern matching and template detection results
 */

import { logManager } from "../index";
import type { HandlerSetupFunction } from "./types";
import type { ImageRecognitionEventPayload } from "../../types/eventPayloads";
import {
  executionReportingService,
  type ImageRecognitionData,
} from "../../services/ExecutionReportingService";

/**
 * Setup image recognition event handlers
 */
export const setupImageHandlers: HandlerSetupFunction = (context) => {
  const { eventRouter } = context;
  const unsubscribers: Array<() => void> = [];

  // Handler for "image_recognition" event
  unsubscribers.push(
    eventRouter.subscribe("image_recognition", (payload: ImageRecognitionEventPayload) => {
      console.log("[IMAGE_HANDLER] image_recognition event received");
      const data = payload.data;

      if (!data) {
        console.warn("[IMAGE_HANDLER] image_recognition event has no data");
        return;
      }

      // Delegate image recognition processing to LogManager
      logManager.processImageRecognitionData(data);

      // Report image recognition for historical storage (if execution run is active)
      if (executionReportingService.isActive) {
        // Parse location if it's a string
        let location: { x: number; y: number; width?: number; height?: number } | undefined;
        if (typeof data.location === "string") {
          try {
            location = JSON.parse(data.location);
          } catch {
            // Ignore parse errors
          }
        } else if (data.location && typeof data.location === "object") {
          location = data.location;
        }

        // Extract hierarchy data for active states
        let activeStates: string[] = [];
        if (data.hierarchy) {
          const hierarchy =
            typeof data.hierarchy === "string"
              ? (() => {
                  try {
                    return JSON.parse(data.hierarchy);
                  } catch {
                    return null;
                  }
                })()
              : data.hierarchy;
          if (hierarchy?.active_states && Array.isArray(hierarchy.active_states)) {
            activeStates = hierarchy.active_states;
          }
        }

        const recognitionData: ImageRecognitionData = {
          pattern_id: data.template_name || data.node_id || "unknown",
          pattern_name: data.template_name,
          action_type: "FIND",
          active_states: activeStates,
          success: data.found ?? false,
          match_count: data.found ? 1 : 0,
          best_match_score: data.confidence,
          match_x: location?.x,
          match_y: location?.y,
          match_width: location?.width,
          match_height: location?.height,
          result_data: {
            threshold: data.threshold,
            gap: data.gap,
            percent_off: data.percent_off,
            monitor_index: data.monitor_index,
          },
        };

        executionReportingService.reportImageRecognition(recognitionData).catch((error) => {
          console.error("[IMAGE_HANDLER] Failed to report image recognition:", error);
        });
      }
    }),
  );

  return unsubscribers;
};
