/**
 * Thumbnail Cropper Utility
 *
 * Crops thumbnails from a full page screenshot given element bounds.
 * Uses an offscreen canvas for efficient client-side cropping.
 *
 * For elements outside the viewport, use `isElementOffScreen()` to detect
 * and then use scroll-based capture via `captureElementScreenshot()` hook.
 */

export interface ElementBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * Viewport dimensions for off-screen detection
 */
export interface ViewportDimensions {
  width: number;
  height: number;
}

export interface CropOptions {
  /** Maximum dimension for the thumbnail (default: 48) */
  maxSize?: number;
  /** Image format for output (default: "png") */
  format?: "png" | "jpeg" | "webp";
  /** Quality for JPEG/WebP (0-1, default: 0.8) */
  quality?: number;
  /**
   * Skip elements larger than this fraction of the image (0-1).
   * Default: 0.9 (90%). Set to 1.0 to disable skipping.
   * Large containers (like body, main wrappers) typically don't provide
   * meaningful thumbnails.
   */
  skipLargeThreshold?: number;
}

export interface BatchCropOptions extends CropOptions {
  /** Progress callback called during batch processing */
  onProgress?: (processed: number, total: number) => void;
  /** Interval for progress updates to avoid excessive callbacks (default: 10) */
  progressInterval?: number;
}

/**
 * Result of batch thumbnail cropping with metadata about skipped elements.
 * Use this to track which elements couldn't be processed and why.
 */
export interface BatchCropResult {
  /** Map of element ID to base64 thumbnail data */
  thumbnails: Map<string, string>;
  /** Elements that were skipped due to cross-origin restrictions */
  crossOriginSkipped: string[];
  /** Elements that were skipped due to other reasons (invalid bounds, too large, etc.) */
  otherSkipped: string[];
}

/**
 * Element info for batch cropping, including cross-origin flag
 */
export interface CropElement {
  id: string;
  bounds: ElementBounds;
  /** If true, element is in a cross-origin iframe and thumbnail cannot be captured */
  isCrossOrigin?: boolean;
}

/**
 * Load an image from base64 data
 */
async function loadImage(base64: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error("Failed to load image"));
    img.src = `data:image/png;base64,${base64}`;
  });
}

/**
 * Crops a thumbnail from a full page screenshot given element bounds.
 *
 * @param screenshotBase64 - Base64 encoded PNG screenshot (without data URL prefix)
 * @param bounds - Element bounds in viewport coordinates
 * @param options - Crop options
 * @returns Base64 encoded thumbnail (without data URL prefix), or null if crop fails
 */
export async function cropThumbnail(
  screenshotBase64: string,
  bounds: ElementBounds,
  options: CropOptions = {},
): Promise<string | null> {
  const { maxSize = 48, format = "png", quality = 0.8, skipLargeThreshold = 0.9 } = options;

  try {
    const img = await loadImage(screenshotBase64);

    // Calculate crop region (clamp to image bounds)
    const cropX = Math.max(0, Math.floor(bounds.x));
    const cropY = Math.max(0, Math.floor(bounds.y));
    const cropWidth = Math.min(Math.ceil(bounds.width), img.width - cropX);
    const cropHeight = Math.min(Math.ceil(bounds.height), img.height - cropY);

    // Skip if crop region is too small or invalid
    if (cropWidth <= 0 || cropHeight <= 0) {
      return null;
    }

    // Skip very large elements (probably full-page containers)
    // Set skipLargeThreshold to 1.0 to disable this behavior
    if (
      skipLargeThreshold < 1.0 &&
      cropWidth > img.width * skipLargeThreshold &&
      cropHeight > img.height * skipLargeThreshold
    ) {
      return null;
    }

    // Calculate thumbnail size (maintain aspect ratio)
    const scale = Math.min(1, maxSize / Math.max(cropWidth, cropHeight));
    const thumbWidth = Math.max(1, Math.ceil(cropWidth * scale));
    const thumbHeight = Math.max(1, Math.ceil(cropHeight * scale));

    // Create canvas and draw cropped/scaled thumbnail
    const canvas = document.createElement("canvas");
    canvas.width = thumbWidth;
    canvas.height = thumbHeight;
    const ctx = canvas.getContext("2d");
    if (!ctx) {
      return null;
    }

    // Use high-quality image scaling
    ctx.imageSmoothingEnabled = true;
    ctx.imageSmoothingQuality = "high";

    ctx.drawImage(img, cropX, cropY, cropWidth, cropHeight, 0, 0, thumbWidth, thumbHeight);

    // Return as base64 (strip data URL prefix)
    const mimeType = `image/${format}`;
    const dataUrl =
      format === "png" ? canvas.toDataURL(mimeType) : canvas.toDataURL(mimeType, quality);
    return dataUrl.split(",")[1];
  } catch (error) {
    console.error("[thumbnail-cropper] Failed to crop thumbnail:", error);
    return null;
  }
}

/**
 * Crop a single element from a pre-loaded image.
 * Used internally by both cropThumbnails and cropThumbnailsIncremental.
 *
 * @param img - Pre-loaded HTMLImageElement
 * @param bounds - Element bounds
 * @param options - Crop options
 * @returns Base64 thumbnail data or null if skipped
 */
function cropFromImage(
  img: HTMLImageElement,
  bounds: ElementBounds,
  options: {
    maxSize: number;
    format: "png" | "jpeg" | "webp";
    quality: number;
    skipLargeThreshold: number;
  },
): string | null {
  const { maxSize, format, quality, skipLargeThreshold } = options;

  // Calculate crop region (clamp to image bounds)
  const cropX = Math.max(0, Math.floor(bounds.x));
  const cropY = Math.max(0, Math.floor(bounds.y));
  const cropWidth = Math.min(Math.ceil(bounds.width), img.width - cropX);
  const cropHeight = Math.min(Math.ceil(bounds.height), img.height - cropY);

  // Skip if crop region is too small or invalid
  if (cropWidth <= 2 || cropHeight <= 2) {
    return null;
  }

  // Skip very large elements (probably full-page containers)
  // Set skipLargeThreshold to 1.0 to disable this behavior
  if (
    skipLargeThreshold < 1.0 &&
    cropWidth > img.width * skipLargeThreshold &&
    cropHeight > img.height * skipLargeThreshold
  ) {
    return null;
  }

  // Calculate thumbnail size (maintain aspect ratio)
  const scale = Math.min(1, maxSize / Math.max(cropWidth, cropHeight));
  const thumbWidth = Math.max(1, Math.ceil(cropWidth * scale));
  const thumbHeight = Math.max(1, Math.ceil(cropHeight * scale));

  // Create canvas and draw cropped/scaled thumbnail
  const canvas = document.createElement("canvas");
  canvas.width = thumbWidth;
  canvas.height = thumbHeight;
  const ctx = canvas.getContext("2d");
  if (!ctx) {
    return null;
  }

  // Use high-quality image scaling
  ctx.imageSmoothingEnabled = true;
  ctx.imageSmoothingQuality = "high";

  ctx.drawImage(img, cropX, cropY, cropWidth, cropHeight, 0, 0, thumbWidth, thumbHeight);

  // Return as base64
  const mimeType = `image/${format}`;
  const dataUrl =
    format === "png" ? canvas.toDataURL(mimeType) : canvas.toDataURL(mimeType, quality);
  return dataUrl.split(",")[1];
}

/**
 * Batch crop thumbnails for multiple elements.
 * More efficient than calling cropThumbnail multiple times as it loads the image once.
 *
 * @param screenshotBase64 - Base64 encoded PNG screenshot (without data URL prefix)
 * @param elements - Array of elements with id and bounds
 * @param options - Crop options with optional progress callback
 * @returns Map of element ID to base64 thumbnail
 */
export async function cropThumbnails(
  screenshotBase64: string,
  elements: Array<{ id: string; bounds: ElementBounds }>,
  options: BatchCropOptions = {},
): Promise<Map<string, string>> {
  const {
    maxSize = 48,
    format = "png",
    quality = 0.8,
    skipLargeThreshold = 0.9,
    onProgress,
    progressInterval = 10,
  } = options;
  const thumbnails = new Map<string, string>();

  if (!screenshotBase64 || elements.length === 0) {
    return thumbnails;
  }

  try {
    // Load image once
    const img = await loadImage(screenshotBase64);
    const total = elements.length;

    // Process all elements
    for (let i = 0; i < elements.length; i++) {
      const element = elements[i];
      const result = cropFromImage(img, element.bounds, {
        maxSize,
        format,
        quality,
        skipLargeThreshold,
      });
      if (result) {
        thumbnails.set(element.id, result);
      }

      // Report progress at intervals to avoid excessive callbacks
      if (onProgress && (i + 1) % progressInterval === 0) {
        onProgress(i + 1, total);
      }
    }

    // Final progress update
    if (onProgress && elements.length > 0) {
      onProgress(elements.length, total);
    }

    return thumbnails;
  } catch (error) {
    console.error("[thumbnail-cropper] Failed to batch crop thumbnails:", error);
    return thumbnails;
  }
}

/**
 * Incrementally crop thumbnails only for elements that need processing.
 * This is used by the caching layer to only crop new or changed elements.
 *
 * @param screenshotBase64 - Base64 encoded PNG screenshot (without data URL prefix)
 * @param elementsToProcess - Array of elements that need cropping
 * @param options - Crop options
 * @returns Map of element ID to base64 thumbnail for processed elements
 */
export async function cropThumbnailsIncremental(
  screenshotBase64: string,
  elementsToProcess: Array<{ id: string; bounds: ElementBounds }>,
  options: CropOptions = {},
): Promise<Map<string, string>> {
  // Uses the same logic as cropThumbnails, just with a subset of elements
  return cropThumbnails(screenshotBase64, elementsToProcess, options);
}

/**
 * Batch crop thumbnails with detailed metadata about which elements were skipped.
 * This version handles cross-origin elements by skipping them and reporting them separately.
 *
 * @param screenshotBase64 - Base64 encoded PNG screenshot (without data URL prefix)
 * @param elements - Array of elements with id, bounds, and optional isCrossOrigin flag
 * @param options - Crop options with optional progress callback
 * @returns BatchCropResult with thumbnails and lists of skipped elements
 */
export async function cropThumbnailsWithMetadata(
  screenshotBase64: string,
  elements: CropElement[],
  options: BatchCropOptions = {},
): Promise<BatchCropResult> {
  const {
    maxSize = 48,
    format = "png",
    quality = 0.8,
    skipLargeThreshold = 0.9,
    onProgress,
    progressInterval = 10,
  } = options;

  const result: BatchCropResult = {
    thumbnails: new Map<string, string>(),
    crossOriginSkipped: [],
    otherSkipped: [],
  };

  if (!screenshotBase64 || elements.length === 0) {
    return result;
  }

  try {
    // Load image once
    const img = await loadImage(screenshotBase64);
    const total = elements.length;

    // Process all elements
    for (let i = 0; i < elements.length; i++) {
      const element = elements[i];

      // Skip cross-origin elements
      if (element.isCrossOrigin) {
        result.crossOriginSkipped.push(element.id);
        // Report progress even for skipped elements
        if (onProgress && (i + 1) % progressInterval === 0) {
          onProgress(i + 1, total);
        }
        continue;
      }

      const thumbnail = cropFromImage(img, element.bounds, {
        maxSize,
        format,
        quality,
        skipLargeThreshold,
      });

      if (thumbnail) {
        result.thumbnails.set(element.id, thumbnail);
      } else {
        result.otherSkipped.push(element.id);
      }

      // Report progress at intervals to avoid excessive callbacks
      if (onProgress && (i + 1) % progressInterval === 0) {
        onProgress(i + 1, total);
      }
    }

    // Final progress update
    if (onProgress && elements.length > 0) {
      onProgress(elements.length, total);
    }

    return result;
  } catch (error) {
    console.error("[thumbnail-cropper] Failed to batch crop thumbnails with metadata:", error);
    return result;
  }
}

/**
 * Get a larger preview crop for a single element (for detail view)
 *
 * @param screenshotBase64 - Base64 encoded PNG screenshot
 * @param bounds - Element bounds
 * @param maxSize - Maximum dimension (default: 200)
 * @returns Base64 encoded preview image
 */
export async function cropPreview(
  screenshotBase64: string,
  bounds: ElementBounds,
  maxSize: number = 200,
): Promise<string | null> {
  return cropThumbnail(screenshotBase64, bounds, { maxSize, format: "png" });
}

// =============================================================================
// Off-Screen Detection Utilities
// =============================================================================

/**
 * Check if an element is completely or mostly outside the viewport.
 * Use this to determine if scroll-based capture is needed.
 *
 * An element is considered "off-screen" if:
 * - Any part of it is completely outside the viewport boundaries, OR
 * - Less than a threshold percentage of it is visible
 *
 * @param bounds - Element bounds in viewport coordinates
 * @param viewport - Viewport dimensions
 * @param visibilityThreshold - Minimum visible percentage (0-1, default: 0.1 = 10%)
 * @returns true if element needs scroll-based capture
 */
export function isElementOffScreen(
  bounds: ElementBounds,
  viewport: ViewportDimensions,
  visibilityThreshold: number = 0.1,
): boolean {
  // Check if element is completely outside viewport
  const isCompletelyAbove = bounds.y + bounds.height <= 0;
  const isCompletelyBelow = bounds.y >= viewport.height;
  const isCompletelyLeft = bounds.x + bounds.width <= 0;
  const isCompletelyRight = bounds.x >= viewport.width;

  if (isCompletelyAbove || isCompletelyBelow || isCompletelyLeft || isCompletelyRight) {
    return true;
  }

  // Calculate visible area
  const visibleLeft = Math.max(0, bounds.x);
  const visibleTop = Math.max(0, bounds.y);
  const visibleRight = Math.min(viewport.width, bounds.x + bounds.width);
  const visibleBottom = Math.min(viewport.height, bounds.y + bounds.height);

  const visibleWidth = Math.max(0, visibleRight - visibleLeft);
  const visibleHeight = Math.max(0, visibleBottom - visibleTop);
  const visibleArea = visibleWidth * visibleHeight;
  const totalArea = bounds.width * bounds.height;

  // If visible area is less than threshold, consider it off-screen
  if (totalArea > 0) {
    const visibleRatio = visibleArea / totalArea;
    return visibleRatio < visibilityThreshold;
  }

  return false;
}

/**
 * Result of analyzing elements for off-screen status
 */
export interface OffScreenAnalysis {
  /** Elements that are fully or mostly visible in viewport */
  inViewport: Array<{ id: string; bounds: ElementBounds }>;
  /** Elements that need scroll-based capture */
  offScreen: Array<{ id: string; bounds: ElementBounds }>;
}

/**
 * Analyze a list of elements and categorize them as in-viewport or off-screen.
 * Use this to batch process thumbnails efficiently:
 * 1. Crop in-viewport elements from current screenshot
 * 2. Use scroll-based capture for off-screen elements (one at a time or batched by region)
 *
 * @param elements - Array of elements with id and bounds
 * @param viewport - Viewport dimensions
 * @param visibilityThreshold - Minimum visible percentage (0-1, default: 0.1)
 * @returns Analysis result with elements categorized
 */
export function analyzeOffScreenElements(
  elements: Array<{ id: string; bounds: ElementBounds }>,
  viewport: ViewportDimensions,
  visibilityThreshold: number = 0.1,
): OffScreenAnalysis {
  const inViewport: Array<{ id: string; bounds: ElementBounds }> = [];
  const offScreen: Array<{ id: string; bounds: ElementBounds }> = [];

  for (const element of elements) {
    if (isElementOffScreen(element.bounds, viewport, visibilityThreshold)) {
      offScreen.push(element);
    } else {
      inViewport.push(element);
    }
  }

  return { inViewport, offScreen };
}

// =============================================================================
// Full-Page Screenshot Stitching Utilities
// =============================================================================

/**
 * A single tile from a full-page screenshot capture
 */
export interface ScreenshotTile {
  /** Base64 encoded PNG data (without data URL prefix) */
  screenshot: string;
  /** X position of this tile on the full page (in pixels) */
  x: number;
  /** Y position of this tile on the full page (in pixels) */
  y: number;
  /** Width of this tile (in pixels) */
  width: number;
  /** Height of this tile (in pixels) */
  height: number;
  /** Row index in the tile grid (optional, for debugging) */
  row?: number;
  /** Column index in the tile grid (optional, for debugging) */
  col?: number;
}

/**
 * Options for stitching screenshots together
 */
export interface StitchOptions {
  /** Image format for output (default: "png") */
  format?: "png" | "jpeg" | "webp";
  /** Quality for JPEG/WebP (0-1, default: 0.9) */
  quality?: number;
  /** If true, return null on any tile load failure instead of skipping (default: false) */
  failOnError?: boolean;
  /** Progress callback called after each tile is processed */
  onProgress?: (processed: number, total: number) => void;
}

/**
 * Result of stitching screenshots together
 */
export interface StitchResult {
  /** Base64 encoded full-page image (without data URL prefix) */
  data: string;
  /** Total width of the stitched image */
  width: number;
  /** Total height of the stitched image */
  height: number;
  /** Number of tiles successfully processed */
  tilesProcessed: number;
  /** Number of tiles that failed to load */
  tilesFailed: number;
}

/**
 * Stitch multiple screenshot tiles together into a single full-page image.
 *
 * This function takes an array of screenshot tiles captured at different scroll
 * positions and combines them into a single large canvas representing the full page.
 *
 * The function handles:
 * - Overlapping regions: Uses the first tile that covers a region (tiles should be non-overlapping)
 * - Different tile sizes: Each tile can have different dimensions (common for edge tiles)
 * - Failed tile loads: Skips tiles that fail to load (or fails entirely if failOnError is true)
 * - Large images: Uses standard canvas operations, may hit browser memory limits for very large pages
 *
 * @param tiles - Array of screenshot tiles with position and size information
 * @param totalWidth - Total width of the full page in pixels
 * @param totalHeight - Total height of the full page in pixels
 * @param options - Stitching options
 * @returns StitchResult with the stitched image, or null if stitching fails
 *
 * @example
 * ```typescript
 * const result = await stitchScreenshots(tiles, 1920, 5000);
 * if (result) {
 *   console.log(`Stitched ${result.tilesProcessed} tiles into ${result.width}x${result.height} image`);
 *   const fullPageImage = result.data; // Base64 PNG
 * }
 * ```
 */
export async function stitchScreenshots(
  tiles: ScreenshotTile[],
  totalWidth: number,
  totalHeight: number,
  options: StitchOptions = {},
): Promise<StitchResult | null> {
  const { format = "png", quality = 0.9, failOnError = false, onProgress } = options;

  if (!tiles || tiles.length === 0) {
    console.warn("[thumbnail-cropper] No tiles provided for stitching");
    return null;
  }

  if (totalWidth <= 0 || totalHeight <= 0) {
    console.warn("[thumbnail-cropper] Invalid dimensions for stitching:", totalWidth, totalHeight);
    return null;
  }

  try {
    // Create the full-page canvas
    const canvas = document.createElement("canvas");
    canvas.width = totalWidth;
    canvas.height = totalHeight;
    const ctx = canvas.getContext("2d");

    if (!ctx) {
      console.error("[thumbnail-cropper] Failed to get canvas 2D context");
      return null;
    }

    // Fill with white background (in case of gaps)
    ctx.fillStyle = "#ffffff";
    ctx.fillRect(0, 0, totalWidth, totalHeight);

    let tilesProcessed = 0;
    let tilesFailed = 0;

    // Sort tiles by position (top-to-bottom, left-to-right) for consistent layering
    const sortedTiles = [...tiles].sort((a, b) => {
      if (a.y !== b.y) return a.y - b.y;
      return a.x - b.x;
    });

    // Load and draw each tile
    for (let i = 0; i < sortedTiles.length; i++) {
      const tile = sortedTiles[i];

      try {
        const img = await loadImage(tile.screenshot);

        // Draw the tile at its position
        // For overlapping regions, we draw the full tile and let later tiles overwrite
        // This is fine since tiles should be non-overlapping in a proper capture
        ctx.drawImage(img, tile.x, tile.y);

        tilesProcessed++;
      } catch (error) {
        console.warn(`[thumbnail-cropper] Failed to load tile at (${tile.x}, ${tile.y}):`, error);
        tilesFailed++;

        if (failOnError) {
          return null;
        }
      }

      // Report progress
      if (onProgress) {
        onProgress(i + 1, sortedTiles.length);
      }
    }

    // Check if we processed any tiles
    if (tilesProcessed === 0) {
      console.error("[thumbnail-cropper] No tiles were successfully processed");
      return null;
    }

    // Convert canvas to base64
    const mimeType = `image/${format}`;
    const dataUrl =
      format === "png" ? canvas.toDataURL(mimeType) : canvas.toDataURL(mimeType, quality);
    const base64 = dataUrl.split(",")[1];

    return {
      data: base64,
      width: totalWidth,
      height: totalHeight,
      tilesProcessed,
      tilesFailed,
    };
  } catch (error) {
    console.error("[thumbnail-cropper] Failed to stitch screenshots:", error);
    return null;
  }
}

/**
 * Stitch screenshots and return as a data URL (for direct use in img src)
 *
 * @param tiles - Array of screenshot tiles
 * @param totalWidth - Total width of the full page
 * @param totalHeight - Total height of the full page
 * @param options - Stitching options
 * @returns Data URL string, or null if stitching fails
 */
export async function stitchScreenshotsToDataUrl(
  tiles: ScreenshotTile[],
  totalWidth: number,
  totalHeight: number,
  options: StitchOptions = {},
): Promise<string | null> {
  const result = await stitchScreenshots(tiles, totalWidth, totalHeight, options);

  if (!result) {
    return null;
  }

  const format = options.format ?? "png";
  return `data:image/${format};base64,${result.data}`;
}
