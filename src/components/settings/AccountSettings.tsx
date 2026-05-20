/**
 * AccountSettings.tsx
 *
 * Settings sub-tab for the Qontinui account binding.
 *
 *   - Tier 0/1: shows a "Sign in to Qontinui" button that opens the
 *     browser-side `/connect-runner` flow. The actual tier promotion to
 *     Tier 2 happens server-side in `mcp::auth_callback` when the user
 *     confirms in the browser — the runner re-syncs via the existing
 *     `web-integration-changed` event (emitted by
 *     `apply_web_integration_settings`) which AuthProvider already
 *     listens for indirectly via `runner-tier-changed`.
 *
 *   - Tier 2: shows the signed-in account (when available) and a "Sign
 *     out" button that drops the runner back to Tier Local.
 *
 * The component is intentionally lean — token/runner_id/heartbeat
 * diagnostics live in the existing `WebIntegrationSettings` panel. This
 * panel is purely about account-bind state.
 */

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { LogIn, LogOut, UserCircle2, Loader2, AlertCircle } from "lucide-react";

import { useRunnerTier } from "@/hooks/useRunnerTier";
import { useAuth } from "@/components/AuthProvider";
import { SectionHeader } from "./SectionHeader";
import type { LogFunction } from "./types";

interface AccountSettingsProps {
  onLog: LogFunction;
}

type FlowState = "idle" | "opening" | "awaiting-browser" | "error";

export function AccountSettings({ onLog }: AccountSettingsProps) {
  const { tier, loading: tierLoading, refresh: refreshTier } = useRunnerTier();
  const { authStatus, logout: clearAuthState } = useAuth();
  const [flowState, setFlowState] = useState<FlowState>("idle");
  const [flowError, setFlowError] = useState<string | null>(null);
  const [signingOut, setSigningOut] = useState(false);

  const isTier2 = tier === "qontinui_account";

  // Listen for the `web-integration-changed` Tauri event that the loopback
  // callback handler implicitly fires (via `apply_web_integration_settings`).
  // That event signals that token receipt completed in the browser; we
  // re-read the tier and clear the spinner.
  useEffect(() => {
    const unlistenPromise = listen("web-integration-changed", () => {
      void refreshTier();
      // Also re-fire `runner-tier-changed` so AuthProvider re-syncs its
      // JWT-flow gate now that we're (presumably) in Tier 2.
      window.dispatchEvent(new CustomEvent("runner-tier-changed"));
      setFlowState((prev) => (prev === "awaiting-browser" ? "idle" : prev));
    });
    return () => {
      void unlistenPromise.then((un) => un());
    };
  }, [refreshTier]);

  const handleSignIn = useCallback(async () => {
    setFlowState("opening");
    setFlowError(null);
    try {
      await invoke("start_qontinui_sign_in");
      setFlowState("awaiting-browser");
      onLog("info", "Opened browser for Qontinui sign-in — complete the flow there");
    } catch (err) {
      const msg = typeof err === "string" ? err : String(err);
      setFlowError(msg);
      setFlowState("error");
      onLog("error", `Sign-in failed to start: ${msg}`);
    }
  }, [onLog]);

  const handleSignOut = useCallback(async () => {
    setSigningOut(true);
    setFlowError(null);
    try {
      await invoke("qontinui_sign_out");
      // FE re-fires the tier-change event so AuthProvider re-syncs to the
      // synthesized local-guest auth without waiting for a poll cycle.
      window.dispatchEvent(new CustomEvent("runner-tier-changed"));
      await refreshTier();
      // Drop the JWT-flow auth state locally as well so the LoginScreen
      // doesn't briefly show on the next render.
      try {
        await clearAuthState();
      } catch {
        // clearAuthState invokes the Tier-2 `logout` command which now
        // errors with "Tier 0/1 — no auth"; that's expected and harmless.
      }
      onLog("info", "Signed out — runner returned to Tier Local");
    } catch (err) {
      const msg = typeof err === "string" ? err : String(err);
      setFlowError(msg);
      onLog("error", `Sign-out failed: ${msg}`);
    } finally {
      setSigningOut(false);
    }
  }, [onLog, refreshTier, clearAuthState]);

  if (tierLoading) {
    return (
      <div className="p-4">
        <SectionHeader
          title="Qontinui Account"
          description="Sign in to enable cloud sync and remote runner dispatch."
          icon={<UserCircle2 />}
        />
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <Loader2 className="w-4 h-4 animate-spin" />
          Loading tier…
        </div>
      </div>
    );
  }

  return (
    <div className="p-4 space-y-4">
      <SectionHeader
        title="Qontinui Account"
        description={
          isTier2
            ? "This runner is signed in to a Qontinui account (Tier 2)."
            : "Sign in to enable cloud sync, remote runner dispatch, and qontinui-web integration."
        }
        icon={<UserCircle2 />}
      />

      {flowError && (
        <div className="flex items-start gap-2 rounded border border-red-500/40 bg-red-500/10 p-3 text-sm text-red-200">
          <AlertCircle className="w-4 h-4 mt-0.5 shrink-0" />
          <span className="break-words">{flowError}</span>
        </div>
      )}

      {isTier2 ? (
        <SignedInPanel
          email={authStatus?.user?.email ?? null}
          userId={authStatus?.user?.id ?? null}
          name={authStatus?.user?.name ?? null}
          signingOut={signingOut}
          onSignOut={handleSignOut}
        />
      ) : (
        <SignedOutPanel flowState={flowState} onSignIn={handleSignIn} />
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Subcomponents
// ---------------------------------------------------------------------------

interface SignedInPanelProps {
  email: string | null;
  userId: string | null;
  name: string | null;
  signingOut: boolean;
  onSignOut: () => void;
}

function SignedInPanel({ email, userId, name, signingOut, onSignOut }: SignedInPanelProps) {
  // `check_auth_status` returns `user: null` for opaque runner tokens
  // (no JWT to decode `sub` from). Surface the qontinui_user_id placeholder
  // instead so the panel still shows "you're signed in".
  const displayLabel = email && email.length > 0 ? email : name && name.length > 0 ? name : null;

  return (
    <div className="space-y-3">
      <div className="rounded border border-border bg-card p-4">
        <div className="text-xs uppercase tracking-wide text-muted-foreground mb-1">
          Signed in as
        </div>
        {displayLabel ? (
          <div className="text-sm font-medium text-foreground">{displayLabel}</div>
        ) : (
          <div className="text-sm text-muted-foreground italic">
            (account identity unavailable — runner token does not carry a verified email)
          </div>
        )}
        {userId && userId !== "local-guest" && (
          <div className="mt-1 text-xs text-muted-foreground font-mono break-all">
            User ID: {userId}
          </div>
        )}
      </div>

      <button
        type="button"
        onClick={onSignOut}
        disabled={signingOut}
        className="inline-flex items-center gap-2 rounded bg-red-600 px-4 py-2 text-sm font-medium text-white hover:bg-red-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
      >
        {signingOut ? <Loader2 className="w-4 h-4 animate-spin" /> : <LogOut className="w-4 h-4" />}
        Sign out
      </button>
      <p className="text-xs text-muted-foreground">
        Signing out clears the runner token and drops the runner to Tier Local. Local automation and
        captures continue to work; cloud features become unavailable until you sign in again.
      </p>
    </div>
  );
}

interface SignedOutPanelProps {
  flowState: FlowState;
  onSignIn: () => void;
}

function SignedOutPanel({ flowState, onSignIn }: SignedOutPanelProps) {
  const inFlight = flowState === "opening" || flowState === "awaiting-browser";

  return (
    <div className="space-y-3">
      <button
        type="button"
        onClick={onSignIn}
        disabled={inFlight}
        className="inline-flex items-center gap-2 rounded bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
      >
        {inFlight ? <Loader2 className="w-4 h-4 animate-spin" /> : <LogIn className="w-4 h-4" />}
        Sign in to Qontinui
      </button>

      {flowState === "awaiting-browser" && (
        <p className="text-xs text-muted-foreground">
          Complete the sign-in in your browser. This panel will update automatically when the
          runner is paired.
        </p>
      )}
      {flowState === "opening" && (
        <p className="text-xs text-muted-foreground">Opening browser…</p>
      )}
    </div>
  );
}
