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

// Development auto-login credentials
const DEV_AUTO_LOGIN = {
  email: "josh@qontinui.io",
  password: "admin123",
};

export function AuthProvider({ children }: AuthProviderProps) {
  const [authStatus, setAuthStatus] = useState<AuthStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // In dev mode, we wait for auto-login to complete before showing the app
  const [devAutoLoginPending, setDevAutoLoginPending] = useState(import.meta.env.DEV);
  const mountCountRef = useRef(0);
  const refreshCallCountRef = useRef(0);
  const hasAttemptedDevLogin = useRef(false);

  // Log mount/unmount
  useEffect(() => {
    mountCountRef.current += 1;
    console.log(`[AUTH] AuthProvider MOUNTED (mount #${mountCountRef.current})`);
    return () => {
      console.log(`[AUTH] AuthProvider UNMOUNTED (was mount #${mountCountRef.current})`);
    };
  }, []);

  /**
   * Check authentication status
   * Called on mount and after login
   */
  const checkAuthStatus = useCallback(async () => {
    console.log("[AUTH] checkAuthStatus() called");
    try {
      setError(null);
      const status = await invoke<AuthStatus>("check_auth_status");
      console.log("[AUTH] checkAuthStatus() result:", status);
      setAuthStatus(status);
      return status;
    } catch (err) {
      console.error("[AUTH] Failed to check auth status:", err);
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
    console.log(`[AUTH] refreshAuth() called (call #${callNum})`);
    console.log("[AUTH] refreshAuth() stack trace:", new Error().stack);
    try {
      setError(null);
      console.log(`[AUTH] refreshAuth() #${callNum} - invoking refresh_token command...`);
      await invoke("refresh_token");
      console.log(
        `[AUTH] refreshAuth() #${callNum} - refresh_token succeeded, checking auth status...`,
      );
      await checkAuthStatus();
      console.log(`[AUTH] refreshAuth() #${callNum} - completed successfully`);
    } catch (err) {
      console.error(`[AUTH] refreshAuth() #${callNum} - Failed to refresh token:`, err);
      setError(err as string);
      // If refresh fails, user needs to log in again
      setAuthStatus({
        authenticated: false,
        user: null,
        device_info: null,
      });
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
      console.error("[AUTH] Login failed:", err);
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
      console.error("[AUTH] Logout failed:", err);
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
    console.log("[AUTH] useEffect[checkAuthStatus] - checking auth status on mount");
    checkAuthStatus();
  }, [checkAuthStatus]);

  /**
   * Development mode auto-login
   * Automatically logs in with dev credentials when not authenticated in dev mode
   */
  useEffect(() => {
    // Only run in development mode
    if (!import.meta.env.DEV) {
      setDevAutoLoginPending(false);
      return;
    }

    // Wait for auth check to complete
    if (loading) {
      return;
    }

    // If already authenticated, clear pending flag (covers both initial auth and successful auto-login)
    if (authStatus?.authenticated) {
      console.log("[AUTH] Dev mode: Authenticated, clearing devAutoLoginPending");
      setDevAutoLoginPending(false);
      return;
    }

    // Only attempt auto-login once to avoid infinite loops
    if (hasAttemptedDevLogin.current) {
      // Already attempted but not authenticated - login must have failed
      // Clear pending flag so user can see login screen
      setDevAutoLoginPending(false);
      return;
    }

    // Auto-login in development
    console.log("[AUTH] Dev mode: Not authenticated, attempting auto-login...");
    hasAttemptedDevLogin.current = true;
    login(DEV_AUTO_LOGIN.email, DEV_AUTO_LOGIN.password).catch((err) => {
      console.error("[AUTH] Dev mode auto-login failed:", err);
      // Don't set devAutoLoginPending here - the effect will re-run and handle it above
    });
  }, [loading, authStatus?.authenticated, login]);

  /**
   * Set up auto-refresh timer when authenticated
   */
  useEffect(() => {
    console.log("[AUTH] useEffect[auto-refresh] triggered:", {
      authStatus: authStatus,
      "authStatus?.authenticated": authStatus?.authenticated,
      "refreshAuth reference": refreshAuth.toString().slice(0, 100),
    });

    if (!authStatus?.authenticated) {
      console.log("[AUTH] useEffect[auto-refresh] - NOT authenticated, skipping timer setup");
      return;
    }

    console.log(
      "[AUTH] useEffect[auto-refresh] - IS authenticated, setting up token refresh timer",
    );
    const intervalId = setInterval(() => {
      console.log("[AUTH] Auto-refresh timer fired - calling refreshAuth()");
      refreshAuth();
    }, TOKEN_REFRESH_INTERVAL);

    return () => {
      console.log("[AUTH] useEffect[auto-refresh] cleanup - clearing token refresh timer");
      clearInterval(intervalId);
    };
  }, [authStatus?.authenticated, refreshAuth]);

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
