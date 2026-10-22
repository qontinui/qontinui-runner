/**
 * Thin client over the `/ui-bridge/vision/*` HTTP routes.
 *
 * These wrappers exist because Phase 2 of the UI Bridge vision-pipeline plan
 * removed the legacy `/ui-bridge/control/screenshot`, `/ui-bridge/sdk/screenshot`,
 * and `/ui-bridge/control/annotated-screenshot` routes. The new endpoints
 * return `{ url, sha256, width, height, bytes, format, contract }`, where `url`
 * is `GET /ui-bridge/vision/cache/<sha256>`. Base64 bytes are in the payload
 * only when the request sets `inline: true` (capped at 5 MiB — over the cap the
 * runner answers 413 `INLINE_TOO_LARGE` naming `url`). `captureScreenshotBase64`
 * asks for `inline: true` so it needs no second round-trip.
 */
import { getApiBase, tracedFetch } from "./runner-api";

/**
 * Raw response shape returned by `POST /ui-bridge/vision/capture` and
 * `POST /ui-bridge/vision/annotate`. Mirrors `CaptureResponse` in
 * `src-tauri/src/mcp/ui_bridge/vision_routes.rs`.
 */
export interface VisionCaptureResponse {
  /** Relative HTTP path serving the bytes: `/ui-bridge/vision/cache/<sha256>`. */
  url: string;
  /** Standard base64 of the bytes — present only when the request set `inline: true`. */
  base64?: string;
  /**
   * Hex SHA-256 of the runner's cache KEY (mutation id + request shape), NOT a
   * hash of the image bytes. Names the entry `url` serves.
   */
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
  /**
   * Return the bytes as `base64` in the same response (capped at 5 MiB; over
   * the cap the runner answers 413 `INLINE_TOO_LARGE` with `hint.url`).
   */
  inline?: boolean;
}

/** A non-2xx `vision/capture` answer, with its HTTP status and envelope `code`. */
export class VisionCaptureError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly code?: string,
  ) {
    super(message);
    this.name = "VisionCaptureError";
  }
}

/**
 * POST `/ui-bridge/vision/capture` and return the JSON envelope. Throws if
 * the request fails or the server returns a non-success payload (a
 * `VisionCaptureError` for a non-2xx status).
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
    let code: string | undefined;
    try {
      const body: unknown = await resp.json();
      if (body && typeof body === "object" && "code" in body) {
        code = String((body as { code: unknown }).code);
      }
    } catch {
      // Non-JSON error body: the status alone is the signal.
    }
    throw new VisionCaptureError(
      `vision/capture failed: HTTP ${resp.status}${code ? ` ${code}` : ""}`,
      resp.status,
      code,
    );
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
 * response shape: `{ screenshot, width, height, format, sha256, url }`.
 * Performs one `vision/capture` with `inline: true`, so the base64 payload
 * arrives in the same response. A capture over the runner's inline cap
 * (413 `INLINE_TOO_LARGE`) is re-requested without `inline` and fetched by
 * its cache URL instead.
 *
 * Use this only when a downstream consumer genuinely needs the base64 bytes
 * (e.g. client-side thumbnail cropping). When you can fetch via the cache
 * URL directly, prefer `captureVision` to avoid moving multi-MB base64 strings.
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
  url: string;
}> {
  let meta: VisionCaptureResponse;
  try {
    meta = await captureVision({ ...req, inline: true });
  } catch (e) {
    if (!(e instanceof VisionCaptureError && e.code === "INLINE_TOO_LARGE")) {
      throw e;
    }
    // The capture is cached; this second call is a cache hit.
    meta = await captureVision({ ...req, inline: false });
  }
  const screenshot =
    meta.base64 ?? uint8ToBase64(await fetchVisionCacheBytes(meta.sha256));
  return {
    screenshot,
    width: meta.width,
    height: meta.height,
    format: meta.format,
    sha256: meta.sha256,
    url: meta.url,
  };
}
