/**
 * AuthProvider.tsx
 *
 * React context provider for authentication state management.
 *
 * Auth is **Cognito-only**: the web backend no longer exposes a local
 * email/password login (`/jwt/login` / `/jwt/refresh` are deleted server-side),
 * so this provider no longer has a `login(email, password)` action and never
 * invokes a `refresh_token` command. Sign-in goes through the runner's Cognito
 * Hosted-UI flow (`cognito_sign_in`, surfaced by `LoginScreen` and
 * `AccountSettings`); the Cognito access token is kept fresh by the Rust-side
 * device-JWT refresher, not by a frontend timer.
 *
 * This provider's job is now just to:
 *   - read auth status (`check_auth_status`) for Tier 2,
 *   - synthesize a local-guest auth for Tier 0/1,
 *   - periodically re-validate Tier-2 status,
 *   - expose `logout`.
 */

import {
  createContext,
  useContext,
  useState,
  useEffect,
  useCallback,
  ReactNode,
  useRef,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AuthStatus, AuthContextValue } from "../types";
import { useRunnerTier } from "@/hooks/useRunnerTier";
import { createLogger } from "@/lib/logger";
import { withTimeout } from "@/lib/withTimeout";

// Defensive timeout for the initial-mount Tauri-invoke calls in AuthProvider.
// If the IPC channel is mid-setup on the first webview render and the
// invoke Promise never settles, the effect chain stays stuck. This cap unsticks
// the chain so the existing catch handlers run their normal cleanup.
//
// It MUST exceed the Rust side's own worst case. `check_auth_status` decides
// the verdict from the local keychain, then best-effort-enriches the profile
// over the network under its own `ENRICHMENT_BUDGET` (commands/auth.rs). This
// cap sits comfortably above that budget so a slow-but-successful probe is
// never cut short — the old 5s value was equal to a single one of the
// enrichment's network legs, so ordinary backend latency tripped it.
const AUTH_PROBE_TIMEOUT_MS = 15000;

// Backoff schedule for re-probing after an INDETERMINATE result (timeout /
// IPC error). Capped and repeated at the last value — the runner is useless
// to a Tier-2 operator until auth resolves, so we never stop retrying.
const AUTH_PROBE_RETRY_BACKOFF_MS = [1000, 2000, 4000, 8000, 15000] as const;

const log = createLogger("Auth");

// Store context in window to survive HMR reloads
declare global {
  interface Window {
    __AUTH_CONTEXT__?: React.Context<AuthContextValue | null>;
  }
}

// Create context once and store in window to survive HMR reloads
const AuthContext: React.Context<AuthContextValue | null> =
  window.__AUTH_CONTEXT__ ||
  (window.__AUTH_CONTEXT__ = createContext<AuthContextValue | null>(null));

interface AuthProviderProps {
  children: ReactNode;
}

// How often we re-validate the Tier-2 auth status against the backend. The
// Cognito access token itself is refreshed by the Rust device-JWT refresher;
// this interval only re-reads the resulting authenticated/expired verdict.
const AUTH_RECHECK_INTERVAL = 14 * 60 * 1000; // 14 minutes

export function AuthProvider({ children }: AuthProviderProps) {
  // Tier gating — runner-tier-decoupling Phase 1. Tier 0 ("local") and
  // Tier 1 ("local_provider") DO NOT require a qontinui-account session; they
  // boot with a synthesized local-guest auth and never hit the auth backend.
  // Only Tier 2 ("qontinui_account") runs the Cognito status flow.
  const { tier, loading: tierLoading } = useRunnerTier();
  const isTier2 = tier === "qontinui_account";

  const [authStatus, setAuthStatus] = useState<AuthStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // True while the Tier-2 probe has not yet produced a DEFINITIVE verdict —
  // i.e. every attempt so far timed out or threw. Distinct from `loading`
  // (which the 3s failsafe below force-clears): consumers gate the sign-in
  // screen on this so an indeterminate probe never reads as "signed out".
  const [probeUnresolved, setProbeUnresolved] = useState(true);
  const mountCountRef = useRef(0);
  const refreshCallCountRef = useRef(0);
  const retryTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const retryAttemptRef = useRef(0);

  // Log mount/unmount
  useEffect(() => {
    mountCountRef.current += 1;
    log.debug(`AuthProvider MOUNTED (mount #${mountCountRef.current})`);
    return () => {
      log.debug(`AuthProvider UNMOUNTED (was mount #${mountCountRef.current})`);
    };
  }, []);

  /**
   * Check authentication status. Called on mount and periodically (Tier 2).
   *
   * A probe that TIMES OUT or throws is INDETERMINATE — it says nothing about
   * whether the runner is signed in. It used to be recorded as
   * `authenticated: false`, which is how a transient IPC/backend stall turned
   * into a sign-out: `check_auth_status` reaches the network to enrich the
   * user profile, the invoke overran the cap, and the App gate
   * (`isTier2 && !authenticated → LoginScreen`) put a fully-signed-in runner
   * on the sign-in screen. Nothing recovered it either — the periodic
   * re-check timer below only arms when already authenticated — so the window
   * stayed there until the operator signed in by hand. A brand-new webview
   * (every pop-out terminal window) probes auth while the app is still
   * booting, which is exactly when the stall is likeliest.
   *
   * So: only a RESOLVED response mutates `authStatus`. An indeterminate one
   * leaves the previous state alone and schedules a backoff retry.
   */
  const checkAuthStatusRef = useRef<(() => Promise<AuthStatus | null>) | null>(null);

  const checkAuthStatus = useCallback(async () => {
    log.debug("checkAuthStatus() called");
    try {
      setError(null);
      const status = await withTimeout(
        invoke<AuthStatus>("check_auth_status"),
        AUTH_PROBE_TIMEOUT_MS,
        "check_auth_status",
      );
      log.debug("checkAuthStatus() result:", status);
      if (retryTimerRef.current !== null) {
        clearTimeout(retryTimerRef.current);
        retryTimerRef.current = null;
      }
      retryAttemptRef.current = 0;
      setAuthStatus(status);
      setProbeUnresolved(false);
      return status;
    } catch (err) {
      // Indeterminate — keep whatever we knew before and try again shortly.
      log.error("Auth status probe failed (indeterminate — will retry):", err);
      setError(err as string);
      const attempt = retryAttemptRef.current;
      retryAttemptRef.current = attempt + 1;
      const delay =
        AUTH_PROBE_RETRY_BACKOFF_MS[
          Math.min(attempt, AUTH_PROBE_RETRY_BACKOFF_MS.length - 1)
        ];
      if (retryTimerRef.current !== null) clearTimeout(retryTimerRef.current);
      retryTimerRef.current = setTimeout(() => {
        retryTimerRef.current = null;
        void checkAuthStatusRef.current?.();
      }, delay);
      log.debug(`Auth status re-probe scheduled in ${delay}ms (attempt #${attempt + 1})`);
      return null;
    } finally {
      setLoading(false);
    }
  }, []);

  // `checkAuthStatus` is stable ([] deps); publish it for the retry timer,
  // which cannot close over the callback it is defined inside.
  useEffect(() => {
    checkAuthStatusRef.current = checkAuthStatus;
  }, [checkAuthStatus]);

  // Never leave a retry armed past unmount (pop-out window closed mid-backoff).
  useEffect(
    () => () => {
      if (retryTimerRef.current !== null) clearTimeout(retryTimerRef.current);
    },
    [],
  );

  /**
   * Re-validate authentication. Under Cognito-only auth there is no separate
   * `refresh_token` command — the Cognito access token is refreshed Rust-side
   * by the device-JWT refresher — so this simply re-reads the auth status.
   * Kept in the context (and on the periodic timer) so consumers that want to
   * force a re-check have a stable entry point.
   */
  const refreshAuth = useCallback(async () => {
    refreshCallCountRef.current += 1;
    const callNum = refreshCallCountRef.current;
    log.debug(`refreshAuth() #${callNum} - re-checking auth status`);
    try {
      await checkAuthStatus();
    } catch (err) {
      // Don't destroy the session on a transient status-check failure; the
      // backend's `check_auth_status` only reports `authenticated:false` on a
      // definitive 401/403, which surfaces through the next render.
      log.warn(`refreshAuth() #${callNum} - status re-check failed (keeping state):`, err);
    }
  }, [checkAuthStatus]);

  /**
   * Logout and clear local auth state.
   *
   * This is the DEFAULT logout: the Rust `logout` command clears only the
   * interactive device-JWT session and PRESERVES the Cognito session, so the
   * runner's autonomous terminal AI sessions keep running (the device JWT is
   * re-minted in the background). Use {@link signOutFull} to fully sign out and
   * stop autonomous sessions.
   */
  const logout = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      await invoke("logout");
      setAuthStatus({
        authenticated: false,
        user: null,
        device_info: null,
      });
    } catch (err) {
      log.error("Logout failed:", err);
      setError(err as string);
      // Clear local state even if backend call fails
      setAuthStatus({
        authenticated: false,
        user: null,
        device_info: null,
      });
    } finally {
      // An OPERATOR-requested sign-out is a definitive verdict — unlike a
      // failed probe, it must reach the sign-in screen.
      setProbeUnresolved(false);
      setLoading(false);
    }
  }, []);

  /**
   * Full sign-out that STOPS autonomous terminal sessions.
   *
   * The Rust `sign_out_full` command clears ALL credentials (device JWT AND the
   * Cognito session, including the long-lived refresh token), so the
   * device-JWT refresher can no longer self-recover and the background daemons
   * stop driving autonomous sessions until an interactive re-login.
   */
  const signOutFull = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      await invoke("sign_out_full");
      setAuthStatus({
        authenticated: false,
        user: null,
        device_info: null,
      });
    } catch (err) {
      log.error("Full sign-out failed:", err);
      setError(err as string);
      // The credentials may still be on disk — surface the error, but still
      // flip local state so the user is not trapped in a signed-in shell.
      // NOTE: this catch once masked a real bug. `sign_out_full` was never
      // registered in main.rs's `generate_handler!`, so every call threw
      // "Command sign_out_full not found" and the wipe silently never ran,
      // while this branch still reported a clean sign-out. A registration
      // guard now prevents that class (src-tauri/tests/tauri_commands_are_registered.rs).
      setAuthStatus({
        authenticated: false,
        user: null,
        device_info: null,
      });
    } finally {
      // Operator-requested — definitive, same as `logout`.
      setProbeUnresolved(false);
      setLoading(false);
    }
  }, []);

  /**
   * Check auth status on mount — Tier 2 only.
   *
   * Tier 0/1 never call `check_auth_status` (the backend short-circuits with
   * `authenticated: false` anyway when tier != QontinuiAccount), so we avoid
   * the round-trip and synthesize a local-guest auth state below.
   */
  useEffect(() => {
    if (tierLoading) return;
    if (!isTier2) return;
    log.debug("useEffect[checkAuthStatus] - checking auth status on mount");
    let cancelled = false;
    void (async () => {
      if (cancelled) return;
      await checkAuthStatus();
    })();
    return () => {
      cancelled = true;
    };
  }, [checkAuthStatus, isTier2, tierLoading]);

  /**
   * Re-check auth status when the runner tier changes underneath us.
   *
   * `useRunnerTier` already re-reads the tier on the `runner-tier-changed`
   * window event, but when the tier is ALREADY "qontinui_account" (e.g. a
   * prior sign-in promoted it, but the auth status was still unauthenticated
   * so `LoginScreen` was showing), a fresh sign-in does NOT change the `tier`
   * value — so the mount effect above (keyed on `isTier2`) never re-runs and
   * `authStatus.authenticated` stays stale-false. The App gate
   * (`isTier2 && !authenticated → LoginScreen`) then keeps showing the
   * sign-in screen until a process restart.
   *
   * Listening for the same event here and re-running `checkAuthStatus` flips
   * `authenticated` live so the view leaves `LoginScreen` without a restart.
   */
  useEffect(() => {
    const onTierChanged = () => {
      log.debug("runner-tier-changed observed - re-checking auth status");
      void checkAuthStatus();
    };
    window.addEventListener("runner-tier-changed", onTierChanged);
    return () => window.removeEventListener("runner-tier-changed", onTierChanged);
  }, [checkAuthStatus]);

  /**
   * Synthesize a local-guest authStatus for Tier 0/1.
   */
  useEffect(() => {
    if (tierLoading) return;
    if (!isTier2 && authStatus === null) {
      queueMicrotask(() => {
        setAuthStatus({
          authenticated: true,
          user: { id: "local-guest", email: "", name: null },
          device_info: null,
        });
        // Tier 0/1 never probes, so the probe is trivially resolved.
        setProbeUnresolved(false);
        setLoading(false);
      });
    }
  }, [tierLoading, isTier2, authStatus]);

  /**
   * Failsafe timeout for loading state. If loading is still true after 3
   * seconds, force it to false so the app doesn't hang on "Loading...".
   */
  useEffect(() => {
    if (!loading) return;

    const timeout = setTimeout(() => {
      log.warn("Loading timeout - forcing loading to false");
      setLoading(false);
    }, 3000);

    return () => clearTimeout(timeout);
  }, [loading]);

  /**
   * Periodic auth-status re-check when authenticated (Tier 2 only).
   */
  useEffect(() => {
    // Tier 0/1 have no backend session to re-validate.
    if (!isTier2) return;
    log.debug("useEffect[auto-recheck] triggered", { authenticated: authStatus?.authenticated });

    if (!authStatus?.authenticated) {
      log.debug("useEffect[auto-recheck] - NOT authenticated, skipping timer setup");
      return;
    }

    log.debug("useEffect[auto-recheck] - IS authenticated, setting up status re-check timer");
    const intervalId = setInterval(() => {
      log.debug("Auth re-check timer fired - calling refreshAuth()");
      refreshAuth();
    }, AUTH_RECHECK_INTERVAL);

    return () => {
      log.debug("useEffect[auto-recheck] cleanup - clearing status re-check timer");
      clearInterval(intervalId);
    };
  }, [authStatus, refreshAuth, isTier2]);

  const contextValue: AuthContextValue = {
    authStatus,
    loading,
    error,
    // Retained for API stability: dev email/password auto-login is removed, so
    // there is never a pending auto-login. Consumers (App, PromptHomePage) gate
    // on this; keeping it constant-false preserves their logic.
    devAutoLoginPending: false,
    // Tier 2 only: no definitive verdict yet. Consumers must NOT read this as
    // "signed out" — see `checkAuthStatus`.
    authProbeUnresolved: isTier2 && probeUnresolved,
    tier,
    logout,
    signOutFull,
    refreshAuth,
  };

  return <AuthContext.Provider value={contextValue}>{children}</AuthContext.Provider>;
}

/**
 * Hook to use auth context
 * Must be used within AuthProvider
 */
export function useAuth(): AuthContextValue {
  const context = useContext(AuthContext);
  if (!context) {
    throw new Error("useAuth must be used within AuthProvider");
  }
  return context;
}
