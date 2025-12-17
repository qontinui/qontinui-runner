/**
 * AuthProvider.tsx
 *
 * React context provider for authentication state management
 * Handles login/logout, token refresh, and auth status checking
 */

import { createContext, useContext, useState, useEffect, useCallback, ReactNode, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AuthStatus, AuthContextValue, LoginResponse } from "../types";

const AuthContext = createContext<AuthContextValue | null>(null);

interface AuthProviderProps {
  children: ReactNode;
}

const TOKEN_REFRESH_INTERVAL = 14 * 60 * 1000; // 14 minutes (tokens expire in 15 minutes)

export function AuthProvider({ children }: AuthProviderProps) {
  const [authStatus, setAuthStatus] = useState<AuthStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const mountCountRef = useRef(0);
  const refreshCallCountRef = useRef(0);

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
      console.log(`[AUTH] refreshAuth() #${callNum} - refresh_token succeeded, checking auth status...`);
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
   * Set up auto-refresh timer when authenticated
   */
  useEffect(() => {
    console.log("[AUTH] useEffect[auto-refresh] triggered:", {
      "authStatus": authStatus,
      "authStatus?.authenticated": authStatus?.authenticated,
      "refreshAuth reference": refreshAuth.toString().slice(0, 100),
    });

    if (!authStatus?.authenticated) {
      console.log("[AUTH] useEffect[auto-refresh] - NOT authenticated, skipping timer setup");
      return;
    }

    console.log("[AUTH] useEffect[auto-refresh] - IS authenticated, setting up token refresh timer");
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
