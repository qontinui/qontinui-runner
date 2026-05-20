/**
 * AuthProvider.tsx
 *
 * React context provider for authentication state management
 * Handles login/logout, token refresh, and auth status checking
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
import type { AuthStatus, AuthContextValue, LoginResponse } from "../types";
import { useRunnerTier } from "@/hooks/useRunnerTier";
import { createLogger } from "@/lib/logger";
import { withTimeout } from "@/lib/withTimeout";

// Defensive timeout for the initial-mount Tauri-invoke calls in AuthProvider.
// If the IPC channel is mid-setup on the first webview render and the
// invoke Promise never settles, the effect chain stays stuck and auto-login
// never fires. A 5s cap unsticks the chain so the existing catch handlers
// run their normal cleanup. See
// qontinui-dev-notes/plans/2026-05-17-runner-auto-login-initial-mount.md.
const AUTH_PROBE_TIMEOUT_MS = 5000;

// TODO(tier-gating): calibration tests for tier-gating are pending — once a
// Vitest harness for AuthProvider exists, cover:
//   1. Tier "local"/"local_provider" synthesizes local-guest auth and never
//      invokes `check_auth_status` / `refresh_token`.
//   2. Tier "qontinui_account" boots the existing JWT flow unchanged.
//   3. A `runner-tier-changed` event switching local→qontinui_account
//      drops the synthesized auth and triggers `check_auth_status`.

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

const TOKEN_REFRESH_INTERVAL = 14 * 60 * 1000; // 14 minutes (tokens expire in 15 minutes)

// Disarming window for the devAutoLoginPending failsafe. Re-armed on every
// retry attempt; fires only when the retry chain has gone idle without
// success. Must be strictly greater than the longest single retry interval
// (10s for transient non-network errors) so an in-flight scheduled retry is
// never pre-empted.
const DEV_AUTO_LOGIN_PENDING_FAILSAFE_MS = 15000;

// Development auto-login credentials (loaded from environment variables at
// Vite build time). For temp test runners spawned by the supervisor, the
// credentials are instead loaded at runtime from process env via the
// `get_test_auto_login` Tauri command (see the effect below). This lets a
// single pre-built runner binary auto-login for UI Bridge testing without
// rebuilding with VITE_DEV_EMAIL baked in.
const DEV_AUTO_LOGIN = {
  email: import.meta.env.VITE_DEV_EMAIL || "",
  password: import.meta.env.VITE_DEV_PASSWORD || "",
};

export function AuthProvider({ children }: AuthProviderProps) {
  // Tier gating — runner-tier-decoupling Phase 1. Tier 0 ("local") and
  // Tier 1 ("local_provider") DO NOT require a qontinui-account JWT; they
  // boot with a synthesized local-guest auth and never hit the auth
  // backend. Only Tier 2 ("qontinui_account") runs the JWT flow.
  const { tier, loading: tierLoading } = useRunnerTier();
  const isTier2 = tier === "qontinui_account";

  const [authStatus, setAuthStatus] = useState<AuthStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // Dev auto-login pending flag - starts false, set to true only when auto-login starts
  // This prevents blocking the UI while waiting for auth check to complete
  const [devAutoLoginPending, setDevAutoLoginPending] = useState(false);
  // Effective auto-login credentials. Starts with Vite-baked VITE_DEV_* values
  // and is overridden at runtime by `get_test_auto_login` if the process was
  // spawned by the supervisor with QONTINUI_TEST_AUTO_LOGIN_* env vars.
  const [autoLoginCreds, setAutoLoginCreds] = useState<{ email: string; password: string }>(
    DEV_AUTO_LOGIN,
  );
  // Tracks whether the runtime test-auto-login probe has completed. We defer
  // the auto-login effect until this resolves so a temp runner without Vite
  // creds doesn't flash the LoginScreen before runtime creds arrive.
  const [testAutoLoginProbed, setTestAutoLoginProbed] = useState(false);
  const mountCountRef = useRef(0);
  const refreshCallCountRef = useRef(0);
  // Timer ID for retrying dev auto-login when backend is temporarily unavailable
  const devLoginRetryTimer = useRef<number | null>(null);
  const devLoginRetryCount = useRef(0);
  const devLoginFailed = useRef(false);
  // Bump on each retry attempt so the devAutoLoginPending failsafe effect
  // re-arms its disarming timeout — without this, the failsafe fires after
  // the first 10s window even though the retry chain is still in flight.
  const [devLoginRetryTick, setDevLoginRetryTick] = useState(0);
  const MAX_DEV_LOGIN_RETRIES = 10;

  // Log mount/unmount
  useEffect(() => {
    mountCountRef.current += 1;
    log.debug(`AuthProvider MOUNTED (mount #${mountCountRef.current})`);
    return () => {
      log.debug(`AuthProvider UNMOUNTED (was mount #${mountCountRef.current})`);
      if (devLoginRetryTimer.current) {
        clearTimeout(devLoginRetryTimer.current);
      }
    };
  }, []);

  /**
   * Check authentication status
   * Called on mount and after login
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
   * Refresh authentication (token refresh)
   * Called periodically and can be called manually
   */
  const refreshAuth = useCallback(async () => {
    refreshCallCountRef.current += 1;
    const callNum = refreshCallCountRef.current;
    log.debug(`refreshAuth() called (call #${callNum})`);
    try {
      setError(null);
      log.debug(`refreshAuth() #${callNum} - invoking refresh_token command...`);
      await invoke("refresh_token");
      log.debug(`refreshAuth() #${callNum} - refresh_token succeeded, checking auth status...`);
      // Wrap the status check in its own try/catch — if refresh succeeded but the
      // status check fails (e.g., transient 5xx), keep the existing auth state
      // rather than propagating the failure as unauthenticated.
      try {
        await checkAuthStatus();
      } catch (statusErr) {
        log.warn(
          `refreshAuth() #${callNum} - status check failed after successful refresh, keeping existing auth state:`,
          statusErr,
        );
      }
      log.debug(`refreshAuth() #${callNum} - completed successfully`);
    } catch (err) {
      log.warn(`refreshAuth() #${callNum} - Token refresh failed:`, err);
      // Don't immediately log the user out. The current access token may still
      // be valid — a refresh failure alone (network blip, backend momentarily
      // unavailable, 5xx) should not destroy the user's session.
      //
      // We also deliberately do NOT `setError(err)` here. A non-null `error`
      // is read by `PromptHomePage`'s "Sign-in required" banner as "session
      // expired", which it isn't — the next refresh cycle will retry. The
      // backend's `refresh_token_impl` clears tokens itself on a true 401/403,
      // so a real expiry surfaces through `authStatus.authenticated = false`
      // on the next `checkAuthStatus`, not through this catch.
    }
  }, [checkAuthStatus]);

  /**
   * Login with email and password
   */
  const login = useCallback(async (email: string, password: string) => {
    try {
      setLoading(true);
      setError(null);
      const response = await invoke<LoginResponse>("login", { email, password });

      // Update auth status with the response data
      setAuthStatus({
        authenticated: true,
        user: response.user,
        device_info: response.device_info,
      });
    } catch (err) {
      log.error("Login failed:", err);
      setError(err as string);
      throw err; // Re-throw so LoginScreen can handle it
    } finally {
      setLoading(false);
    }
  }, []);

  /**
   * Logout and clear tokens
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
   * Tier 0/1 never call `check_auth_status` (the backend short-circuits
   * with `authenticated: false` anyway when tier != QontinuiAccount, but
   * we avoid the round-trip entirely and synthesize a local-guest auth
   * state in the effect below).
   */
  useEffect(() => {
    if (tierLoading) return;
    if (!isTier2) return;
    log.debug("useEffect[checkAuthStatus] - checking auth status on mount");
    // Async IIFE so we don't invoke a setState-bearing callback synchronously
    // in the effect body (set-state-in-effect).
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
   *
   * Runs once the tier has been read. Sets `authenticated: true` with a
   * sentinel `local-guest` user so downstream consumers that gate on
   * `authStatus?.authenticated` keep working without ever reaching the
   * backend. Also clears the initial `loading` so the App loading screen
   * doesn't linger on Tier 0/1.
   */
  useEffect(() => {
    if (tierLoading) return;
    if (!isTier2 && authStatus === null) {
      // Defer setState off the effect body to avoid cascading renders
      // during the same commit (react-hooks/set-state-in-effect). Same
      // idiom as the auto-login effect below.
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
   * Probe for runtime test-auto-login credentials (temp test runners spawned
   * by the supervisor). If `QONTINUI_TEST_AUTO_LOGIN_EMAIL` /
   * `QONTINUI_TEST_AUTO_LOGIN_PASSWORD` were set on the process env by the
   * supervisor, the backend `get_test_auto_login` command returns them and
   * we use them as auto-login credentials — overriding (or filling in for)
   * the Vite-baked VITE_DEV_* values. This lets a single pre-built runner
   * binary auto-login for UI Bridge testing of authenticated pages.
   */
  useEffect(() => {
    let cancelled = false;
    withTimeout(
      invoke<{ email: string; password: string } | null>("get_test_auto_login"),
      AUTH_PROBE_TIMEOUT_MS,
      "get_test_auto_login",
    )
      .then((creds) => {
        if (cancelled) return;
        if (creds && creds.email && creds.password) {
          log.debug("Test auto-login: runtime credentials found, enabling auto-login");
          setAutoLoginCreds(creds);
        }
        setTestAutoLoginProbed(true);
      })
      .catch((err) => {
        if (cancelled) return;
        log.debug("Test auto-login probe failed (expected for primary runners):", err);
        setTestAutoLoginProbed(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  /**
   * Attempt the dev auto-login once, scheduling a retry on transient failure.
   *
   * The retry-timer must be able to re-invoke this function directly: relying
   * on the auto-login useEffect to re-run does NOT work, because that effect's
   * deps include `authStatus?.authenticated` as a boolean. A failed re-check
   * that yields the same `false` value does not change the dep, so the effect
   * does not fire — historically this meant the very first retry timer fired
   * but never produced a second login attempt.
   *
   * To avoid a useCallback self-reference (which the react-hooks lint rule
   * forbids), we route the recursive call through `attemptRef`.
   */
  const attemptRef = useRef<((email: string, password: string) => void) | null>(null);
  const attemptDevAutoLogin = useCallback(
    (email: string, password: string) => {
      log.debug("Dev mode: Not authenticated, attempting auto-login...");
      // Set pending flag BEFORE starting login so UI shows "Signing in..."
      setDevAutoLoginPending(true);
      // Bump the retry tick so the devAutoLoginPending failsafe effect
      // re-arms its disarming timeout window for this attempt.
      setDevLoginRetryTick((t) => t + 1);
      login(email, password)
        .then(() => {
          // Login successful - the authStatus effect will clear devAutoLoginPending
          log.debug("Dev mode auto-login succeeded");
        })
        .catch((err) => {
          const errStr = String(err);
          const isNetworkError =
            errStr.includes("Network error") || errStr.includes("Failed to fetch");
          const isTransientError =
            isNetworkError || errStr.includes("Server error") || errStr.includes("Rate limit");
          if (isTransientError && devLoginRetryCount.current < MAX_DEV_LOGIN_RETRIES) {
            // Transient error (network, server, rate limit) — retry with backoff
            devLoginRetryCount.current += 1;
            const delay = isNetworkError ? 5000 : 10000;
            log.warn(
              `Dev mode auto-login failed (${isNetworkError ? "backend unavailable" : "transient error"}), retry ${devLoginRetryCount.current}/${MAX_DEV_LOGIN_RETRIES} in ${delay / 1000}s...`,
            );
            devLoginRetryTimer.current = window.setTimeout(() => {
              devLoginRetryTimer.current = null;
              // Re-invoke the attempt directly via the ref. Calling
              // checkAuthStatus() alone would not retry: the auto-login
              // effect bails when authStatus?.authenticated is unchanged
              // (still false).
              attemptRef.current?.(email, password);
            }, delay);
          } else {
            if (isTransientError) {
              log.error("Dev mode auto-login failed: max retries exceeded");
            } else {
              log.error("Dev mode auto-login failed (auth error):", err);
            }
            // Clear pending flag on non-retryable failure so user can see login screen
            devLoginFailed.current = true;
            setDevAutoLoginPending(false);
          }
        });
    },
    [login],
  );
  // Keep the ref pointing at the latest stable callback so timer-driven
  // retries always invoke the current closure (which has the current
  // `login` dep). Updated via effect so we never write the ref during render.
  useEffect(() => {
    attemptRef.current = attemptDevAutoLogin;
  }, [attemptDevAutoLogin]);

  /**
   * Auto-login with configured credentials
   * Automatically logs in when VITE_DEV_EMAIL/VITE_DEV_PASSWORD are set in .env
   * Works in both dev mode (Vite) and exe mode (embedded build)
   */
  useEffect(() => {
    // Tier 0/1 don't authenticate against the qontinui-account backend —
    // there's no credential to auto-login with. The test-auto-login probe
    // itself remains unconditional because the test harness flips tier to
    // "qontinui_account" via the QONTINUI_TEST_AUTO_LOGIN_* path before
    // expecting the auto-login effect to fire.
    if (!isTier2) {
      return;
    }

    // Wait for the runtime test-auto-login probe to complete so a temp runner
    // with runtime-only creds doesn't miss its window.
    if (!testAutoLoginProbed) {
      return;
    }

    // Skip if no credentials configured (covers both dev and production builds)
    if (!autoLoginCreds.email || !autoLoginCreds.password) {
      return;
    }

    // Wait for auth check to complete
    if (loading) {
      return;
    }

    // If already authenticated, clear pending flag (covers both initial auth and successful auto-login)
    if (authStatus?.authenticated) {
      log.debug("Dev mode: Authenticated, clearing devAutoLoginPending");

      if (devLoginRetryTimer.current) {
        clearTimeout(devLoginRetryTimer.current);
        devLoginRetryTimer.current = null;
      }
      devLoginRetryCount.current = 0;
      devLoginFailed.current = false;

      // Defer setState off the effect body to avoid cascading renders
      // during the same commit (set-state-in-effect).
      queueMicrotask(() => {
        setDevAutoLoginPending(false);
        // Signal UI Bridge to re-discover elements now that the
        // authenticated page is rendered. Without this, the auto-register's
        // initial scan captures login-page DOM and agents only see the
        // login form elements in /control/snapshot.
        window.dispatchEvent(new CustomEvent("ui-bridge-auth-complete"));
      });

      return;
    }

    // Don't retry after a non-retryable failure (auth error, rate limit, etc.)
    if (devLoginFailed.current) {
      return;
    }

    // If a retry timer is already scheduled, don't start another attempt
    if (devLoginRetryTimer.current) {
      return;
    }

    // First attempt — subsequent retries are driven by the retry timer
    // re-invoking attemptDevAutoLogin directly (not by this effect re-running).
    attemptDevAutoLogin(autoLoginCreds.email, autoLoginCreds.password);
  }, [
    loading,
    authStatus?.authenticated,
    testAutoLoginProbed,
    autoLoginCreds,
    attemptDevAutoLogin,
    isTier2,
  ]);

  /**
   * Failsafe timeout for loading state
   * If loading is still true after 3 seconds, force it to false
   * This prevents the app from being stuck on "Loading..." forever
   */
  useEffect(() => {
    if (!loading) return;

    const timeout = setTimeout(() => {
      log.warn("Loading timeout - forcing loading to false");
      setLoading(false);
      setDevAutoLoginPending(false);
    }, 3000);

    return () => clearTimeout(timeout);
  }, [loading]);

  /**
   * Failsafe timeout for devAutoLoginPending.
   *
   * Contract: this timer is re-armed on every retry attempt (via the
   * `devLoginRetryTick` dependency, which is bumped by `attemptDevAutoLogin`).
   * It therefore fires only when the retry chain has gone idle without
   * success — i.e., no fresh attempt has happened in the past
   * DEV_AUTO_LOGIN_PENDING_FAILSAFE_MS milliseconds.
   *
   * The window must be strictly greater than the longest single retry
   * interval (10s for non-network transient errors, 5s for network errors)
   * so a still-scheduled retry attempt is never pre-empted by the failsafe.
   * We do not want to remove the failsafe entirely: it covers the rare case
   * where login() resolves but pending stays stuck due to a race or unmount
   * during the retry chain.
   */
  useEffect(() => {
    if (!devAutoLoginPending) return;

    const timeout = setTimeout(() => {
      log.warn("devAutoLoginPending timeout - forcing to false");
      setDevAutoLoginPending(false);
    }, DEV_AUTO_LOGIN_PENDING_FAILSAFE_MS);

    return () => clearTimeout(timeout);
  }, [devAutoLoginPending, devLoginRetryTick]);

  /**
   * Set up auto-refresh timer when authenticated
   */
  useEffect(() => {
    // Tier 0/1 have no JWT to refresh — `refresh_token` would fail anyway.
    if (!isTier2) return;
    log.debug("useEffect[auto-refresh] triggered", { authenticated: authStatus?.authenticated });

    if (!authStatus?.authenticated) {
      log.debug("useEffect[auto-refresh] - NOT authenticated, skipping timer setup");
      return;
    }

    log.debug("useEffect[auto-refresh] - IS authenticated, setting up token refresh timer");
    const intervalId = setInterval(() => {
      log.debug("Auto-refresh timer fired - calling refreshAuth()");
      refreshAuth();
    }, TOKEN_REFRESH_INTERVAL);

    return () => {
      log.debug("useEffect[auto-refresh] cleanup - clearing token refresh timer");
      clearInterval(intervalId);
    };
  }, [authStatus, refreshAuth, isTier2]);

  const contextValue: AuthContextValue = {
    authStatus,
    loading,
    error,
    devAutoLoginPending,
    tier,
    login,
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
