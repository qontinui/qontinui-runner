/**
 * The runner's top-level auth gate: which of the four root surfaces does
 * `App` render — the loading shell, the setup wizard, the login screen, or the
 * app itself?
 *
 * Extracted as a pure function because the gate has one failure mode that is
 * invisible in review and expensive in practice: **rendering `LoginScreen` at a
 * user who is in fact signed in.** A logged-out-looking state has three very
 * different causes that the gate must not conflate:
 *
 *   - `authenticated === false` — a real, explicit sign-out. LoginScreen.
 *   - `authStatus === null` while the first probe is still being retried —
 *     NOT a sign-out, nothing is known yet. Loading shell (`authResolving`).
 *   - `authStatus === null` after the retries were exhausted — still not a
 *     sign-out, but the operator must be able to act, so: LoginScreen, while
 *     `AuthProvider` keeps re-probing in the background.
 *
 * The middle case is the one that regressed: `AuthProvider.loading` cannot
 * express it (a 3s failsafe forces that flag false so a wedged IPC channel can
 * never hang the app), so a retry chain that "stays in the loading shell" via
 * `loading` alone actually showed a sign-in prompt for most of its duration.
 * `authResolving` carries it instead, and this function is where that is
 * enforced — and tested.
 */

export type AuthGate = "loading" | "wizard" | "login" | "app";

export interface AuthGateInput {
  /** `AuthProvider.loading` — in-flight auth work, force-cleared after 3s. */
  authLoading: boolean;
  /** `AuthProvider.authResolving` — the FIRST probe has produced no verdict yet. */
  authResolving: boolean;
  /** Legacy dev auto-login in flight. Always false under Cognito-only auth. */
  devAutoLoginPending: boolean;
  /** The local API server has finished starting. */
  apiReady: boolean;
  /** `null` while still being read from disk; `false` means run the wizard. */
  setupCompleted: boolean | null;
  /** Tier 2 ("qontinui_account") is the only tier that requires a sign-in. */
  isTier2: boolean;
  /** `authStatus?.authenticated === true`. */
  authenticated: boolean;
}

export function resolveAuthGate(input: AuthGateInput): AuthGate {
  // Nothing may be decided from an unsettled auth state — least of all "show
  // them a sign-in prompt".
  if (input.authLoading || input.devAutoLoginPending || input.authResolving) {
    return "loading";
  }
  if (!input.apiReady) {
    return "loading";
  }
  // Wizard runs FIRST so Tier 0/1 setup can happen without ever hitting the
  // login screen.
  if (input.setupCompleted === false) {
    return "wizard";
  }
  // Tier 0/1 get a synthesized local-guest auth, so they never land here.
  if (input.isTier2 && !input.authenticated) {
    return "login";
  }
  return "app";
}
