/**
 * ImageLogHandler
 *
 * Responsible for processing image recognition events and creating log entries.
 * Follows Single Responsibility Principle - handles only image log processing.
 */

import type { ImageRecognitionEntry } from "./LogStore";
import { TimestampFormatter } from "./TimestampFormatter";
import type {
  ImageRecognitionEventData,
  ImageRecognitionLocation,
} from "../../types/eventPayloads";

/**
 * Handles processing of image recognition data into log entries.
 */
export class ImageLogHandler {
  private timestampFormatter: TimestampFormatter;

  constructor(timestampFormatter: TimestampFormatter) {
    this.timestampFormatter = timestampFormatter;
  }

  /**
   * Process raw image recognition data into a structured log entry
   */
  processImageRecognitionData(data: ImageRecognitionEventData): ImageRecognitionEntry {
    // Format timestamps
    const timestamp = this.timestampFormatter.formatCurrentTime();

    let screenshotTimestamp: string | undefined;
    if (data.screenshot_timestamp) {
      screenshotTimestamp = this.timestampFormatter.formatUnixTimestamp(data.screenshot_timestamp);
    }

    // Format location - backend can send location as string "(x, y)" or object
    const location = this.parseLocation(data.location);

    // Calculate gap and percent off - use data from backend if available
    const { gap, percentOff, bestMatchLocation } = this.calculateMatchMetrics(data);

    // Extract node ID
    const nodeId = this.extractNodeId(data);

    return {
      id: `img-${Date.now()}-${Math.random()}`,
      timestamp,
      screenshotTimestamp,
      node: nodeId,
      template: data.template_name || data.image_path || "unknown",
      confidence: data.confidence || 0,
      found: data.found || false,
      threshold: data.threshold || 0,
      location,
      gap,
      percentOff,
      bestMatchLocation,
      screenshotPath: data.screenshot_path,
      screenshotData: data.screenshot_base64,
      visualDebugImage: data.visual_debug_image,
      templatePath: data.template_path,
      imageData: data.image_data,
      matchedRegionImage: data.matched_region_image, // Cropped region from screenshot at match location
      debug: data.debug,
      monitorIndex: data.monitor_index, // Which monitor was captured (null/undefined = all monitors)
    };
  }

  /**
   * Parse location from various formats
   */
  private parseLocation(
    location: ImageRecognitionLocation | string | undefined,
  ): { x: number; y: number; width: number; height: number } | undefined {
    if (!location) {
      return undefined;
    }

    if (typeof location === "string") {
      // Parse "(x, y)" format
      const match = location.match(/\((\d+),\s*(\d+)\)/);
      if (match) {
        return {
          x: parseInt(match[1]),
          y: parseInt(match[2]),
          width: 0,
          height: 0,
        };
      }
    } else if (typeof location === "object") {
      return {
        x: location.x,
        y: location.y,
        width: location.width || 0,
        height: location.height || 0,
      };
    }

    return undefined;
  }

  /**
   * Calculate match quality metrics
   */
  private calculateMatchMetrics(data: ImageRecognitionEventData): {
    gap?: number;
    percentOff?: number;
    bestMatchLocation?: string;
  } {
    let gap: number | undefined = data.gap;
    let percentOff: number | undefined = data.percent_off;
    let bestMatchLocation: string | undefined = data.best_match_location;

    // Fallback: calculate from debug data if not provided
    if (!data.found && !gap && data.debug?.top_matches && data.debug.top_matches.length > 0) {
      const bestMatch = data.debug.top_matches[0];
      gap = (data.threshold ?? 0) - bestMatch.confidence;
      percentOff = data.threshold ? (gap / data.threshold) * 100 : 0;
      bestMatchLocation = `(${bestMatch.location.x}, ${bestMatch.location.y})`;
    }

    return { gap, percentOff, bestMatchLocation };
  }

  /**
   * Extract node ID from hierarchy data
   */
  private extractNodeId(data: ImageRecognitionEventData): string {
    if (data.node_id) {
      return data.node_id;
    }

    if (data.hierarchy && typeof data.hierarchy === "object") {
      // Hierarchy is {nesting_level, parent_id, workflow_name}
      return data.hierarchy.workflow_name || data.hierarchy.parent_id || "unknown";
    }

    if (typeof data.hierarchy === "string") {
      return data.hierarchy;
    }

    return "unknown";
  }
}
