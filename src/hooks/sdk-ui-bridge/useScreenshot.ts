/**
 * useScreenshot — Screenshot and highlighting sub-hook.
 */

import { useState, useCallback } from "react";
import type { PageScreenshot } from "./types";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

export interface UseScreenshotReturn {
  pageScreenshot: PageScreenshot | null;
  isCapturingScreenshot: boolean;
  captureScreenshot: (monitor?: number) => Promise<void>;
  highlightElement: (elementId: string) => Promise<void>;
}

export function useScreenshot(): UseScreenshotReturn {
  const [pageScreenshot, setPageScreenshot] = useState<PageScreenshot | null>(null);
  const [isCapturingScreenshot, setIsCapturingScreenshot] = useState(false);

  const captureScreenshot = useCallback(async (monitor?: number) => {
    setIsCapturingScreenshot(true);
    try {
      const params = new URLSearchParams();
      if (monitor !== undefined) {
        params.set("monitor", String(monitor));
      }
      const queryString = params.toString();
      const url = `${getApiBase()}/ui-bridge/sdk/screenshot${queryString ? `?${queryString}` : ""}`;

      const resp = await tracedFetch(url);
      const json = await resp.json();

      if (json.success && json.data) {
        setPageScreenshot({
          data: json.data.screenshot,
          capturedAt: Date.now(),
          viewport: {
            width: json.data.width,
            height: json.data.height,
          },
        });
      } else {
        console.warn("[useScreenshot] Screenshot capture failed:", json.error || "Unknown error");
      }
    } catch (e) {
      console.warn(
        "[useScreenshot] Screenshot capture error:",
        e instanceof Error ? e.message : e,
      );
    } finally {
      setIsCapturingScreenshot(false);
    }
  }, []);

  const highlightElement = useCallback(async (elementId: string) => {
    try {
      await tracedFetch(
        `${getApiBase()}/ui-bridge/sdk/debug/highlight/${encodeURIComponent(elementId)}`,
        {
          method: "POST",
        },
      );
    } catch {
      // Best-effort highlight
    }
  }, []);

  return {
    pageScreenshot,
    isCapturingScreenshot,
    captureScreenshot,
    highlightElement,
  };
}
