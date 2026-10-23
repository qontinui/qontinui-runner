/**
 * The runner-webview half of text visibility (plan
 * 2026-10-13-image-generation-and-visual-judgment, Phase 16): the
 * `text_visibility` message `src-tauri/src/mcp/ui_bridge/text_visibility.rs`
 * sends for `POST /ui-bridge/control/text-visibility`, and the hide/restore
 * pair `vision/analyze` and `vision/assert` send under `captureTextHidden`.
 *
 * It runs the SDK's own engine (`@qontinui/ui-bridge` `setTextVisibility`) —
 * the same code the relay dispatcher and the injected runtime run — behind the
 * SDK's dev-controls gate, which it shares with the media override. It works
 * only where this webview opted in: a Vite dev runner, once
 * qontinui-runner#2311's `features.devControls: import.meta.env.DEV` is in
 * `App.tsx`. Everywhere else — every exe-mode runner — it answers the typed
 * `capability_unavailable_in_build` refusal below. Text over a web page is
 * measured through inject-cli (`/verify-web`), not here. The gate is not a
 * security boundary: hiding text adds nothing beyond the existing
 * `page_evaluate` / eval surface.
 *
 * After a hide or restore that CHANGED the page, the reply waits for a paint
 * ({@link nextPaint}), so the frame the runner captures next shows the new
 * state rather than the one composited before the styles were written.
 */

import { setTextVisibility } from "@qontinui/ui-bridge/control";
import { devControlsEnabled } from "@qontinui/ui-bridge/core";

/** The reply fields the hook sends back (it adds `requestId`, `type`, `timestamp`). */
export interface TextVisibilityReply {
  success: boolean;
  error?: string;
  data: unknown;
}

/**
 * The refusal when the webview has not opted into dev controls, in the SDK's
 * vocabulary (`code` + `reason`), so a caller branches on the same fields it
 * would read from the relay or the injected runtime.
 */
export const DEV_CONTROLS_REFUSAL = {
  code: "capability_unavailable_in_build",
  reason: "dev_controls_disabled",
  error:
    "Text visibility is a dev control. The runner's webview enables it only on a Vite dev " +
    "build; use /ui-bridge/sdk/control/text-visibility against an app that opted in, or the " +
    "injected runtime (ui-bridge-inject).",
} as const;

/** How long the reply waits for a paint before answering anyway. */
export const PAINT_FENCE_TIMEOUT_MS = 500;

/**
 * Which signal released the paint fence: two animation frames (the styles were
 * painted), or the timeout (a hidden or occluded webview throttles frames, so
 * the capture may still see the previous state — vision-core then reads frame
 * B as still painting its glyphs and answers unknown, never a false pass).
 */
export type PaintFence = "animation_frame" | "timeout";

/** Resolve after the next painted frame, or after {@link PAINT_FENCE_TIMEOUT_MS}. */
export function nextPaint(): Promise<PaintFence> {
  return new Promise((resolve) => {
    let settled = false;
    const done = (how: PaintFence) => {
      if (settled) return;
      settled = true;
      resolve(how);
    };
    setTimeout(() => done("timeout"), PAINT_FENCE_TIMEOUT_MS);
    if (typeof requestAnimationFrame === "function") {
      // The first callback runs before the frame that carries the new styles
      // is painted; the second, after it.
      requestAnimationFrame(() => requestAnimationFrame(() => done("animation_frame")));
    }
  });
}

/**
 * The request body of a text-visibility message. The Rust layer merges the
 * validated HTTP body into the envelope's top level, so it is everything that
 * is not envelope metadata.
 */
export function textVisibilityBody(payload: object): Record<string, unknown> {
  const {
    requestId: _requestId,
    type: _type,
    windowLabel: _windowLabel,
    ...rest
  } = payload as Record<string, unknown>;
  return rest;
}

/**
 * Run one text-visibility message against the webview's own DOM.
 *
 * The reply carries two stamps from this webview's own `Date.now()`:
 * `receivedAt`, taken immediately BEFORE the SDK call, and `answeredAt`, taken
 * after the paint wait. A hide's `hiddenAt` (the SDK's, from the same clock) at
 * or after `receivedAt` was created by THIS request; one before it is another
 * caller's that this request only re-armed. The runner decides ownership from
 * that one comparison, on one clock.
 */
export async function runTextVisibilityRequest(
  payload: object,
  waitForPaint: () => Promise<PaintFence> = nextPaint,
): Promise<TextVisibilityReply> {
  if (!devControlsEnabled()) {
    return { success: false, error: DEV_CONTROLS_REFUSAL.error, data: { ...DEV_CONTROLS_REFUSAL } };
  }
  const receivedAt = Date.now();
  const result = setTextVisibility(textVisibilityBody(payload));
  if (!result.success) {
    return { success: false, error: result.error, data: { ...result, receivedAt } };
  }
  if (!result.changed) {
    return { success: true, data: { ...result, receivedAt, answeredAt: Date.now() } };
  }
  const paintFence = await waitForPaint();
  return { success: true, data: { ...result, paintFence, receivedAt, answeredAt: Date.now() } };
}
