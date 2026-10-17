/**
 * The runner-webview half of the in-situ asset override (plan
 * 2026-10-13-image-generation-and-visual-judgment, Phase 18): the
 * `media_override_*` messages `src-tauri/src/mcp/ui_bridge/media_override.rs`
 * sends for `POST / DELETE / GET /ui-bridge/control/media/override`.
 *
 * It runs the SDK's own engine (`@qontinui/ui-bridge` `control/media-override`)
 * — the same code the relay dispatcher and the injected runtime run — behind
 * the SDK's dev-controls gate. The runner's webview opts in only on a Vite dev
 * build (`App.tsx`, `features.devControls: import.meta.env.DEV`); an exe-mode
 * runner never is one, so on every published runner these messages answer the
 * typed refusal below and no page-altering capability reaches a user.
 */

import {
  applyMediaOverride,
  listMediaOverrides,
  restoreMediaOverrides,
} from "@qontinui/ui-bridge/control";
import { devControlsEnabled, getGlobalRegistry } from "@qontinui/ui-bridge/core";

export type MediaOverrideRequestType =
  | "media_override_apply"
  | "media_override_restore"
  | "media_override_list";

/** The reply fields the hook sends back (it adds `requestId`, `type`, `timestamp`). */
export interface MediaOverrideReply {
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
    "The media override is a dev control. The runner's webview enables it only on a Vite dev " +
    "build; use /ui-bridge/sdk/control/media/override against an app that opted in, or the " +
    "injected runtime (ui-bridge-inject).",
} as const;

/**
 * The request body of a media-override message. The Rust layer merges the
 * validated HTTP body into the envelope's top level, so it is everything that
 * is not envelope metadata.
 */
export function overrideBody(payload: object): Record<string, unknown> {
  const {
    requestId: _requestId,
    type: _type,
    windowLabel: _windowLabel,
    ...rest
  } = payload as Record<string, unknown>;
  return rest;
}

/** Run one media-override message against the webview's own DOM. */
export async function runMediaOverrideRequest(
  type: MediaOverrideRequestType,
  payload: object,
): Promise<MediaOverrideReply> {
  if (!devControlsEnabled()) {
    return { success: false, error: DEV_CONTROLS_REFUSAL.error, data: { ...DEV_CONTROLS_REFUSAL } };
  }
  const result =
    type === "media_override_apply"
      ? await applyMediaOverride(
          overrideBody(payload),
          (id) => getGlobalRegistry().getElement(id)?.element,
        )
      : type === "media_override_restore"
        ? restoreMediaOverrides(overrideBody(payload))
        : listMediaOverrides();
  return result.success
    ? { success: true, data: result }
    : { success: false, error: result.error, data: result };
}
