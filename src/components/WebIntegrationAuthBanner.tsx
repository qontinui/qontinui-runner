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
 * The Authorize button invokes the existing `start_web_token_flow` Tauri
 * command, which opens the user's browser to the web app's `/connect-runner`
 * page and feeds the resulting token back via the runner's HTTP callback.
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
  shouldShowAuthBanner,
  statusSignature,
  type AuthBannerStatus,
} from "./web-integration-banner-logic";

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

/** Session-storage key used for dismissal persistence across re-renders. */
const DISMISS_STORAGE_KEY = "qontinui:web-integration-auth-banner:dismissed-signature";

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
  const [dismissedSignature, setDismissedSignature] = useState<string | null>(() =>
    readDismissedSignature(),
  );
  const [authorizing, setAuthorizing] = useState(false);
  const [authorizeError, setAuthorizeError] = useState<string | null>(null);

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

  // Subscribe-style effect: kick off the initial fetch AND register a
  // listener for the runner's `web-integration-changed` event. setState is
  // only called inside the async resolution / event callback, satisfying
  // react-hooks/set-state-in-effect (which forbids synchronous setState in
  // an effect body).
  useEffect(() => {
    let cancelled = false;

    const fetchStatus = () => {
      invoke<AuthBannerStatus>("get_web_integration_status")
        .then((next) => {
          if (!cancelled) setStatus(next);
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

  const handleAuthorize = useCallback(async () => {
    setAuthorizing(true);
    setAuthorizeError(null);
    try {
      await invoke<void>("start_web_token_flow", {});
      // Browser opens; token will land via callback and emit
      // `web-integration-changed`, which refreshes status above.
    } catch (err) {
      setAuthorizeError(String(err));
    } finally {
      setAuthorizing(false);
    }
  }, []);

  const handleDismiss = useCallback(() => {
    const sig = statusSignature(status);
    writeDismissedSignature(sig);
    setDismissedSignature(sig);
  }, [status]);

  if (tier !== "qontinui_account") return null;

  const visible = shouldShowAuthBanner(status, dismissedSignature);
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
              <span style={{ color: "var(--destructive, #ef4444)" }}>{authorizeError}</span>
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
