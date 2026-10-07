/**
 * WebIntegrationAuthBanner.tsx
 *
 * Top-of-app banner that appears when this runner is enabled for the
 * qontinui-web backend but has no runner token yet — i.e. a fresh install
 * where the user hasn't completed the OAuth-style web-login flow.
 *
 * Visibility rule (see {@link shouldShowAuthBanner}):
 *   - Hide when status hasn't loaded yet (avoid flashing on every render).
 *   - Hide when `enabled === false` — user opted out, respect it.
 *   - Hide when `runnerTokenMasked` is non-empty — token is set, no action needed.
 *   - Hide when the user dismissed it for the session AND status hasn't changed
 *     since dismissal (re-shows on reload, on token clear, or on a new
 *     registration error).
 *   - SHOW unconditionally while the runner is credential-dark, with no
 *     dismiss affordance: every session spawned in that state has no coord
 *     access, and a dismissed banner is a silent runner again.
 *
 * The Authorize button invokes the `cognito_sign_in` Tauri command — the
 * canonical one-click connect path. It opens the system browser for Cognito
 * Hosted-UI sign-in (RFC 8252 PKCE), then pairs this device server-side
 * (minting a device JWT) and promotes the runner to Tier 2. On success it
 * emits `web-integration-changed`, which refreshes the status + device-JWT
 * probe below and dismisses the banner. (The legacy `start_web_token_flow`
 * browser-token flow it used to call was removed — it was broken and
 * redundant with Cognito sign-in.)
 *
 * Status source: invokes `get_web_integration_status` and refreshes on the
 * `web-integration-changed` Tauri event (emitted by save + token-flow).
 *
 * UI Bridge: registers the banner root and both buttons via `useUIElement`
 * so the integration showcase can find and interact with them via id.
 */

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useUIElement } from "@qontinui/ui-bridge";
import { AlertCircle, X } from "lucide-react";

import { useRunnerTier } from "@/hooks/useRunnerTier";
import { useTenant } from "@/contexts/TenantContext";

import {
  applyCredentialDarkSignal,
  credentialDarkFromPostureSnapshot,
  credentialDarkPresentation,
  effectiveCredentialDark,
  GET_COORD_CREDENTIAL_POSTURE_CMD,
  makeRePairClickHandler,
  makeRetryRefreshHandler,
  makeSwitchTenantHandler,
  normalizeCredentialDarkSignal,
  RE_PAIR_CTA_GRACE_MS,
  retryErrorSurvives,
  shouldShowAuthBanner,
  shouldShowRePairCta,
  statusSignature,
  SWITCH_TENANT_LABEL,
  type AuthBannerStatus,
  type CredentialDarkSignal,
} from "./web-integration-banner-logic";

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

/** Session-storage key used for dismissal persistence across re-renders. */
const DISMISS_STORAGE_KEY = "qontinui:web-integration-auth-banner:dismissed-signature";

/** Fallback backend URL when status hasn't loaded yet. */
const DEFAULT_BACKEND_URL = "https://api.qontinui.io";

function readDismissedSignature(): string | null {
  try {
    return sessionStorage.getItem(DISMISS_STORAGE_KEY);
  } catch {
    // sessionStorage may throw in some webview environments — treat as
    // "no dismissal recorded".
    return null;
  }
}

function writeDismissedSignature(signature: string): void {
  try {
    sessionStorage.setItem(DISMISS_STORAGE_KEY, signature);
  } catch {
    // Best-effort — losing dismissal across reloads is acceptable.
  }
}

export function WebIntegrationAuthBanner() {
  // Runner-tier-decoupling Phase 1 — the qontinui-web banner is only
  // relevant when this runner is configured for the qontinui-account tier.
  // Tier 0 ("local") and Tier 1 ("local_provider") never integrate with
  // qontinui-web, so we never surface the "Connect this runner to your
  // account" prompt for them.
  const { tier } = useRunnerTier();

  const [status, setStatus] = useState<AuthBannerStatus | null>(null);
  // Backend URL the Cognito-bound device JWT is minted against. Captured from
  // the same `get_web_integration_status` fetch that feeds the banner; falls
  // back to the production API host when the status hasn't loaded or carries no
  // backendUrl (the Rust command then derives the web origin itself).
  const [backendUrl, setBackendUrl] = useState<string>(DEFAULT_BACKEND_URL);
  const [dismissedSignature, setDismissedSignature] = useState<string | null>(() =>
    readDismissedSignature(),
  );
  const [authorizing, setAuthorizing] = useState(false);
  const [authorizeError, setAuthorizeError] = useState<string | null>(null);

  // Phase 4 (unified-devices migration): post-upgrade re-pair CTA state.
  //
  // `deviceJwtPresent` mirrors the `device_jwt_present` Tauri command
  // result. `firstDetectedAt` is the wall-clock ms at which we first
  // observed the migration state (Tier 2 + runner_token + !deviceJwt).
  // Both are *component* state, not setting state — this is a transient
  // UI signal that resets on every mount.
  //
  // `nowTick` is the ms timestamp the visibility predicate compares
  // `firstDetectedAt` against. We bump it once via setTimeout after the
  // grace period so React re-renders and reveals the CTA without polling.
  const [deviceJwtPresent, setDeviceJwtPresent] = useState<boolean | null>(null);
  const [firstDetectedAt, setFirstDetectedAt] = useState<number | null>(null);
  const [nowTick, setNowTick] = useState<number>(() => Date.now());
  const [reKicking, setReKicking] = useState(false);
  const [reKickError, setReKickError] = useState<string | null>(null);
  // The credential banner's "Retry refresh now" result: its inline error and
  // the posture signal it was written about. Held apart from `reKickError`
  // because a concluded `unrefreshable` swaps the CTA to the sign-in, and the
  // error must stay visible beside the new button — but only until a posture
  // signal that says something else arrives (`retryErrorSurvives`).
  const [retryRefresh, setRetryRefresh] = useState<{
    error: string;
    signal: CredentialDarkSignal | null;
  } | null>(null);
  const retryRefreshError = retryRefresh?.error ?? null;

  // Credential-dark signal. The device-JWT refresher emits
  // `autonomy-credential-dark` with `{dark, cause, message, cta, since}` for
  // EVERY terminal credential cause — a HARD Cognito refresh failure
  // (`cognito_hard`, the original arm) and, since the coord-credential-posture
  // plan, every non-answering coord-credential posture: `expired`, `absent`,
  // `unrefreshable`, `upstream_401`. It fires at BOOT too, which is the moment
  // a runner that restored a dead credential used to be silent. `dark:false`
  // clears it on recovery.
  //
  // M5 — held PER SOURCE. The Cognito loop and the coord-credential posture
  // are two independent authorities on one event; in a single slot they
  // last-writer-win, and the losing write is routinely a RECOVERY: Cognito
  // goes dark, the user signs in, Cognito fires `dark:false`, the banner
  // clears — while the coord slot is still `unrefreshable` and the posture arm
  // will not re-fire, because the posture did not change.
  const [credentialDarkBySource, setCredentialDarkBySource] = useState<
    Record<string, CredentialDarkSignal>
  >({});
  const credentialDark = effectiveCredentialDark(credentialDarkBySource);

  // Plan 2026-09-14-credential-posture-third-residuals Phase 3: the tenant
  // switch for a pinned tenant this runner holds no credential for. It is the
  // EXISTING re-pin (`set_active_tenant`, via TenantContext) behind an explicit
  // operator pick from this device's bound tenants — never a guessed tenant.
  // Offered only when `showSwitcher` (more than one binding), so a
  // single-tenant box never sees a second button that could not act.
  const {
    showSwitcher,
    candidates: tenantCandidates,
    defaultTenantIdForNewSessions,
    setDefaultTenantForNewSessions,
  } = useTenant();
  const [tenantSwitcherOpen, setTenantSwitcherOpen] = useState(false);
  const [switchingTenant, setSwitchingTenant] = useState(false);
  const [switchTenantError, setSwitchTenantError] = useState<string | null>(null);

  const { ref: rootRef } = useUIElement({
    id: "web-integration-banner",
    label: "Web integration authorization banner",
    type: "generic",
  });
  const { ref: authorizeButtonRef } = useUIElement({
    id: "web-integration-banner-authorize",
    label: "Authorize this runner with qontinui-web",
    type: "button",
  });
  const { ref: dismissButtonRef } = useUIElement({
    id: "web-integration-banner-dismiss",
    label: "Dismiss web integration authorization banner",
    type: "button",
  });
  const { ref: rePairButtonRef } = useUIElement({
    id: "web-integration-banner-re-pair",
    label: "Retry device re-pair manually",
    type: "button",
  });
  const { ref: switchTenantButtonRef } = useUIElement({
    id: "web-integration-banner-switch-tenant",
    label: "Switch this runner's active tenant",
    type: "button",
  });

  // Subscribe-style effect: kick off the initial fetch AND register a
  // listener for the runner's `web-integration-changed` event. setState is
  // only called inside the async resolution / event callback, satisfying
  // react-hooks/set-state-in-effect (which forbids synchronous setState in
  // an effect body).
  useEffect(() => {
    let cancelled = false;

    const fetchStatus = () => {
      invoke<AuthBannerStatus & { backendUrl?: string }>("get_web_integration_status")
        .then((next) => {
          if (cancelled) return;
          setStatus(next);
          const url = next.backendUrl?.trim();
          if (url) setBackendUrl(url);
        })
        .catch(() => {
          // If the command isn't available (e.g. older runner build), keep
          // the banner hidden rather than crashing — `status` stays null.
        });
    };

    fetchStatus();
    const unlisten = listen("web-integration-changed", fetchStatus);

    return () => {
      cancelled = true;
      unlisten
        .then((fn) => fn())
        .catch(() => {
          /* listener cleanup is best-effort */
        });
    };
  }, []);

  // Phase 4 (unified-devices migration): fetch `device_jwt_present` on
  // mount. The refresher may flip this from false → true under us; the
  // simplest signal we own is a re-fetch on `web-integration-changed`
  // (emitted by the pair-cli wrapper) so we piggyback the same listener
  // shape used above. setState is only called inside the async resolution.
  useEffect(() => {
    let cancelled = false;

    const fetchJwt = () => {
      invoke<boolean>("device_jwt_present")
        .then((present) => {
          if (!cancelled) setDeviceJwtPresent(present);
        })
        .catch(() => {
          // Older runner build (pre-Phase 4) won't have this command.
          // Treat the absence as "we can't tell" — never surface the CTA.
          if (!cancelled) setDeviceJwtPresent(true);
        });
    };

    fetchJwt();
    const unlisten = listen("web-integration-changed", fetchJwt);

    return () => {
      cancelled = true;
      unlisten
        .then((fn) => fn())
        .catch(() => {
          /* listener cleanup is best-effort */
        });
    };
  }, []);

  // Phase 2 (terminal-autonomy-survives-logout): subscribe to the refresher's
  // `autonomy-credential-dark` event. `dark:true` reveals the credential-dark
  // banner; `dark:false` (autonomy resumed) clears it. setState is only called
  // inside the event callback, satisfying react-hooks/set-state-in-effect.
  useEffect(() => {
    let cancelled = false;
    const unlisten = listen<unknown>("autonomy-credential-dark", (event) => {
      // Normalised, because this event comes from the RUNNER BINARY and the
      // frontend may be running against a build that predates the widened
      // payload. An unparseable payload is dropped rather than rendered as a
      // blank banner.
      const signal = normalizeCredentialDarkSignal(event.payload);
      if (!cancelled && signal !== null) {
        setCredentialDarkBySource((prev) => applyCredentialDarkSignal(prev, signal));
        setRetryRefresh((prev) => (prev && retryErrorSurvives(prev.signal, signal) ? prev : null));
      }
    });
    return () => {
      cancelled = true;
      unlisten
        .then((fn) => fn())
        .catch(() => {
          /* listener cleanup is best-effort */
        });
    };
  }, []);

  // M4 — READ the coord-credential posture on mount.
  //
  // Without this the posture reached the UI only as a Tauri event, and
  // `emit` has no replay: the BOOT publish happens before this tree has
  // registered its `listen()` above, so the one case the posture exists for —
  // a runner that came up holding a dead credential — landed on no listener at
  // all. After a webview reload nothing re-fired either, because the posture
  // had not CHANGED. Re-read on `web-integration-changed` too, piggybacking
  // the same listener shape the two fetches above use.
  useEffect(() => {
    let cancelled = false;

    const fetchPosture = () => {
      invoke<unknown>(GET_COORD_CREDENTIAL_POSTURE_CMD)
        .then((raw) => {
          if (cancelled) return;
          const signal = credentialDarkFromPostureSnapshot(raw);
          // `null` is UNKNOWN — no refresher pass has concluded. It is not
          // health, and it is not a recovery: contribute nothing.
          if (signal !== null) {
            setCredentialDarkBySource((prev) => applyCredentialDarkSignal(prev, signal));
            setRetryRefresh((prev) =>
              prev && retryErrorSurvives(prev.signal, signal) ? prev : null,
            );
          }
        })
        .catch(() => {
          // Older runner build without the command — we cannot tell, so we
          // say nothing rather than clearing a banner an event may have set.
        });
    };

    fetchPosture();
    const unlisten = listen("web-integration-changed", fetchPosture);

    return () => {
      cancelled = true;
      unlisten
        .then((fn) => fn())
        .catch(() => {
          /* listener cleanup is best-effort */
        });
    };
  }, []);

  // Phase 4 — record the first-detected-at timestamp when the migration
  // state first qualifies, and schedule a one-shot re-render at
  // detectedAt + RE_PAIR_CTA_GRACE_MS so the CTA reveal isn't gated on
  // unrelated re-renders. We don't *poll* — we just nudge `nowTick`
  // once the grace deadline arrives.
  const migrationStateQualifies =
    tier === "qontinui_account" &&
    status !== null &&
    status.runnerTokenMasked.length > 0 &&
    deviceJwtPresent === false;

  useEffect(() => {
    if (!migrationStateQualifies) {
      // State no longer qualifies (e.g. JWT arrived) — drop the timer.
      setFirstDetectedAt(null);
      return;
    }
    if (firstDetectedAt !== null) return;

    const detectedAt = Date.now();
    setFirstDetectedAt(detectedAt);
    const handle = setTimeout(() => {
      setNowTick(Date.now());
    }, RE_PAIR_CTA_GRACE_MS);

    return () => {
      clearTimeout(handle);
    };
  }, [migrationStateQualifies, firstDetectedAt]);

  const handleRePair = useCallback(async () => {
    setReKicking(true);
    setReKickError(null);
    try {
      // Delegated to a pure factory so the click → invoke contract can be
      // unit-tested in node (no jsdom). Refresher will fire its `Pair`
      // path on next tick; the resulting `web-integration-changed` event
      // refreshes `deviceJwtPresent` above.
      const click = makeRePairClickHandler((cmd, args) => invoke<void>(cmd, args));
      await click();
    } catch (err) {
      setReKickError(String(err));
    } finally {
      setReKicking(false);
    }
  }, []);

  const handleAuthorize = useCallback(async () => {
    setAuthorizing(true);
    setAuthorizeError(null);
    try {
      // Canonical one-click connect: open the system browser for Cognito
      // Hosted-UI sign-in. The Rust command pairs this device (minting a
      // device JWT) and promotes the runner to Tier 2, then emits
      // `web-integration-changed`, which refreshes the status + device-JWT
      // probe above and hides the banner.
      await invoke<void>("cognito_sign_in", { backendUrl });
      // A retry's "did not recover" is answered by this sign-in.
      setRetryRefresh(null);
      window.dispatchEvent(new CustomEvent("runner-tier-changed"));
    } catch (err) {
      setAuthorizeError(String(err));
    } finally {
      setAuthorizing(false);
    }
  }, [backendUrl]);

  // The operator picked `tenantId` in the switcher. Re-pin (machine.json,
  // future sessions only — running sessions keep their tenant), close the
  // switcher as soon as the re-pin lands, then kick the refresher so the
  // posture is re-derived against the new pin now. The banner updates from the
  // kicked pass's own event, never from an immediate re-read (see
  // `makeSwitchTenantHandler`).
  const handleSwitchTenant = useCallback(
    async (tenantId: string) => {
      setSwitchingTenant(true);
      setSwitchTenantError(null);
      let pinned = false;
      try {
        await makeSwitchTenantHandler({
          setDefaultTenant: setDefaultTenantForNewSessions,
          invoker: (cmd, args) => invoke<unknown>(cmd, args),
          onPinned: () => {
            pinned = true;
            setTenantSwitcherOpen(false);
          },
        })(tenantId);
      } catch (err) {
        // After a successful re-pin only the refresher kick can have failed:
        // say the switch landed, so the error never reads as a failed switch.
        setSwitchTenantError(
          pinned
            ? `Switched to tenant ${tenantId}; the credential refresh kick failed (${String(err)}). The banner updates at the next refresh.`
            : String(err),
        );
      } finally {
        setSwitchingTenant(false);
      }
    },
    [setDefaultTenantForNewSessions],
  );

  const handleDismiss = useCallback(() => {
    // `deviceJwtPresent !== false` treats the not-yet-loaded (null) state as
    // paired so a paired runner never flashes the banner while the
    // device_jwt_present probe is in flight. The credential posture is part of
    // the signature, so a later posture transition resurfaces the banner.
    const sig = statusSignature(status, deviceJwtPresent !== false, credentialDark);
    writeDismissedSignature(sig);
    setDismissedSignature(sig);
  }, [status, deviceJwtPresent, credentialDark]);

  // "Retry refresh now": kick the refresher, WAIT for the pass it triggers,
  // and render from the posture that pass left behind — so a retry that could
  // not recover the credential says so (and an `unrefreshable` verdict swaps
  // the button to the sign-in) instead of silently changing nothing.
  const handleRetryRefresh = useCallback(async () => {
    setReKicking(true);
    setRetryRefresh(null);
    try {
      const retry = makeRetryRefreshHandler((cmd, args) => invoke<unknown>(cmd, args));
      const result = await retry();
      const signal = result.signal;
      if (signal !== null) {
        setCredentialDarkBySource((prev) => applyCredentialDarkSignal(prev, signal));
      }
      setRetryRefresh(result.error === null ? null : { error: result.error, signal });
    } catch (err) {
      setRetryRefresh({ error: String(err), signal: null });
    } finally {
      setReKicking(false);
    }
  }, []);

  // The credential banner's CTA. Both destinations are commands that already
  // exist: an interactive Cognito re-login, or the headless refresher kick.
  const handleCredentialCta = useCallback(
    async (action: "cognito_sign_in" | "kick_refresher") => {
      if (action === "cognito_sign_in") {
        await handleAuthorize();
        return;
      }
      await handleRetryRefresh();
    },
    [handleAuthorize, handleRetryRefresh],
  );

  if (tier !== "qontinui_account") return null;

  // The credential banner takes TOP precedence. While the runner's credential
  // is dark — a dead Cognito refresh token, an expired/absent/unrefreshable
  // coord device JWT, or a coord that keeps rejecting it — every session this
  // runner spawns works WITHOUT coord and does not know it. That is the whole
  // defect this banner closes, so it is not dismissable and it renders the
  // CAUSE rather than one generic sentence.
  if (credentialDark?.dark) {
    const presentation = credentialDarkPresentation(credentialDark, undefined, showSwitcher);
    const busy = presentation.ctaAction === "cognito_sign_in" ? authorizing : reKicking;
    const ctaError =
      (presentation.ctaAction === "cognito_sign_in" ? authorizeError : reKickError) ??
      retryRefreshError ??
      (presentation.showSwitchTenant ? switchTenantError : null);
    return (
      <div
        ref={rootRef}
        role="alert"
        aria-live="assertive"
        style={{
          position: "fixed",
          top: 16,
          left: "50%",
          transform: "translateX(-50%)",
          zIndex: 9999,
          display: "flex",
          alignItems: "center",
          gap: 12,
          padding: "10px 14px",
          background: "var(--bg-tertiary, #242837)",
          color: "var(--text-primary, #e4e4e7)",
          border: "1px solid hsl(var(--destructive, 0 84% 60%))",
          borderRadius: 8,
          boxShadow: "0 4px 16px rgba(0, 0, 0, 0.35)",
          fontSize: "0.875rem",
          maxWidth: "min(640px, calc(100vw - 32px))",
        }}
      >
        <AlertCircle
          className="w-4 h-4 shrink-0"
          style={{ color: "hsl(var(--destructive, 0 84% 60%))" }}
          aria-hidden="true"
        />
        <div style={{ display: "flex", flexDirection: "column", gap: 2, minWidth: 0 }}>
          <div style={{ fontWeight: 600 }}>{presentation.title}</div>
          <div style={{ fontSize: "0.8125rem", opacity: 0.85 }}>
            {presentation.body}
            {ctaError ? (
              <>
                <br />
                <span style={{ color: "hsl(var(--destructive, 0 84% 60%))" }}>{ctaError}</span>
              </>
            ) : null}
          </div>
          {presentation.showSwitchTenant && tenantSwitcherOpen ? (
            <div
              role="group"
              aria-label="Choose this runner's active tenant"
              style={{ display: "flex", flexWrap: "wrap", gap: 6, marginTop: 6 }}
            >
              {tenantCandidates.map((candidate) => {
                const current = candidate === defaultTenantIdForNewSessions;
                return (
                  <button
                    key={candidate}
                    type="button"
                    title={candidate}
                    disabled={switchingTenant || current}
                    onClick={() => {
                      void handleSwitchTenant(candidate);
                    }}
                    style={{
                      background: "transparent",
                      color: "inherit",
                      border: "1px solid var(--border, #3f3f46)",
                      borderRadius: 4,
                      padding: "2px 8px",
                      fontSize: "0.75rem",
                      fontFamily: "monospace",
                      cursor: switchingTenant || current ? "default" : "pointer",
                      opacity: current ? 0.6 : 1,
                    }}
                  >
                    {current ? `${candidate} (current)` : candidate}
                  </button>
                );
              })}
            </div>
          ) : null}
        </div>
        {presentation.ctaAction ? (
          <button
            ref={
              presentation.ctaAction === "cognito_sign_in" ? authorizeButtonRef : rePairButtonRef
            }
            type="button"
            onClick={() => {
              void handleCredentialCta(presentation.ctaAction!);
            }}
            disabled={busy}
            style={{
              background: "var(--accent, #6366f1)",
              color: "#fff",
              border: "none",
              borderRadius: 4,
              padding: "4px 10px",
              fontSize: "0.8125rem",
              fontWeight: 600,
              cursor: busy ? "wait" : "pointer",
              opacity: busy ? 0.7 : 1,
              whiteSpace: "nowrap",
            }}
          >
            {busy ? "Working…" : presentation.ctaLabel}
          </button>
        ) : null}
        {presentation.showSwitchTenant ? (
          <button
            ref={switchTenantButtonRef}
            type="button"
            aria-expanded={tenantSwitcherOpen}
            onClick={() => setTenantSwitcherOpen((open) => !open)}
            disabled={switchingTenant}
            style={{
              background: "transparent",
              color: "inherit",
              border: "1px solid var(--accent, #6366f1)",
              borderRadius: 4,
              padding: "4px 10px",
              fontSize: "0.8125rem",
              fontWeight: 600,
              cursor: switchingTenant ? "wait" : "pointer",
              whiteSpace: "nowrap",
            }}
          >
            {switchingTenant ? "Switching…" : SWITCH_TENANT_LABEL}
          </button>
        ) : null}
      </div>
    );
  }

  // Phase 4 (unified-devices migration): re-pair CTA takes precedence over
  // the "needs first pair" banner. When this runner WAS paired pre-upgrade
  // but is missing a device-JWT, surface a distinct CTA that kicks the
  // refresher rather than re-opening the OAuth flow. The 5-min grace
  // covers the refresher's normal tick — only surface if it hasn't healed.
  const showRePair = shouldShowRePairCta({
    tier,
    runnerTokenPresent: (status?.runnerTokenMasked.length ?? 0) > 0,
    deviceJwtPresent: deviceJwtPresent ?? true,
    firstDetectedAt,
    now: nowTick,
  });

  if (showRePair) {
    return (
      <div
        ref={rootRef}
        role="status"
        aria-live="polite"
        style={{
          position: "fixed",
          top: 16,
          left: "50%",
          transform: "translateX(-50%)",
          zIndex: 9999,
          display: "flex",
          alignItems: "center",
          gap: 12,
          padding: "10px 14px",
          background: "var(--bg-tertiary, #242837)",
          color: "var(--text-primary, #e4e4e7)",
          border: "1px solid var(--accent, #6366f1)",
          borderRadius: 8,
          boxShadow: "0 4px 16px rgba(0, 0, 0, 0.35)",
          fontSize: "0.875rem",
          maxWidth: "min(640px, calc(100vw - 32px))",
        }}
      >
        <AlertCircle
          className="w-4 h-4 shrink-0"
          style={{ color: "var(--accent, #6366f1)" }}
          aria-hidden="true"
        />
        <div style={{ display: "flex", flexDirection: "column", gap: 2, minWidth: 0 }}>
          <div style={{ fontWeight: 600 }}>Re-pair this runner with Qontinui.</div>
          <div style={{ fontSize: "0.8125rem", opacity: 0.85 }}>
            Your runner needs a fresh device credential after the upgrade. The runner will pair
            automatically — if this banner stays visible, click here to retry manually.
            {reKickError ? (
              <>
                <br />
                <span style={{ color: "hsl(var(--destructive, 0 84% 60%))" }}>{reKickError}</span>
              </>
            ) : null}
          </div>
        </div>
        <button
          ref={rePairButtonRef}
          type="button"
          onClick={handleRePair}
          disabled={reKicking}
          style={{
            background: "var(--accent, #6366f1)",
            color: "#fff",
            border: "none",
            borderRadius: 4,
            padding: "4px 10px",
            fontSize: "0.8125rem",
            fontWeight: 600,
            cursor: reKicking ? "wait" : "pointer",
            opacity: reKicking ? 0.7 : 1,
            whiteSpace: "nowrap",
          }}
        >
          {reKicking ? "Retrying…" : "Retry"}
        </button>
      </div>
    );
  }

  const visible = shouldShowAuthBanner(
    status,
    deviceJwtPresent !== false,
    dismissedSignature,
    credentialDark,
  );
  if (!visible) return null;

  return (
    <div
      ref={rootRef}
      role="status"
      aria-live="polite"
      style={{
        position: "fixed",
        top: 16,
        left: "50%",
        transform: "translateX(-50%)",
        zIndex: 9999,
        display: "flex",
        alignItems: "center",
        gap: 12,
        padding: "10px 14px",
        background: "var(--bg-tertiary, #242837)",
        color: "var(--text-primary, #e4e4e7)",
        border: "1px solid var(--accent, #6366f1)",
        borderRadius: 8,
        boxShadow: "0 4px 16px rgba(0, 0, 0, 0.35)",
        fontSize: "0.875rem",
        maxWidth: "min(640px, calc(100vw - 32px))",
      }}
    >
      <AlertCircle
        className="w-4 h-4 shrink-0"
        style={{ color: "var(--accent, #6366f1)" }}
        aria-hidden="true"
      />
      <div style={{ display: "flex", flexDirection: "column", gap: 2, minWidth: 0 }}>
        <div style={{ fontWeight: 600 }}>Connect this runner to your account</div>
        <div style={{ fontSize: "0.8125rem", opacity: 0.85 }}>
          Authorize this runner with qontinui-web to make it visible to your account.
          {authorizeError ? (
            <>
              <br />
              <span style={{ color: "hsl(var(--destructive, 0 84% 60%))" }}>{authorizeError}</span>
            </>
          ) : null}
        </div>
      </div>
      <button
        ref={authorizeButtonRef}
        type="button"
        onClick={handleAuthorize}
        disabled={authorizing}
        style={{
          background: "var(--accent, #6366f1)",
          color: "#fff",
          border: "none",
          borderRadius: 4,
          padding: "4px 10px",
          fontSize: "0.8125rem",
          fontWeight: 600,
          cursor: authorizing ? "wait" : "pointer",
          opacity: authorizing ? 0.7 : 1,
          whiteSpace: "nowrap",
        }}
      >
        {authorizing ? "Opening…" : "Authorize"}
      </button>
      <button
        ref={dismissButtonRef}
        type="button"
        onClick={handleDismiss}
        aria-label="Dismiss"
        style={{
          background: "transparent",
          color: "var(--text-primary, #e4e4e7)",
          border: "none",
          borderRadius: 4,
          padding: "4px",
          cursor: "pointer",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          opacity: 0.7,
        }}
      >
        <X className="w-4 h-4" aria-hidden="true" />
      </button>
    </div>
  );
}
