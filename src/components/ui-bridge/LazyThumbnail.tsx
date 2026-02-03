/**
 * LazyThumbnail Component
 *
 * Renders a thumbnail that only loads when visible in the viewport.
 * Uses IntersectionObserver for efficient visibility detection.
 * Shows a placeholder for cross-origin iframe content where screenshots cannot be captured.
 */

import { useState, useEffect, useRef, useCallback } from "react";
import { Globe } from "lucide-react";
import { cn } from "../../lib/utils";
import { cropThumbnail, type ElementBounds } from "../../lib/thumbnail-cropper";

interface LazyThumbnailProps {
  /** Unique element ID for caching */
  elementId: string;
  /** Element bounds for cropping */
  bounds: ElementBounds;
  /** Base64 screenshot data (without data URL prefix) */
  screenshotBase64: string | null;
  /** Maximum thumbnail dimension */
  maxSize?: number;
  /** CSS class name */
  className?: string;
  /** Fallback content when no thumbnail available */
  fallback?: React.ReactNode;
  /** Shared thumbnail cache to avoid re-cropping */
  thumbnailCache?: Map<string, string>;
  /** Callback when thumbnail is loaded */
  onLoad?: (elementId: string, thumbnail: string) => void;
  /** If true, element is in a cross-origin iframe and thumbnail cannot be captured */
  isCrossOrigin?: boolean;
}

// Shared IntersectionObserver for all LazyThumbnail instances
let sharedObserver: IntersectionObserver | null = null;
const observerCallbacks = new Map<Element, () => void>();

function getSharedObserver(): IntersectionObserver {
  if (!sharedObserver) {
    sharedObserver = new IntersectionObserver(
      (entries) => {
        entries.forEach((entry) => {
          if (entry.isIntersecting) {
            const callback = observerCallbacks.get(entry.target);
            if (callback) {
              callback();
              // Unobserve after triggering - thumbnail loads once
              sharedObserver?.unobserve(entry.target);
              observerCallbacks.delete(entry.target);
            }
          }
        });
      },
      {
        rootMargin: "100px", // Start loading slightly before visible
        threshold: 0,
      },
    );
  }
  return sharedObserver;
}

export function LazyThumbnail({
  elementId,
  bounds,
  screenshotBase64,
  maxSize = 40,
  className,
  fallback,
  thumbnailCache,
  onLoad,
  isCrossOrigin = false,
}: LazyThumbnailProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [thumbnail, setThumbnail] = useState<string | null>(() => {
    // Check cache on initial render
    return thumbnailCache?.get(elementId) ?? null;
  });
  const [isLoading, setIsLoading] = useState(false);
  const [hasError, setHasError] = useState(false);

  // Crop the thumbnail
  const loadThumbnail = useCallback(async () => {
    // Skip if already loaded, loading, or no screenshot
    if (thumbnail || isLoading || !screenshotBase64) {
      return;
    }

    // Check cache again (might have been populated by another instance)
    const cached = thumbnailCache?.get(elementId);
    if (cached) {
      setThumbnail(cached);
      return;
    }

    setIsLoading(true);
    setHasError(false);

    try {
      const result = await cropThumbnail(screenshotBase64, bounds, { maxSize });
      if (result) {
        setThumbnail(result);
        // Store in cache if provided
        if (thumbnailCache) {
          thumbnailCache.set(elementId, result);
        }
        // Notify parent
        onLoad?.(elementId, result);
      } else {
        setHasError(true);
      }
    } catch (err) {
      console.error(`[LazyThumbnail] Failed to crop thumbnail for ${elementId}:`, err);
      setHasError(true);
    } finally {
      setIsLoading(false);
    }
  }, [elementId, bounds, screenshotBase64, maxSize, thumbnail, isLoading, thumbnailCache, onLoad]);

  // Set up IntersectionObserver
  useEffect(() => {
    const container = containerRef.current;
    if (!container || thumbnail || !screenshotBase64) {
      return;
    }

    const observer = getSharedObserver();

    // Register callback
    observerCallbacks.set(container, loadThumbnail);
    observer.observe(container);

    return () => {
      observer.unobserve(container);
      observerCallbacks.delete(container);
    };
  }, [loadThumbnail, thumbnail, screenshotBase64]);

  // Update thumbnail if cache changes externally
  useEffect(() => {
    if (thumbnailCache && !thumbnail) {
      const cached = thumbnailCache.get(elementId);
      if (cached) {
        setThumbnail(cached);
      }
    }
  }, [elementId, thumbnailCache, thumbnail]);

  // Cross-origin iframe placeholder - cannot capture thumbnail
  if (isCrossOrigin) {
    return (
      <div
        className={cn(
          "flex items-center justify-center border border-amber-500/30 rounded bg-amber-500/10 flex-shrink-0",
          className,
        )}
        title="Thumbnail not available - cross-origin iframe content"
      >
        <Globe className="w-3 h-3 text-amber-600" />
      </div>
    );
  }

  // Render thumbnail or placeholder
  if (thumbnail) {
    return (
      <img
        src={`data:image/png;base64,${thumbnail}`}
        alt=""
        className={cn(
          "object-contain border border-border/50 rounded bg-white/50 flex-shrink-0",
          className,
        )}
      />
    );
  }

  // Loading or placeholder state
  return (
    <div
      ref={containerRef}
      className={cn(
        "flex items-center justify-center border border-border/30 rounded bg-muted/20 flex-shrink-0",
        className,
      )}
    >
      {isLoading ? (
        <div className="w-2 h-2 rounded-full bg-muted-foreground/40 animate-pulse" />
      ) : hasError ? (
        fallback
      ) : (
        // Placeholder - will trigger load when visible
        fallback
      )}
    </div>
  );
}

export default LazyThumbnail;
