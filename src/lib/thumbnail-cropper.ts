/**
 * Thumbnail Cropper Utility
 *
 * Crops thumbnails from a full page screenshot given element bounds.
 * Uses an offscreen canvas for efficient client-side cropping.
 */

export interface ElementBounds {
  x: number;
  y: number;
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
  options: CropOptions = {}
): Promise<string | null> {
  const { maxSize = 48, format = "png", quality = 0.8 } = options;

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
    if (cropWidth > img.width * 0.9 && cropHeight > img.height * 0.9) {
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

    ctx.drawImage(
      img,
      cropX,
      cropY,
      cropWidth,
      cropHeight,
      0,
      0,
      thumbWidth,
      thumbHeight
    );

    // Return as base64 (strip data URL prefix)
    const mimeType = `image/${format}`;
    const dataUrl = format === "png" ? canvas.toDataURL(mimeType) : canvas.toDataURL(mimeType, quality);
    return dataUrl.split(",")[1];
  } catch (error) {
    console.error("[thumbnail-cropper] Failed to crop thumbnail:", error);
    return null;
  }
}

/**
 * Batch crop thumbnails for multiple elements.
 * More efficient than calling cropThumbnail multiple times as it loads the image once.
 *
 * @param screenshotBase64 - Base64 encoded PNG screenshot (without data URL prefix)
 * @param elements - Array of elements with id and bounds
 * @param options - Crop options
 * @returns Map of element ID to base64 thumbnail
 */
export async function cropThumbnails(
  screenshotBase64: string,
  elements: Array<{ id: string; bounds: ElementBounds }>,
  options: CropOptions = {}
): Promise<Map<string, string>> {
  const { maxSize = 48, format = "png", quality = 0.8 } = options;
  const thumbnails = new Map<string, string>();

  if (!screenshotBase64 || elements.length === 0) {
    return thumbnails;
  }

  try {
    // Load image once
    const img = await loadImage(screenshotBase64);

    // Process all elements
    for (const element of elements) {
      const { bounds } = element;

      // Calculate crop region (clamp to image bounds)
      const cropX = Math.max(0, Math.floor(bounds.x));
      const cropY = Math.max(0, Math.floor(bounds.y));
      const cropWidth = Math.min(Math.ceil(bounds.width), img.width - cropX);
      const cropHeight = Math.min(Math.ceil(bounds.height), img.height - cropY);

      // Skip if crop region is too small or invalid
      if (cropWidth <= 2 || cropHeight <= 2) {
        continue;
      }

      // Skip very large elements (probably full-page containers)
      if (cropWidth > img.width * 0.9 && cropHeight > img.height * 0.9) {
        continue;
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
        continue;
      }

      // Use high-quality image scaling
      ctx.imageSmoothingEnabled = true;
      ctx.imageSmoothingQuality = "high";

      ctx.drawImage(
        img,
        cropX,
        cropY,
        cropWidth,
        cropHeight,
        0,
        0,
        thumbWidth,
        thumbHeight
      );

      // Store thumbnail
      const mimeType = `image/${format}`;
      const dataUrl = format === "png" ? canvas.toDataURL(mimeType) : canvas.toDataURL(mimeType, quality);
      thumbnails.set(element.id, dataUrl.split(",")[1]);
    }

    return thumbnails;
  } catch (error) {
    console.error("[thumbnail-cropper] Failed to batch crop thumbnails:", error);
    return thumbnails;
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
  maxSize: number = 200
): Promise<string | null> {
  return cropThumbnail(screenshotBase64, bounds, { maxSize, format: "png" });
}
