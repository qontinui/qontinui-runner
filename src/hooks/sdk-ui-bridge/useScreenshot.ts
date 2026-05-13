/**
 * useScreenshot — Screenshot and highlighting sub-hook.
 *
 * Migrated 2026-05-13 to the `/ui-bridge/vision/*` route set (Phase 2 of the
 * UI Bridge vision-pipeline plan). The legacy `/ui-bridge/sdk/screenshot`
 * route was deleted along with the underlying `screenshot_command`. The
 * vision/capture endpoint always captures the runner's own window — the
 * legacy `monitor` query param is no longer wired through; we accept it for
 * API compatibility but it is ignored by Phase 2 (and unused by current
 * callers).
 */

import { useState, useCallback } from "react";
import type { PageScreenshot } from "./types";
import { captureScreenshotBase64 } from "@/lib/vision-api";
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

  const captureScreenshot = useCallback(async (_monitor?: number) => {
    setIsCapturingScreenshot(true);
    try {
      // Phase 2 vision/capture always targets the runner window; the legacy
      // `monitor` parameter is accepted for API compatibility but ignored.
      const result = await captureScreenshotBase64({ contract: "claude" });
      setPageScreenshot({
        data: result.screenshot,
        capturedAt: Date.now(),
        viewport: {
          width: result.width,
          height: result.height,
        },
      });
    } catch (e) {
      console.warn("[useScreenshot] Screenshot capture error:", e instanceof Error ? e.message : e);
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
