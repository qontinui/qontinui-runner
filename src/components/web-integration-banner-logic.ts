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
 * Which call to action a credential cause needs. Every one of these is served
 * by a command that ALREADY exists — `cognito_sign_in` for an interactive
 * re-login, `kick_device_jwt_refresher_cmd` for both the re-pair and the
 * "retry refresh now" arms. This plan adds no Tauri command.
 */
export type CredentialDarkCta = "sign_in" | "re_pair" | "retry_refresh";

/**
 * Payload of the `autonomy-credential-dark` event, widened by the
 * coord-credential-posture plan (DD3) from `{dark, message}` to carry the
 * CAUSE and the CTA that cause needs.
 *
 * `cause` is the coord-credential posture token (`expired`, `absent`,
 * `unrefreshable`, `upstream_401`) for a posture transition, `cognito_hard`
 * for the pre-existing HARD Cognito arm, and `recovered` when `dark` is false.
 * `since` is unix SECONDS at which the runner entered this state — the banner
 * says "since 03:54" because a user needs to know whether the sessions they
 * already opened are affected.
 */
export interface CredentialDarkSignal {
  dark: boolean;
  cause: string;
  message: string;
  cta: CredentialDarkCta | null;
  since: number | null;
}

/**
 * Stable signature used to decide "did the status change since the user
 * dismissed?". Any change to the bits the banner cares about resurfaces it.
 *
 * `deviceJwtPresent` is the real "is this runner paired?" signal post-Cognito
 * unification (Cognito sign-in / pair-code mint a device JWT; neither sets
 * `runner_token`). Including `registrationError` means a 401 transition (auth
 * failure after the credential was revoked / rotated server-side) re-shows the
 * banner even mid-session — the user explicitly needs to know.
 *
 * The credential POSTURE is part of the signature too. It was not, and that
 * was a hole: the signature carried `enabled | deviceJwtPresent |
 * registrationError`, none of which moves when the coord credential expires —
 * so a banner dismissed while healthy stayed dismissed through the transition
 * that mattered, and the runner was silent again.
 */
export function statusSignature(
  status: AuthBannerStatus | null,
  deviceJwtPresent: boolean,
  credentialDark: CredentialDarkSignal | null = null,
): string {
  if (!status) return "";
  return [
    status.enabled ? "1" : "0",
    deviceJwtPresent ? "P" : "_",
    status.registrationError ?? "",
    credentialDark?.dark ? `D:${credentialDark.cause}` : "_",
  ].join("|");
}

/**
 * Decide whether to show the banner given current status and dismissal state.
 *
 * Visibility rule:
 *   - Hide when status hasn't loaded yet (avoid flashing on every render).
 *   - Hide when `enabled === false` — user opted out, respect it.
 *   - Hide when `deviceJwtPresent` — the runner IS paired (has a device JWT
 *     from Cognito sign-in or a pair-code), so no authorization action is
 *     needed. NOTE: this used to key off `runnerTokenMasked`, which broke
 *     post-Cognito: Cognito-/pair-code-paired runners have an empty
 *     `runner_token`, so a fully-paired runner showed a perpetual "needs
 *     authorization" banner. Callers should pass `deviceJwtPresent !== false`
 *     so the not-yet-loaded state (null) is treated as paired (no flash).
 *   - Hide when the user dismissed it for the session AND status hasn't changed
 *     since dismissal (re-shows on reload, on un-pair, or on a new
 *     registration error).
 *   - SHOW unconditionally while `credentialDark.dark` — no opt-out, no
 *     dismissal, no "the runner is paired so it must be fine". See below.
 */
export function shouldShowAuthBanner(
  status: AuthBannerStatus | null,
  deviceJwtPresent: boolean,
  dismissedSignature: string | null,
  credentialDark: CredentialDarkSignal | null = null,
): boolean {
  // NOT DISMISSABLE WHILE DARK, and not suppressible by any other rule.
  //
  // This overrides every hide below, including `enabled === false` and a
  // present device JWT — because while the posture is dark those two say
  // nothing useful: a paired runner holding an EXPIRED credential has a device
  // JWT and is still spawning sessions with no coord access. A dismissed
  // credential banner is a silent runner again, which is the whole defect.
  if (credentialDark?.dark) return true;
  if (!status) return false;
  if (!status.enabled) return false;
  if (deviceJwtPresent) return false;
  if (
    dismissedSignature !== null &&
    dismissedSignature === statusSignature(status, deviceJwtPresent, credentialDark)
  ) {
    return false;
  }
  return true;
}

/**
 * What the credential banner RENDERS for a given cause — kept here rather than
 * in the `.tsx` so every cause's wording and CTA is unit-testable in node.
 *
 * `ctaAction` names which existing Tauri command the button invokes:
 * `cognito_sign_in` for an interactive re-login, `kick_refresher` for the
 * headless retry (`kick_device_jwt_refresher_cmd`).
 */
export interface CredentialDarkPresentation {
  title: string;
  body: string;
  ctaLabel: string | null;
  ctaAction: "cognito_sign_in" | "kick_refresher" | null;
}

/** Render `since` (unix SECONDS) as a wall-clock time for the banner body. */
export function formatSince(since: number): string {
  return new Date(since * 1000).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
  });
}

const CREDENTIAL_DARK_TITLES: Record<string, string> = {
  cognito_hard: "Autonomous sessions paused",
  expired: "Coord credential expired",
  unrefreshable: "Coord credential expired — automatic refresh failed",
  absent: "This runner has no coord credential",
  upstream_401: "Coord is rejecting this runner's credential",
};

const CREDENTIAL_DARK_CTA_LABELS: Record<CredentialDarkCta, string> = {
  sign_in: "Sign in",
  re_pair: "Re-pair",
  retry_refresh: "Retry refresh now",
};

/**
 * Coerce whatever arrived on the `autonomy-credential-dark` event into a
 * complete {@link CredentialDarkSignal}.
 *
 * The event is emitted by the RUNNER BINARY, and the frontend can be running
 * against a build that predates the widened payload (`{dark, message}` only).
 * An unknown cause must still render a banner — a runner that says "I am dark"
 * in the old shape is exactly as dark as one that says it in the new shape.
 */
export function normalizeCredentialDarkSignal(raw: unknown): CredentialDarkSignal | null {
  if (raw === null || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.dark !== "boolean") return null;
  const cta =
    r.cta === "sign_in" || r.cta === "re_pair" || r.cta === "retry_refresh" ? r.cta : null;
  return {
    dark: r.dark,
    // A legacy payload carries no cause. `cognito_hard` is what the only
    // pre-widening emitter meant, so that is the honest default for `dark`.
    cause:
      typeof r.cause === "string" && r.cause.length > 0
        ? r.cause
        : r.dark
          ? "cognito_hard"
          : "recovered",
    message: typeof r.message === "string" ? r.message : "",
    // A legacy dark payload had exactly one remedy: sign in again.
    cta: cta ?? (r.dark && r.cta === undefined ? "sign_in" : null),
    since: typeof r.since === "number" && Number.isFinite(r.since) ? r.since : null,
  };
}

export function credentialDarkPresentation(
  signal: CredentialDarkSignal,
  formatTime: (since: number) => string = formatSince,
): CredentialDarkPresentation {
  const title = CREDENTIAL_DARK_TITLES[signal.cause] ?? "Coord credential problem";
  const prefix =
    typeof signal.since === "number" && Number.isFinite(signal.since)
      ? `Since ${formatTime(signal.since)} — `
      : "";
  const body = `${prefix}${signal.message}`;
  const ctaAction =
    signal.cta === null
      ? null
      : signal.cta === "sign_in"
        ? ("cognito_sign_in" as const)
        : ("kick_refresher" as const);
  return {
    title,
    body,
    ctaLabel: signal.cta === null ? null : CREDENTIAL_DARK_CTA_LABELS[signal.cta],
    ctaAction,
  };
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
