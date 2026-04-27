/**
 * Pure visibility logic for WebIntegrationAuthBanner.
 *
 * Lives in its own file (no React, no Tauri imports) so it can be unit
 * tested without a DOM harness and without dragging in `@tauri-apps/api`
 * — which throws at module load in a plain node test environment.
 */

/** Subset of `get_web_integration_status` used by the banner. */
export interface AuthBannerStatus {
  enabled: boolean;
  runnerTokenMasked: string;
  registrationError: string | null;
}

/**
 * Stable signature used to decide "did the status change since the user
 * dismissed?". Any change to the bits the banner cares about resurfaces it.
 *
 * Including `registrationError` means a 401 transition (auth failure after
 * the token was revoked / rotated server-side) re-shows the banner even
 * mid-session — the user explicitly needs to know.
 */
export function statusSignature(status: AuthBannerStatus | null): string {
  if (!status) return "";
  return [
    status.enabled ? "1" : "0",
    status.runnerTokenMasked.length > 0 ? "T" : "_",
    status.registrationError ?? "",
  ].join("|");
}

/**
 * Decide whether to show the banner given current status and dismissal state.
 *
 * Visibility rule:
 *   - Hide when status hasn't loaded yet (avoid flashing on every render).
 *   - Hide when `enabled === false` — user opted out, respect it.
 *   - Hide when `runnerTokenMasked` is non-empty — token is set, no action needed.
 *   - Hide when the user dismissed it for the session AND status hasn't changed
 *     since dismissal (re-shows on reload, on token clear, or on a new
 *     registration error).
 */
export function shouldShowAuthBanner(
  status: AuthBannerStatus | null,
  dismissedSignature: string | null,
): boolean {
  if (!status) return false;
  if (!status.enabled) return false;
  if (status.runnerTokenMasked.length > 0) return false;
  if (dismissedSignature !== null && dismissedSignature === statusSignature(status)) {
    return false;
  }
  return true;
}
