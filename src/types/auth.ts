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
  /**
   * Always `false` under Cognito-only auth — the legacy dev email/password
   * auto-login was removed. Retained on the interface (and as a constant) so
   * existing consumers that gate on it keep compiling and behaving.
   */
  devAutoLoginPending: boolean;
  /**
   * Current runner tier. Surfaced so consumers can gate UI without calling
   * `useRunnerTier()` redundantly. Only "qontinui_account" requires a Qontinui
   * (Cognito) sign-in; "local" and "local_provider" run with a synthesized
   * local-guest auth.
   */
  tier: RunnerTier;
  /**
   * Default logout — clears only the interactive device-JWT session and keeps
   * the Cognito session, so the runner's autonomous terminal sessions keep
   * running (the device JWT is re-minted in the background).
   */
  logout: () => Promise<void>;
  /**
   * Full sign-out that STOPS autonomous terminal sessions — clears ALL
   * credentials (device JWT + the Cognito session). The runner cannot
   * self-recover its device JWT until an interactive re-login.
   */
  signOutFull: () => Promise<void>;
  /** Re-validate the current auth status (no token-refresh side effect). */
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
