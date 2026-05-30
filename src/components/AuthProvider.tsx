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
// invoke Promise never settles, the effect chain stays stuck. A 5s cap unsticks
// the chain so the existing catch handlers run their normal cleanup.
const AUTH_PROBE_TIMEOUT_MS = 5000;

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
  const mountCountRef = useRef(0);
  const refreshCallCountRef = useRef(0);

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
   */
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
      setAuthStatus(status);
      return status;
    } catch (err) {
      log.error("Failed to check auth status:", err);
      setError(err as string);
      setAuthStatus({
        authenticated: false,
        user: null,
        device_info: null,
      });
      return null;
    } finally {
      setLoading(false);
    }
  }, []);

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
    tier,
    logout,
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
