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

import { setTextVisibility, textHidden } from "@qontinui/ui-bridge/control";
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
 * How recent the SDK's last restore must be for the probe to wait for its
 * paint: one paint fence. An unthrottled webview paints a restore within a
 * frame; past this window, a restore is taken as painted.
 */
export const RECENT_RESTORE_MS = PAINT_FENCE_TIMEOUT_MS;

/**
 * Whether the SDK's last restore may not have been painted yet: it happened
 * within {@link RECENT_RESTORE_MS}, or it cannot be read. Read through the
 * SDK's own public answer — `setTextVisibility({hidden: false})` with nothing
 * hidden writes nothing and returns the last restore (`changed: false`). Call
 * it only right after `textHidden()` answered `false`, in the same synchronous
 * run, so there is nothing for it to restore. Wall-clock: a clock stepped
 * forward past the window reads a recent restore as old (a backward step only
 * makes the probe wait).
 */
function restoreMayBeUnpainted(): boolean {
  const read = setTextVisibility({ hidden: false });
  if (!read.success || read.hidden !== false || read.changed !== false) return true;
  if (read.restore === null) return false;
  const restoredAt = read.restore?.restoredAt;
  if (typeof restoredAt !== "number" || !Number.isFinite(restoredAt)) return true;
  return Date.now() - restoredAt < RECENT_RESTORE_MS;
}

/**
 * Run one text-visibility message against the webview's own DOM.
 *
 * Two fields only the runner's own arm (`capture_text_hidden_pair` in
 * `text_visibility.rs`) sends, never forwarded to the SDK:
 *
 * - `query: true` — the probe: answer `{hidden, paintFence}` from the SDK's
 *   `textHidden()`, with no page-state change (`textHidden()` may lazily
 *   create the SDK's shared state slot, nothing more). The arm asks this
 *   before frame A. A restore — the SDK's TTL, or a restore whose reply the
 *   runner never saw — writes synchronously and paints later, so when the
 *   text reads visible but the SDK's last restore is recent (within
 *   {@link RECENT_RESTORE_MS}), the state is read on both sides of a paint
 *   fence: `hidden: false` then answers only once a frame has painted, and
 *   `hidden: null` when the state differs before and after the fence. With no
 *   recent restore there is nothing unpainted to wait for, and the answer is
 *   `paintFence: "not_needed"` at once — so a minimized or occluded dev
 *   runner, whose paused frames would time every fence out, still gets its
 *   frame A. Where dev controls are off nothing can ever have been hidden in
 *   this realm (the gate is one-way), so the answer is likewise
 *   `"not_needed"`, and it carries `devControls: false` so the arm sends no
 *   hide at all (it would only be refused). Not seen: another caller's hide
 *   AND its restore, both made inside the fence's own wait.
 * - `onlyIfVisible: true` — hide only when text is not already hidden. When it
 *   is, answer `{hidden: true, changed: false, wasHidden: true}` WITHOUT
 *   calling the SDK, so another caller's hide is neither re-armed nor touched.
 *
 * The runner's text-visibility routes refuse a body carrying either field —
 * the direct route (`POST /ui-bridge/control/text-visibility`) with a 400,
 * the `/sdk/` route with `{success: false, code: "INVALID_REQUEST"}` — so only
 * the arm sends them; an explicit re-hide on the direct route re-arms the
 * TTL, as the SDK documents.
 *
 * Every reply that reached the SDK carries `wasHidden`: `textHidden()` read
 * immediately before `setTextVisibility`, in the same synchronous run, so no
 * other hide can land between the two. `wasHidden: false` on a hide means THIS
 * request created it — no clock is involved. `null` means the SDK could not
 * tell (another copy of it owns the state), and the runner reads that as
 * unknown.
 */
export async function runTextVisibilityRequest(
  payload: object,
  waitForPaint: () => Promise<PaintFence> = nextPaint,
): Promise<TextVisibilityReply> {
  const { query, onlyIfVisible, ...body } = textVisibilityBody(payload);
  if (query === true) {
    if (!devControlsEnabled()) {
      return {
        success: true,
        data: {
          success: true,
          query: true,
          hidden: textHidden(),
          paintFence: "not_needed",
          devControls: false,
        },
      };
    }
    const before = textHidden();
    if (before === false && !restoreMayBeUnpainted()) {
      return {
        success: true,
        data: { success: true, query: true, hidden: false, paintFence: "not_needed" },
      };
    }
    const paintFence = await waitForPaint();
    const after = textHidden();
    return {
      success: true,
      data: { success: true, query: true, hidden: before === after ? after : null, paintFence },
    };
  }
  if (!devControlsEnabled()) {
    return { success: false, error: DEV_CONTROLS_REFUSAL.error, data: { ...DEV_CONTROLS_REFUSAL } };
  }
  const wasHidden = textHidden();
  if (onlyIfVisible === true && body.hidden === true && wasHidden === true) {
    return {
      success: true,
      data: { success: true, hidden: true, changed: false, wasHidden: true },
    };
  }
  const result = setTextVisibility(body);
  if (!result.success) {
    return { success: false, error: result.error, data: { ...result, wasHidden } };
  }
  if (!result.changed) {
    return { success: true, data: { ...result, wasHidden } };
  }
  const paintFence = await waitForPaint();
  return { success: true, data: { ...result, paintFence, wasHidden } };
}
