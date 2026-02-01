/**
 * useElementThumbnails Hook
 *
 * Manages thumbnail generation for UI Bridge elements by cropping
 * from a full page screenshot.
 */

import { useState, useEffect, useRef } from "react";
import { cropThumbnails, type ElementBounds } from "../lib/thumbnail-cropper";

export interface ElementWithBounds {
  id: string;
  bounds: ElementBounds;
}

export interface UseElementThumbnailsReturn {
  /** Map of element ID to base64 thumbnail data */
  thumbnails: Map<string, string>;
  /** Whether thumbnails are currently being processed */
  isProcessing: boolean;
  /** Error message if processing failed */
  error: string | null;
}

/**
 * Hook to generate thumbnails for elements from a page screenshot.
 *
 * @param elements - Array of elements with id and bounds
 * @param screenshotBase64 - Base64 encoded screenshot (without data URL prefix)
 * @param maxSize - Maximum thumbnail dimension (default: 40)
 * @returns Thumbnails map, processing state, and error
 */
export function useElementThumbnails(
  elements: ElementWithBounds[],
  screenshotBase64: string | null,
  maxSize: number = 40
): UseElementThumbnailsReturn {
  const [thumbnails, setThumbnails] = useState<Map<string, string>>(new Map());
  const [isProcessing, setIsProcessing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Track the screenshot we processed to avoid re-processing
  const lastScreenshotRef = useRef<string | null>(null);
  const lastElementCountRef = useRef<number>(0);

  useEffect(() => {
    // Clear thumbnails if no screenshot or elements
    if (!screenshotBase64 || elements.length === 0) {
      setThumbnails(new Map());
      setError(null);
      lastScreenshotRef.current = null;
      lastElementCountRef.current = 0;
      return;
    }

    // Skip if we already processed this screenshot with same element count
    if (
      screenshotBase64 === lastScreenshotRef.current &&
      elements.length === lastElementCountRef.current
    ) {
      return;
    }

    // Process thumbnails
    let cancelled = false;

    const processThumbnails = async () => {
      setIsProcessing(true);
      setError(null);

      try {
        const result = await cropThumbnails(screenshotBase64, elements, { maxSize });

        if (!cancelled) {
          setThumbnails(result);
          lastScreenshotRef.current = screenshotBase64;
          lastElementCountRef.current = elements.length;
        }
      } catch (err) {
        if (!cancelled) {
          const errorMessage = err instanceof Error ? err.message : "Failed to process thumbnails";
          setError(errorMessage);
          console.error("[useElementThumbnails] Processing failed:", err);
        }
      } finally {
        if (!cancelled) {
          setIsProcessing(false);
        }
      }
    };

    processThumbnails();

    return () => {
      cancelled = true;
    };
  }, [screenshotBase64, elements, maxSize]);

  return { thumbnails, isProcessing, error };
}

export default useElementThumbnails;
