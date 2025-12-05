/**
 * AuthProvider.tsx
 *
 * React context provider for authentication state management
 * Handles login/logout, token refresh, and auth status checking
 */

import { createContext, useContext, useState, useEffect, useCallback, ReactNode } from "react";
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

  /**
   * Check authentication status
   * Called on mount and after login
   */
  const checkAuthStatus = useCallback(async () => {
    try {
      setError(null);
      const status = await invoke<AuthStatus>("check_auth_status");
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
    try {
      setError(null);
      await invoke("refresh_token");
      await checkAuthStatus();
    } catch (err) {
      console.error("[AUTH] Failed to refresh token:", err);
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
    console.log("[AUTH] AuthProvider mounted, checking auth status");
    checkAuthStatus();
  }, [checkAuthStatus]);

  /**
   * Set up auto-refresh timer when authenticated
   */
  useEffect(() => {
    if (!authStatus?.authenticated) {
      return;
    }

    console.log("[AUTH] Setting up token refresh timer");
    const intervalId = setInterval(() => {
      console.log("[AUTH] Auto-refreshing token");
      refreshAuth();
    }, TOKEN_REFRESH_INTERVAL);

    return () => {
      console.log("[AUTH] Clearing token refresh timer");
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
