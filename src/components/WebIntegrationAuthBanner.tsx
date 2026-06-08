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

import {
  makeRePairClickHandler,
  RE_PAIR_CTA_GRACE_MS,
  shouldShowAuthBanner,
  shouldShowRePairCta,
  statusSignature,
  type AuthBannerStatus,
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
      window.dispatchEvent(new CustomEvent("runner-tier-changed"));
    } catch (err) {
      setAuthorizeError(String(err));
    } finally {
      setAuthorizing(false);
    }
  }, [backendUrl]);

  const handleDismiss = useCallback(() => {
    // `deviceJwtPresent !== false` treats the not-yet-loaded (null) state as
    // paired so a paired runner never flashes the banner while the
    // device_jwt_present probe is in flight.
    const sig = statusSignature(status, deviceJwtPresent !== false);
    writeDismissedSignature(sig);
    setDismissedSignature(sig);
  }, [status, deviceJwtPresent]);

  if (tier !== "qontinui_account") return null;

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

  const visible = shouldShowAuthBanner(status, deviceJwtPresent !== false, dismissedSignature);
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
