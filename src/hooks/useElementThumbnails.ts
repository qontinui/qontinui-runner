/**
 * useElementThumbnails Hook
 *
 * Manages thumbnail generation for UI Bridge elements by cropping
 * from a full page screenshot.
 *
 * Features:
 * - Caches thumbnails by element ID with bounds-based invalidation
 * - Automatically invalidates cache when screenshot changes
 * - Only re-crops elements whose bounds have changed or are new
 *
 * Supports two modes:
 * - Eager: Process all thumbnails upfront
 * - Lazy: Provide a shared cache for on-demand thumbnail loading
 */

import { useState, useEffect, useRef, useMemo, useCallback } from "react";
import { cropThumbnails, cropThumbnail, type ElementBounds } from "../lib/thumbnail-cropper";
import { ThumbnailCache, hashScreenshot } from "../lib/thumbnail-cache";

export interface ElementWithBounds {
  id: string;
  bounds: ElementBounds;
}

export interface CacheStats {
  /** Number of thumbnails served from cache */
  cacheHits: number;
  /** Number of thumbnails that needed to be generated */
  cacheMisses: number;
  /** Total cache size */
  cacheSize: number;
}

export interface ThumbnailProgress {
  /** Number of elements processed so far */
  processed: number;
  /** Total number of elements to process */
  total: number;
  /** Progress as a percentage (0-100) */
  percentage: number;
}

export interface UseElementThumbnailsReturn {
  /** Map of element ID to base64 thumbnail data */
  thumbnails: Map<string, string>;
  /** Whether thumbnails are currently being processed (eager mode only) */
  isProcessing: boolean;
  /** Error message if processing failed */
  error: string | null;
  /** Shared cache reference for lazy loading components */
  thumbnailCache: Map<string, string>;
  /** Request a single thumbnail to be loaded (lazy mode) */
  requestThumbnail: (elementId: string, bounds: ElementBounds) => Promise<string | null>;
  /** Cache statistics for debugging/monitoring */
  cacheStats: CacheStats;
  /** Progress of thumbnail generation (only populated during processing) */
  progress: ThumbnailProgress | null;
}

export interface UseElementThumbnailsOptions {
  /** Maximum thumbnail dimension (default: 40) */
  maxSize?: number;
  /** Loading mode: "eager" processes all upfront, "lazy" loads on-demand (default: "lazy") */
  mode?: "eager" | "lazy";
  /**
   * Skip elements larger than this fraction of the image (0-1).
   * Default: 0.9 (90%). Set to 1.0 to disable skipping.
   * Large containers (like body, main wrappers) typically don't provide
   * meaningful thumbnails.
   */
  skipLargeThreshold?: number;
}

/**
 * Hook to generate thumbnails for elements from a page screenshot.
 *
 * @param elements - Array of elements with id and bounds
 * @param screenshotBase64 - Base64 encoded screenshot (without data URL prefix)
 * @param options - Configuration options
 * @returns Thumbnails map, processing state, error, and lazy loading utilities
 */
export function useElementThumbnails(
  elements: ElementWithBounds[],
  screenshotBase64: string | null,
  options: UseElementThumbnailsOptions | number = {},
): UseElementThumbnailsReturn {
  // Support legacy signature: (elements, screenshot, maxSize)
  const normalizedOptions: UseElementThumbnailsOptions =
    typeof options === "number" ? { maxSize: options } : options;

  const { maxSize = 40, mode = "lazy", skipLargeThreshold = 0.9 } = normalizedOptions;

  const [thumbnails, setThumbnails] = useState<Map<string, string>>(new Map());
  const [isProcessing, setIsProcessing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [cacheStats, setCacheStats] = useState<CacheStats>({
    cacheHits: 0,
    cacheMisses: 0,
    cacheSize: 0,
  });
  const [progress, setProgress] = useState<ThumbnailProgress | null>(null);

  // Persistent cache instance that tracks both element ID and bounds
  const cacheRef = useRef<ThumbnailCache | null>(null);

  if (!cacheRef.current) {
    cacheRef.current = new ThumbnailCache();
  }

  // Track in-flight processing to handle concurrent updates
  const processingIdRef = useRef(0);

  // Memoize the screenshot hash to avoid recalculating on every render
  const screenshotHash = useMemo(() => {
    return screenshotBase64 ? hashScreenshot(screenshotBase64) : "";
  }, [screenshotBase64]);

  // Build element bounds map for quick lookup
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  const elementBoundsMap = useMemo(() => {
    const map = new Map<string, ElementBounds>();
    for (const el of elements) {
      map.set(el.id, el.bounds);
    }
    return map;
  }, [elements]);

  // Request a single thumbnail (for lazy loading)
  const requestThumbnail = useCallback(
    async (elementId: string, bounds: ElementBounds): Promise<string | null> => {
      const cache = cacheRef.current!;

      if (!screenshotBase64) {
        return null;
      }

      // Validate cache against current screenshot (may invalidate if screenshot changed)
      cache.validateOrInvalidate(screenshotHash, maxSize);

      // Check cache first (includes bounds-based invalidation)
      const cached = cache.get(elementId, bounds);
      if (cached) {
        return cached;
      }

      try {
        const result = await cropThumbnail(screenshotBase64, bounds, {
          maxSize,
          skipLargeThreshold,
        });
        if (result) {
          // Store in cache with bounds for future invalidation
          cache.set(elementId, bounds, result);
          // Update state to trigger re-renders for components using thumbnails
          setThumbnails((prev) => {
            const next = new Map(prev);
            next.set(elementId, result);
            return next;
          });
          setCacheStats({
            cacheHits: 0,
            cacheMisses: 1,
            cacheSize: cache.size,
          });
        }
        return result;
      } catch (err) {
        console.error(`[useElementThumbnails] Failed to crop thumbnail for ${elementId}:`, err);
        return null;
      }
    },
    [screenshotBase64, screenshotHash, maxSize, skipLargeThreshold],
  );

  // Eager mode: Process all thumbnails upfront with caching
  useEffect(() => {
    if (mode !== "eager") {
      return;
    }

    const cache = cacheRef.current!;

    // Clear thumbnails if no screenshot or elements
    if (!screenshotBase64 || elements.length === 0) {
      setThumbnails(new Map());
      setError(null);
      setProgress(null);
      setCacheStats({ cacheHits: 0, cacheMisses: 0, cacheSize: 0 });
      cache.clear();
      return;
    }

    // Validate cache against current screenshot (invalidates if screenshot changed)
    cache.validateOrInvalidate(screenshotHash, maxSize);

    // Build set of current element IDs for pruning
    const currentElementIds = new Set(elements.map((e) => e.id));

    // Prune stale entries (elements that no longer exist)
    cache.prune(currentElementIds);

    // Determine which elements need processing
    const elementsToProcess: ElementWithBounds[] = [];
    let cacheHits = 0;

    for (const element of elements) {
      if (cache.has(element.id, element.bounds)) {
        cacheHits++;
      } else {
        elementsToProcess.push(element);
      }
    }

    // If all elements are cached, just return the cached thumbnails
    if (elementsToProcess.length === 0) {
      const cachedThumbnails = cache.getAllThumbnails();
      setThumbnails(cachedThumbnails);
      setCacheStats({
        cacheHits,
        cacheMisses: 0,
        cacheSize: cache.size,
      });
      setError(null);
      setProgress(null);
      return;
    }

    // Process only the elements that need cropping
    const processingId = ++processingIdRef.current;

    const processThumbnails = async () => {
      setIsProcessing(true);
      setError(null);

      const total = elementsToProcess.length;
      setProgress({ processed: 0, total, percentage: 0 });

      try {
        // Calculate progress update interval to avoid excessive re-renders
        // Update approximately 20 times during processing, minimum every element
        const progressInterval = Math.max(1, Math.floor(total / 20));

        const newThumbnails = await cropThumbnails(screenshotBase64, elementsToProcess, {
          maxSize,
          skipLargeThreshold,
          onProgress: (processed, totalCount) => {
            // Only update if this processing is still current
            if (processingId === processingIdRef.current) {
              setProgress({
                processed,
                total: totalCount,
                percentage: Math.round((processed / totalCount) * 100),
              });
            }
          },
          progressInterval,
        });

        // Check if this processing is still current
        if (processingId !== processingIdRef.current) {
          return;
        }

        // Add new thumbnails to cache
        for (const [id, data] of newThumbnails) {
          const element = elementsToProcess.find((e) => e.id === id);
          if (element) {
            cache.set(id, element.bounds, data);
          }
        }

        // Get all thumbnails (cached + newly generated)
        const allThumbnails = cache.getAllThumbnails();
        setThumbnails(allThumbnails);
        setCacheStats({
          cacheHits,
          cacheMisses: elementsToProcess.length,
          cacheSize: cache.size,
        });
      } catch (err) {
        if (processingId !== processingIdRef.current) {
          return;
        }
        const errorMessage = err instanceof Error ? err.message : "Failed to process thumbnails";
        setError(errorMessage);
        console.error("[useElementThumbnails] Processing failed:", err);
      } finally {
        if (processingId === processingIdRef.current) {
          setIsProcessing(false);
          setProgress(null);
        }
      }
    };

    processThumbnails();

    // Cleanup function to invalidate in-flight processing
    // The processingId increment invalidates any in-progress work, which is intentional
    return () => {
      // eslint-disable-next-line react-hooks/exhaustive-deps -- Intentionally incrementing to invalidate in-flight work
      processingIdRef.current++;
    };
  }, [screenshotBase64, screenshotHash, elements, maxSize, mode, skipLargeThreshold]);

  // Get the current cache as a simple Map for backwards compatibility
  // Note: We intentionally trigger recomputation based on thumbnails state changes
  // even though we read from cacheRef - this ensures the returned Map is fresh
  const thumbnailCache = useMemo(() => {
    return cacheRef.current?.getAllThumbnails() ?? new Map<string, string>();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- thumbnails triggers cache refresh
  }, [thumbnails]);

  return {
    thumbnails,
    isProcessing,
    error,
    thumbnailCache,
    requestThumbnail,
    cacheStats,
    progress,
  };
}

export default useElementThumbnails;
