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
  /**
   * WHICH authority published this signal — `"cognito"` for the Cognito
   * refresh loop, `"posture"` for the coord-credential posture.
   *
   * Two independent authorities publish onto one event. Without a source they
   * last-writer-win into a single slot and the banner can be wrongly CLEARED:
   * Cognito goes dark, the user signs in, Cognito's recovery fires
   * `dark:false`, the banner clears — while the coord slot is still
   * `unrefreshable`, and the posture arm will not re-fire because the posture
   * did not change. So the component holds one signal PER SOURCE and reduces
   * them ({@link applyCredentialDarkSignal}, {@link effectiveCredentialDark}).
   */
  source: string;
  dark: boolean;
  cause: string;
  message: string;
  cta: CredentialDarkCta | null;
  since: number | null;
  /**
   * The tenant the posture is about — `/health`'s `coordCredential.tenantId`,
   * or the event's `tenant_id`. `null` when the signal names no tenant (the
   * Cognito arm, the legacy default slot, an unattributable orphan, or a
   * runner build that predates the field). Plan
   * 2026-09-14-credential-posture-third-residuals Phase 3: on `absent`/`dark`
   * it is the tenant this runner is pinned to and holds no usable credential
   * for, so the banner names it and offers the tenant switch.
   */
  tenantId: string | null;
  /**
   * `true` only when the runner marked this posture as the UNSERVED-PIN arm:
   * the tenant `machine.json` pins holds no credential the forwarder would
   * present (event `pinned_tenant`, snapshot `pinnedTenant`). The tenant
   * switch is gated on THIS, never on `tenantId` alone — a measured sibling
   * slot's `dark` names a tenant too, and re-pinning cannot help it.
   */
  pinnedTenant: boolean;
}

/** The two authorities the runner binary publishes under. */
export const DARK_SOURCE_COGNITO = "cognito";
export const DARK_SOURCE_POSTURE = "posture";

/**
 * Fold a newly-arrived signal into the per-source map.
 *
 * A `dark:false` clears ONLY its own source's entry — that is the whole point:
 * Cognito recovering says nothing about the coord credential.
 */
export function applyCredentialDarkSignal(
  prev: Record<string, CredentialDarkSignal>,
  next: CredentialDarkSignal,
): Record<string, CredentialDarkSignal> {
  const out = { ...prev };
  if (next.dark) {
    out[next.source] = next;
  } else {
    delete out[next.source];
  }
  return out;
}

/**
 * Which cause the banner shows when more than one authority is dark at once.
 * Ordered by how much operator action the cause needs: a state whose automatic
 * recovery has already been tried and refused outranks one that has not.
 */
const CREDENTIAL_DARK_PRIORITY: string[] = [
  "unrefreshable",
  "cognito_hard",
  "absent",
  "upstream_401",
  "expired",
];

/**
 * The single signal the banner renders, or `null` when no source is dark.
 *
 * Never "the most recent one": the most recent event is routinely a RECOVERY
 * from the other authority, which is exactly how the banner used to be cleared
 * while the runner was still dark.
 */
export function effectiveCredentialDark(
  bySource: Record<string, CredentialDarkSignal>,
): CredentialDarkSignal | null {
  const dark = Object.values(bySource).filter((s) => s.dark);
  if (dark.length === 0) return null;
  const rank = (s: CredentialDarkSignal) => {
    const i = CREDENTIAL_DARK_PRIORITY.indexOf(s.cause);
    return i === -1 ? CREDENTIAL_DARK_PRIORITY.length : i;
  };
  return dark.reduce((best, s) => (rank(s) < rank(best) ? s : best));
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
 * `ctaAction` names which existing Tauri command the button invokes, and the
 * label always says what that command does:
 *
 * - `cognito_sign_in` — the interactive sign-in, which binds the device and
 *   mints a fresh device JWT. Serves BOTH `sign_in` ("Sign in") and `re_pair`
 *   ("Sign in to re-pair"): the postures that ask for a re-pair (`absent`,
 *   `unrefreshable`, `upstream_401`) have already exhausted the automatic
 *   rungs, so re-running the refresher would complete with no error and leave
 *   the banner standing. Sign-in is the only rung that can re-pair.
 * - `kick_refresher` — the headless retry (`kick_device_jwt_refresher_cmd`),
 *   for `retry_refresh` ("Retry refresh now") only.
 */
export interface CredentialDarkPresentation {
  title: string;
  body: string;
  ctaLabel: string | null;
  ctaAction: "cognito_sign_in" | "kick_refresher" | null;
  /**
   * Render the "Switch active tenant" action, which opens the tenant switcher
   * (the operator PICKS; nothing is re-pinned with a guessed tenant).
   *
   * True only for the runner-marked PINNED-TENANT arm (`pinnedTenant`) on an
   * `absent`/`dark` signal with a tenant, AND when the switcher
   * can act (`useTenant().showSwitcher`, i.e. more than one bound tenant). A
   * sign-in lands in whichever tenant coord stamps and never re-pins, so on a
   * box pinned to T that holds no credential for T, "Sign in to re-pair" can
   * loop forever; switching the pin is the other remedy. With one candidate
   * there is nothing to switch to, so no second button that also cannot act.
   */
  showSwitchTenant: boolean;
}

/** Label of the tenant-switch action. */
export const SWITCH_TENANT_LABEL = "Switch active tenant";

/**
 * The causes a PINNED tenant's posture can carry — `absent` (no credential
 * serves it) and `upstream_401` (`dark`: coord keeps rejecting it). Only these
 * name the tenant in the body and may offer the switch.
 */
const TENANT_NAMING_CAUSES: string[] = ["absent", "upstream_401"];

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
  re_pair: "Sign in to re-pair",
  retry_refresh: "Retry refresh now",
};

/**
 * Which command each CTA invokes. A `Record` so a new CTA cannot compile
 * without an explicit action. See {@link CredentialDarkPresentation} for why
 * `re_pair` goes to the sign-in rather than the refresher.
 */
const CREDENTIAL_DARK_CTA_ACTIONS: Record<
  CredentialDarkCta,
  NonNullable<CredentialDarkPresentation["ctaAction"]>
> = {
  sign_in: "cognito_sign_in",
  re_pair: "cognito_sign_in",
  retry_refresh: "kick_refresher",
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
    // A build that predates the source had exactly ONE emitter — the Cognito
    // refresh loop — so that is the honest default, not a third bucket.
    source: typeof r.source === "string" && r.source.length > 0 ? r.source : DARK_SOURCE_COGNITO,
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
    tenantId: nonEmptyString(r.tenant_id),
    pinnedTenant: r.pinned_tenant === true,
  };
}

/** A non-empty string, or `null` for anything else (absent, `null`, wrong type). */
function nonEmptyString(v: unknown): string | null {
  return typeof v === "string" && v.length > 0 ? v : null;
}

/**
 * The postures whose credential can still answer coord — the frontend twin of
 * `CoordCredentialPosture::can_answer()`. Used only as the fallback when a
 * payload carries no `canAnswer` key (N3).
 */
const ANSWERING_POSTURES: string[] = ["live", "expiring"];

/**
 * Turn the `get_coord_credential_posture` command's answer into a
 * {@link CredentialDarkSignal}.
 *
 * M4 — the posture reached the UI only as a Tauri event, and `emit` has no
 * replay. The BOOT publish happens before the React tree has registered its
 * `listen()`, so the one case the posture exists for — a runner that came up
 * holding a dead credential — landed on no listener at all; and after a
 * webview reload nothing re-fired, because the posture had not CHANGED.
 * Reading it on mount is what closes both.
 *
 * `null` in means UNKNOWN (no refresher pass has concluded). UNKNOWN is not
 * health, but there is nothing to SHOW for it, so it contributes no signal —
 * it must never be rendered as a recovery.
 */
export function credentialDarkFromPostureSnapshot(raw: unknown): CredentialDarkSignal | null {
  if (raw === null || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const posture =
    typeof r.posture === "string" ? r.posture : typeof r.state === "string" ? r.state : null;
  if (posture === null || posture === "unknown") return null;
  // N3 — `canAnswer` decides, but only when it IS a boolean. Treating a
  // MISSING key as "can answer" meant a payload carrying `posture: "expired"`
  // and no `canAnswer` would CLEAR the banner. Unreachable from today's
  // `to_json()`, which always emits the key — but the posture string is the
  // same fact, so derive it rather than defaulting to health.
  const canAnswer =
    typeof r.canAnswer === "boolean" ? r.canAnswer : ANSWERING_POSTURES.includes(posture);
  if (canAnswer) {
    return {
      source: DARK_SOURCE_POSTURE,
      dark: false,
      cause: "recovered",
      message: typeof r.reason === "string" ? r.reason : "",
      cta: null,
      since: typeof r.since === "number" && Number.isFinite(r.since) ? r.since : null,
      tenantId: nonEmptyString(r.tenantId),
      pinnedTenant: false,
    };
  }
  const cta =
    r.cta === "sign_in" || r.cta === "re_pair" || r.cta === "retry_refresh" ? r.cta : null;
  return {
    source: DARK_SOURCE_POSTURE,
    dark: true,
    cause: typeof r.cause === "string" && r.cause.length > 0 ? r.cause : posture,
    message: typeof r.reason === "string" ? r.reason : "",
    cta,
    since: typeof r.since === "number" && Number.isFinite(r.since) ? r.since : null,
    tenantId: nonEmptyString(r.tenantId),
    pinnedTenant: r.pinnedTenant === true,
  };
}

/** Command name the mount-time posture read invokes. */
export const GET_COORD_CREDENTIAL_POSTURE_CMD = "get_coord_credential_posture";

/**
 * What the credential banner renders.
 *
 * `showSwitcher` is `useTenant().showSwitcher`. For a tenant-naming
 * `absent`/`dark` signal the body always NAMES the tenant: the runner's own
 * sentence does (the pinned arm composes one naming the tenant and the cause),
 * and a signal whose sentence does not — an older runner build — gets the
 * tenant appended rather than the generic "no coord credential", which is false
 * on a box holding a live credential for another tenant. A signal with no
 * `tenantId` keeps today's copy and never offers the switch.
 */
export function credentialDarkPresentation(
  signal: CredentialDarkSignal,
  formatTime: (since: number) => string = formatSince,
  showSwitcher = false,
): CredentialDarkPresentation {
  const namesTenant = signal.tenantId !== null && TENANT_NAMING_CAUSES.includes(signal.cause);
  const title =
    namesTenant && signal.cause === "absent"
      ? `No usable coord credential for tenant ${signal.tenantId}`
      : (CREDENTIAL_DARK_TITLES[signal.cause] ?? "Coord credential problem");
  const prefix =
    typeof signal.since === "number" && Number.isFinite(signal.since)
      ? `Since ${formatTime(signal.since)} — `
      : "";
  const message =
    namesTenant && signal.tenantId !== null && !signal.message.includes(signal.tenantId)
      ? `${signal.message} (tenant ${signal.tenantId})`
      : signal.message;
  const body = `${prefix}${message}`;
  const ctaAction = signal.cta === null ? null : CREDENTIAL_DARK_CTA_ACTIONS[signal.cta];
  return {
    title,
    body,
    ctaLabel: signal.cta === null ? null : CREDENTIAL_DARK_CTA_LABELS[signal.cta],
    ctaAction,
    showSwitchTenant: namesTenant && signal.pinnedTenant && showSwitcher,
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

/**
 * Build the banner's "switch to this tenant" handler: re-pin through the
 * EXISTING setter (TenantContext -> `set_active_tenant`, an explicit operator
 * pick), report the re-pin as done (`onPinned`) the moment it resolves, then
 * kick the refresher so the posture is re-derived against the new pin now
 * rather than at the next cadence.
 *
 * It deliberately does NOT re-read the posture: the kick is fire-and-forget,
 * so an immediate read returns the PRE-switch status, and because IPC replies
 * are not ordered against events it can land after — and overwrite — the
 * kicked pass's own event. The banner is updated by that event instead (a
 * same-posture tenant change is re-announced as a detail transition).
 * `onPinned` runs before the kick so a kick failure is never shown as a
 * failed switch. Extracted so the order is unit-testable in node.
 */
export function makeSwitchTenantHandler(deps: {
  setDefaultTenant: (tenantId: string) => Promise<void>;
  invoker: (cmd: string, args: Record<string, unknown>) => Promise<unknown>;
  onPinned: () => void;
}): (tenantId: string) => Promise<void> {
  return async (tenantId: string) => {
    await deps.setDefaultTenant(tenantId);
    deps.onPinned();
    await deps.invoker(KICK_DEVICE_JWT_REFRESHER_CMD, {});
  };
}
