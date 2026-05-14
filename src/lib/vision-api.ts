/**
 * Thin client over the `/ui-bridge/vision/*` HTTP routes.
 *
 * These wrappers exist because Phase 2 of the UI Bridge vision-pipeline plan
 * removed the legacy `/ui-bridge/control/screenshot`, `/ui-bridge/sdk/screenshot`,
 * and `/ui-bridge/control/annotated-screenshot` routes. The new endpoints
 * return `{ path, sha256, width, height, bytes, format, contract }` — base64
 * image data is no longer in the JSON payload. Consumers that still need
 * in-process base64 use `captureScreenshotBase64`, which performs the
 * extra `GET /ui-bridge/vision/cache/{sha256}` fetch and encodes the bytes.
 */
import { getApiBase, tracedFetch } from "./runner-api";

/**
 * Raw response shape returned by `POST /ui-bridge/vision/capture` and
 * `POST /ui-bridge/vision/annotate`. Mirrors `CaptureResponse` in
 * `src-tauri/src/mcp/ui_bridge/vision_routes.rs`.
 */
export interface VisionCaptureResponse {
  /** `tmp_vision_cache/<sha256>.<ext>` relative to runner CWD. */
  path: string;
  sha256: string;
  width: number;
  height: number;
  bytes: number;
  format: "jpeg" | "webp" | "png";
  /** OutputContract name, e.g. `claude_vision_v1`. */
  contract: string;
}

/** Request body for `POST /ui-bridge/vision/capture`. */
export interface VisionCaptureRequest {
  /** Pixel-space crop rect (optional). */
  region?: { x: number; y: number; width: number; height: number };
  /** Element id (resolves to a rect via the runner's discover snapshot). */
  element?: string;
  /** `"claude"` (default), `"webp"`, or `"png_strict"`. */
  contract?: "claude" | "webp" | "png_strict";
  /** Bypass cache (Phase 2: accepted but no-op). */
  force?: boolean;
}

/**
 * POST `/ui-bridge/vision/capture` and return the JSON envelope. Throws if
 * the request fails or the server returns a non-success payload.
 */
export async function captureVision(
  req: VisionCaptureRequest = {},
): Promise<VisionCaptureResponse> {
  const resp = await tracedFetch(`${getApiBase()}/ui-bridge/vision/capture`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ contract: "claude", ...req }),
  });
  if (!resp.ok) {
    throw new Error(`vision/capture failed: HTTP ${resp.status}`);
  }
  const json = await resp.json();
  // The server wraps responses in `ApiResponse { success, data, error }`.
  if (json && typeof json === "object" && "success" in json) {
    if (!json.success || !json.data) {
      throw new Error(json.error ?? "vision/capture: unknown error");
    }
    return json.data as VisionCaptureResponse;
  }
  // Defensive: if the envelope is missing, accept the bare object.
  return json as VisionCaptureResponse;
}

/**
 * GET `/ui-bridge/vision/cache/{sha256}` and return the raw bytes as
 * `Uint8Array`. Server streams the file as `image/jpeg`/`png`/`webp`.
 */
export async function fetchVisionCacheBytes(sha256: string): Promise<Uint8Array> {
  const resp = await tracedFetch(
    `${getApiBase()}/ui-bridge/vision/cache/${encodeURIComponent(sha256)}`,
  );
  if (!resp.ok) {
    throw new Error(`vision/cache/${sha256} failed: HTTP ${resp.status}`);
  }
  const buf = await resp.arrayBuffer();
  return new Uint8Array(buf);
}

/**
 * Encode a `Uint8Array` to a base64 string (no `data:` prefix). Uses a
 * chunked `String.fromCharCode` so we don't exceed call-stack limits on
 * multi-megabyte screenshots.
 */
function uint8ToBase64(bytes: Uint8Array): string {
  const CHUNK = 0x8000; // 32 KiB
  let binary = "";
  for (let i = 0; i < bytes.length; i += CHUNK) {
    const chunk = bytes.subarray(i, i + CHUNK);
    binary += String.fromCharCode(...chunk);
  }
  return btoa(binary);
}

/**
 * High-level helper that mirrors the legacy `/ui-bridge/sdk/screenshot`
 * response shape: `{ screenshot, width, height, format, sha256, path }`.
 * Performs `vision/capture` followed by a `vision/cache/{sha}` GET to
 * materialize the base64 payload in-process.
 *
 * Use this only when a downstream consumer genuinely needs the base64 bytes
 * (e.g. client-side thumbnail cropping). When you can pass the file path or
 * fetch via the cache URL directly, prefer `captureVision` to avoid the
 * extra round-trip and the cost of base64 encoding multi-MB buffers.
 */
export async function captureScreenshotBase64(
  req: VisionCaptureRequest = {},
): Promise<{
  /** Base64-encoded image bytes (no `data:` prefix). */
  screenshot: string;
  width: number;
  height: number;
  format: "jpeg" | "webp" | "png";
  sha256: string;
  path: string;
}> {
  const meta = await captureVision(req);
  const bytes = await fetchVisionCacheBytes(meta.sha256);
  const screenshot = uint8ToBase64(bytes);
  return {
    screenshot,
    width: meta.width,
    height: meta.height,
    format: meta.format,
    sha256: meta.sha256,
    path: meta.path,
  };
}
