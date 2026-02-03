/**
 * Thumbnail Cache
 *
 * Caches generated thumbnails to avoid re-cropping on re-render.
 * The cache is automatically invalidated when the screenshot changes.
 */

import type { ElementBounds } from "./thumbnail-cropper";

export interface CachedThumbnail {
  /** The base64 thumbnail data */
  data: string;
  /** Hash of the bounds used to generate this thumbnail */
  boundsHash: string;
}

/**
 * Generate a hash string for element bounds.
 * Used to detect when an element's bounds have changed.
 */
export function hashBounds(bounds: ElementBounds): string {
  return `${bounds.x}:${bounds.y}:${bounds.width}:${bounds.height}`;
}

/**
 * Generate a simple hash for a screenshot.
 * Uses a fast hash of a sample of the base64 data to detect changes.
 *
 * We don't hash the entire screenshot (too slow for large images),
 * instead we sample characters at regular intervals.
 */
export function hashScreenshot(base64: string): string {
  if (!base64) return "";

  // Sample ~100 characters from the string at regular intervals
  const sampleSize = 100;
  const step = Math.max(1, Math.floor(base64.length / sampleSize));

  let hash = base64.length.toString(36); // Include length for uniqueness
  for (let i = 0; i < base64.length; i += step) {
    hash += base64.charCodeAt(i).toString(36);
  }

  return hash;
}

/**
 * Thumbnail cache class.
 *
 * Stores thumbnails keyed by element ID, with bounds hash for change detection.
 * The entire cache is invalidated when the screenshot hash changes.
 */
export class ThumbnailCache {
  private cache: Map<string, CachedThumbnail> = new Map();
  private screenshotHash: string = "";
  private maxSize: number = 0;

  /**
   * Check if the cache is valid for the given screenshot and maxSize.
   * If invalid, clears the cache and updates the stored hash.
   *
   * @param screenshotHash - Hash of the current screenshot
   * @param maxSize - Current max thumbnail size
   * @returns true if cache was valid, false if it was invalidated
   */
  validateOrInvalidate(screenshotHash: string, maxSize: number): boolean {
    if (this.screenshotHash === screenshotHash && this.maxSize === maxSize) {
      return true;
    }

    // Screenshot or maxSize changed, invalidate cache
    this.cache.clear();
    this.screenshotHash = screenshotHash;
    this.maxSize = maxSize;
    return false;
  }

  /**
   * Get a cached thumbnail if it exists and bounds haven't changed.
   *
   * @param elementId - The element ID
   * @param bounds - Current element bounds
   * @returns The cached thumbnail data, or null if not cached or bounds changed
   */
  get(elementId: string, bounds: ElementBounds): string | null {
    const cached = this.cache.get(elementId);
    if (!cached) return null;

    const currentHash = hashBounds(bounds);
    if (cached.boundsHash !== currentHash) {
      // Bounds changed, remove stale entry
      this.cache.delete(elementId);
      return null;
    }

    return cached.data;
  }

  /**
   * Store a thumbnail in the cache.
   *
   * @param elementId - The element ID
   * @param bounds - The element bounds used to generate the thumbnail
   * @param data - The base64 thumbnail data
   */
  set(elementId: string, bounds: ElementBounds, data: string): void {
    this.cache.set(elementId, {
      data,
      boundsHash: hashBounds(bounds),
    });
  }

  /**
   * Check if an element has a valid cached thumbnail.
   *
   * @param elementId - The element ID
   * @param bounds - Current element bounds
   * @returns true if there's a valid cached thumbnail
   */
  has(elementId: string, bounds: ElementBounds): boolean {
    return this.get(elementId, bounds) !== null;
  }

  /**
   * Remove elements that are no longer in the element list.
   * This prevents memory leaks from stale entries.
   *
   * @param currentElementIds - Set of current element IDs
   */
  prune(currentElementIds: Set<string>): void {
    for (const id of this.cache.keys()) {
      if (!currentElementIds.has(id)) {
        this.cache.delete(id);
      }
    }
  }

  /**
   * Get the number of cached thumbnails.
   */
  get size(): number {
    return this.cache.size;
  }

  /**
   * Clear the entire cache.
   */
  clear(): void {
    this.cache.clear();
    this.screenshotHash = "";
    this.maxSize = 0;
  }

  /**
   * Get all cached thumbnails as a Map.
   * Returns a new Map to prevent external mutation.
   */
  getAllThumbnails(): Map<string, string> {
    const result = new Map<string, string>();
    for (const [id, cached] of this.cache) {
      result.set(id, cached.data);
    }
    return result;
  }
}

/**
 * Create a new thumbnail cache instance.
 */
export function createThumbnailCache(): ThumbnailCache {
  return new ThumbnailCache();
}
