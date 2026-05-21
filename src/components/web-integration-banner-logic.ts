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

// ---------------------------------------------------------------------------
// Phase 4 (unified-devices migration): re-pair CTA
// ---------------------------------------------------------------------------

/**
 * Inputs for {@link shouldShowRePairCta}. All fields originate from
 * Tauri-side state at render time:
 *
 *   - `tier` — from `get_runner_tier` (via `useRunnerTier`)
 *   - `runnerTokenPresent` — derived from `runnerTokenMasked.length > 0`
 *   - `deviceJwtPresent` — from the `device_jwt_present` Tauri command
 *   - `firstDetectedAt` — ms timestamp of the first render at which the
 *     above three conditions all aligned this session (component state,
 *     NOT settings — transient UI signal)
 *   - `now` — current time in ms (passed in for testability)
 *
 * Returns true iff the migration banner should surface RIGHT NOW. The
 * 5-minute grace period covers the device-JWT refresher's normal tick:
 * if the refresher is already healing the gap, we don't want to flash a
 * "re-pair required" message on every boot.
 */
export interface RePairCtaInputs {
  tier: string;
  runnerTokenPresent: boolean;
  deviceJwtPresent: boolean;
  firstDetectedAt: number | null;
  now: number;
}

/** 5 minutes in milliseconds — matches the device-JWT refresher's tick. */
export const RE_PAIR_CTA_GRACE_MS = 5 * 60 * 1000;

/**
 * Decide whether to surface the post-upgrade re-pair CTA. See
 * {@link RePairCtaInputs} for input semantics.
 *
 * Visibility rule:
 *   - Hide unless tier === "qontinui_account" (Tier 2 only).
 *   - Hide unless this runner WAS paired pre-upgrade (runner_token present).
 *   - Hide if a device-JWT is already present (refresher succeeded).
 *   - Hide if the migration state has been detected for less than 5min
 *     (RE_PAIR_CTA_GRACE_MS) — covers transient boot-time staleness.
 *
 * Note: `firstDetectedAt === null` means we haven't observed the
 * migration state yet → hide. The component records the timestamp on
 * the first render where the underlying state qualifies.
 */
export function shouldShowRePairCta(input: RePairCtaInputs): boolean {
  if (input.tier !== "qontinui_account") return false;
  if (!input.runnerTokenPresent) return false;
  if (input.deviceJwtPresent) return false;
  if (input.firstDetectedAt === null) return false;
  return input.now - input.firstDetectedAt >= RE_PAIR_CTA_GRACE_MS;
}

/** Command name the re-pair CTA invokes — extracted so tests can assert
 * the contract without importing `@tauri-apps/api/core` (which throws at
 * module load in node).
 */
export const KICK_DEVICE_JWT_REFRESHER_CMD = "kick_device_jwt_refresher_cmd";

/**
 * Build a click handler that invokes the device-JWT refresher kick.
 * Extracted so the test suite can assert the click → invoke wiring in
 * `node` without a DOM. Returns a Promise that resolves once the invoke
 * call settles (success or thrown).
 */
export function makeRePairClickHandler(
  invoker: (cmd: string, args: Record<string, unknown>) => Promise<void>,
): () => Promise<void> {
  return async () => {
    await invoker(KICK_DEVICE_JWT_REFRESHER_CMD, {});
  };
}
