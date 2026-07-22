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
  /**
   * `true` while the FIRST auth probe has not yet produced a verdict and the
   * bounded retry chain is still running.
   *
   * Consumers that gate on `!authStatus?.authenticated` MUST also honour this,
   * or they render a sign-in prompt at a user who is in fact signed in: a null
   * `authStatus` is indistinguishable from a real sign-out at the gate, and
   * `loading` cannot carry this state (a 3s failsafe forces it back to `false`
   * so a wedged IPC channel can never hang the app). While this is `true` the
   * correct UI is the "checking authentication" shell, never `LoginScreen`.
   *
   * Always terminates: the chain is bounded (attempts x probe timeout), and
   * after it gives up a slow background re-probe keeps trying without holding
   * the UI hostage.
   */
  authResolving: boolean;
  error: string | null;
  /**
   * Always `false` under Cognito-only auth — the legacy dev email/password
   * auto-login was removed. Retained on the interface (and as a constant) so
   * existing consumers that gate on it keep compiling and behaving.
   */
  devAutoLoginPending: boolean;
  /**
   * Tier 2 only: the `check_auth_status` probe has not yet returned a
   * DEFINITIVE verdict — every attempt so far timed out or threw, and a
   * backoff retry is armed.
   *
   * Consumers must treat this as UNKNOWN, never as "signed out". Gating the
   * sign-in screen on `!authenticated` alone put fully-signed-in runners on
   * `LoginScreen` whenever the probe stalled (most visibly in freshly-opened
   * pop-out terminal windows, which probe while the app is still booting).
   * Always `false` in Tier 0/1, which never probes.
   */
  authProbeUnresolved: boolean;
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
