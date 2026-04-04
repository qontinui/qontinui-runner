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
import { createLogger } from "@/lib/logger";

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

// Development auto-login credentials (loaded from environment variables)
const DEV_AUTO_LOGIN = {
  email: import.meta.env.VITE_DEV_EMAIL || "",
  password: import.meta.env.VITE_DEV_PASSWORD || "",
};

export function AuthProvider({ children }: AuthProviderProps) {
  const [authStatus, setAuthStatus] = useState<AuthStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // Dev auto-login pending flag - starts false, set to true only when auto-login starts
  // This prevents blocking the UI while waiting for auth check to complete
  const [devAutoLoginPending, setDevAutoLoginPending] = useState(false);
  const mountCountRef = useRef(0);
  const refreshCallCountRef = useRef(0);
  // Timer ID for retrying dev auto-login when backend is temporarily unavailable
  const devLoginRetryTimer = useRef<number | null>(null);
  const devLoginRetryCount = useRef(0);
  const devLoginFailed = useRef(false);
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
      const status = await invoke<AuthStatus>("check_auth_status");
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
      // Don't immediately log the user out. The current access token may still be
      // valid — a refresh failure alone (e.g., transient network issue, backend
      // momentarily unavailable) should not destroy the user's session and cause
      // the entire app tree (including terminal sessions) to unmount.
      // The next refresh cycle will try again. If the token truly expired,
      // subsequent API calls will return 401 and the user can re-authenticate.
      setError(err as string);
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
   * Check auth status on mount
   */
  useEffect(() => {
    log.debug("useEffect[checkAuthStatus] - checking auth status on mount");
    checkAuthStatus();
  }, [checkAuthStatus]);

  /**
   * Auto-login with configured credentials
   * Automatically logs in when VITE_DEV_EMAIL/VITE_DEV_PASSWORD are set in .env
   * Works in both dev mode (Vite) and exe mode (embedded build)
   */
  useEffect(() => {
    // Skip if no credentials configured (covers both dev and production builds)
    if (!DEV_AUTO_LOGIN.email || !DEV_AUTO_LOGIN.password) {
      return;
    }

    // Wait for auth check to complete
    if (loading) {
      return;
    }

    // If already authenticated, clear pending flag (covers both initial auth and successful auto-login)
    if (authStatus?.authenticated) {
      log.debug("Dev mode: Authenticated, clearing devAutoLoginPending");
      setDevAutoLoginPending(false);
      if (devLoginRetryTimer.current) {
        clearTimeout(devLoginRetryTimer.current);
        devLoginRetryTimer.current = null;
      }
      devLoginRetryCount.current = 0;
      devLoginFailed.current = false;
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

    // Auto-login in development
    log.debug("Dev mode: Not authenticated, attempting auto-login...");
    // Set pending flag BEFORE starting login so UI shows "Signing in..."
    setDevAutoLoginPending(true);
    login(DEV_AUTO_LOGIN.email, DEV_AUTO_LOGIN.password)
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
            checkAuthStatus();
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
  }, [loading, authStatus?.authenticated, login, checkAuthStatus]);

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
   * Failsafe timeout for devAutoLoginPending
   * Covers the case where login() resolves quickly (clearing loading)
   * but devAutoLoginPending stays stuck true due to a race condition
   */
  useEffect(() => {
    if (!devAutoLoginPending) return;

    const timeout = setTimeout(() => {
      log.warn("devAutoLoginPending timeout - forcing to false");
      setDevAutoLoginPending(false);
    }, 10000);

    return () => clearTimeout(timeout);
  }, [devAutoLoginPending]);

  /**
   * Set up auto-refresh timer when authenticated
   */
  useEffect(() => {
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
  }, [authStatus, refreshAuth]);

  const contextValue: AuthContextValue = {
    authStatus,
    loading,
    error,
    devAutoLoginPending,
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
