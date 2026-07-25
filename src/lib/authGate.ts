/**
 * The runner's top-level auth gate: which root surface does `App` render — the
 * loading shell, the setup wizard, the login screen, the tier-unavailable hold
 * screen, or the app itself?
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

export type AuthGate = "loading" | "wizard" | "login" | "tier-unknown" | "app";

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
  /**
   * The runner tier could not be determined (settings.json unreadable, read
   * retries exhausted). The tier is neither Tier 2 nor a known local tier, so
   * we must HOLD on a dedicated screen rather than pick any tier-derived
   * surface. Never true while the tier is still loading (that is `authLoading`).
   */
  tierUnknown: boolean;
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
  // NO-DOWNGRADE: we could not determine the runner tier (settings.json
  // unreadable). Every gate below presumes a KNOWN tier — deciding
  // wizard/login/app from an unknown tier is precisely the silent downgrade this
  // screen exists to prevent (a Tier-2 runner would fall through to the
  // local-guest app shell). HOLD here; the tier is re-read when settings becomes
  // readable and this clears on its own. This never eats first-run: a fresh
  // install has a KNOWN default tier (a missing settings file is not an
  // unreadable one), so `tierUnknown` is false and the wizard still shows.
  if (input.tierUnknown) {
    return "tier-unknown";
  }
  // Wizard runs FIRST (of the tier-derived gates) so Tier 0/1 setup can happen
  // without ever hitting the login screen.
  if (input.setupCompleted === false) {
    return "wizard";
  }
  // Tier 0/1 get a synthesized local-guest auth, so they never land here.
  if (input.isTier2 && !input.authenticated) {
    return "login";
  }
  return "app";
}
