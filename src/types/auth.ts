/**
 * Authentication type definitions
 *
 * TypeScript interfaces matching Rust auth structs
 */

export interface User {
  id: string;
  email: string;
  name: string | null;
}

export interface DeviceInfo {
  device_id: string;
  device_name: string;
  platform: "windows" | "darwin" | "linux";
}

export interface LoginResponse {
  access_token: string;
  refresh_token: string;
  user: User;
  device_info: DeviceInfo;
}

export interface AuthStatus {
  authenticated: boolean;
  user: User | null;
  device_info: DeviceInfo | null;
}

/**
 * Runner tier — duplicated locally rather than imported from the hook to
 * avoid a circular-ish dependency (this module is referenced by everything
 * that uses auth; the hook depends on Tauri APIs). Must match the union in
 * `hooks/useRunnerTier.ts`.
 */
export type RunnerTier = "local" | "local_provider" | "qontinui_account";

export interface AuthContextValue {
  authStatus: AuthStatus | null;
  loading: boolean;
  error: string | null;
  /** True in dev mode until auto-login completes or is skipped */
  devAutoLoginPending: boolean;
  /**
   * Current runner tier. Surfaced so consumers can gate UI without calling
   * `useRunnerTier()` redundantly. Only "qontinui_account" requires a
   * qontinui-account JWT; "local" and "local_provider" run with a
   * synthesized local-guest auth.
   */
  tier: RunnerTier;
  login: (email: string, password: string) => Promise<void>;
  logout: () => Promise<void>;
  refreshAuth: () => Promise<void>;
}

export interface ConnectionInfo {
  device_id: string;
  websocket_url: string;
  http_url: string;
  user_id: string;
  is_active: boolean;
}

export interface Project {
  id: string;
  name: string;
  description: string | null;
  created_at: string;
  updated_at: string;
}
