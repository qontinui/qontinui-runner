/**
 * LoginScreen.tsx
 *
 * Tier-2 sign-in gate. The web backend authenticates via Cognito only — there
 * is no local email/password login anymore — so this screen drives the runner's
 * Cognito Hosted-UI sign-in (RFC 8252 PKCE, system browser) via the
 * `cognito_sign_in` Tauri command. On success the runner is promoted to Tier 2
 * (server-side, inside the command) and emits `runner-tier-changed`, which
 * AuthProvider observes to re-check auth and leave this screen.
 *
 * Matches qontinui-runner's dark gaming theme with neon accents.
 */

import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { LogIn, AlertCircle, Loader2 } from "lucide-react";
import { createLogger } from "@/lib/logger";

const log = createLogger("LoginScreen");

/** Web backend the Cognito-bound device JWT is minted against. */
const DEFAULT_BACKEND_URL = "https://api.qontinui.io";

type CognitoSignInResponse = {
  userId: string;
  email: string | null;
  tenantId: string | null;
  deviceId: string;
};

/**
 * Federated identity providers configured on the Cognito app client. These are
 * the exact `identity_provider` values Cognito expects and MUST match the web
 * auth UI verbatim (`qontinui-web/.../services/auth/cognito-oauth.ts`). The
 * runner threads the selected value into the `cognito_sign_in` Tauri command,
 * which appends `&identity_provider=<Provider>` to the Hosted-UI authorize URL.
 */
type CognitoProvider = "Google" | "MicrosoftEntra" | "GitHub";

export function LoginScreen() {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  // Inline email/password (direct Cognito InitiateAuth — no browser).
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [pwLoading, setPwLoading] = useState(false);
  // Which Hosted-UI sign-in is in flight: a federated provider, "qontinui"
  // (the native chooser/email screen), or null. Disables the other buttons and
  // shows a per-button spinner.
  const [pending, setPending] = useState<CognitoProvider | "qontinui" | null>(
    null
  );

  /**
   * Start a Hosted-UI Cognito sign-in. `provider` jumps straight into a
   * federated IdP via `identity_provider`; omitting it preserves the native
   * "Sign in with Qontinui" chooser/email path (param omitted server-side).
   */
  const handleSignIn = async (provider?: CognitoProvider) => {
    log.debug(
      `handleSignIn(${provider ?? "qontinui"}) called — starting Cognito PKCE sign-in`
    );
    setLoading(true);
    setPending(provider ?? "qontinui");
    setError("");

    try {
      await invoke<CognitoSignInResponse>("cognito_sign_in", {
        backendUrl: DEFAULT_BACKEND_URL,
        identityProvider: provider,
      });
      log.debug("Cognito sign-in completed — runner promoted to Tier 2");
      // The command already promoted the runner to Tier 2; nudge tier consumers
      // (notably AuthProvider) so the gate re-evaluates without a poll cycle.
      window.dispatchEvent(new CustomEvent("runner-tier-changed"));
    } catch (err) {
      log.error("Cognito sign-in failed:", err);
      setError(typeof err === "string" ? err : String(err));
    } finally {
      setLoading(false);
      setPending(null);
    }
  };

  const handlePasswordSignIn = async () => {
    log.debug("handlePasswordSignIn() called — direct Cognito USER_PASSWORD_AUTH");
    setPwLoading(true);
    setError("");

    try {
      await invoke<CognitoSignInResponse>("cognito_sign_in_password", {
        email,
        password,
        backendUrl: DEFAULT_BACKEND_URL,
      });
      log.debug("Password sign-in completed — runner promoted to Tier 2");
      // Same as the hosted-UI path: the command already promoted to Tier 2,
      // nudge tier consumers so the gate re-evaluates immediately.
      window.dispatchEvent(new CustomEvent("runner-tier-changed"));
    } catch (err) {
      log.error("Password sign-in failed:", err);
      setError(typeof err === "string" ? err : String(err));
    } finally {
      setPwLoading(false);
    }
  };

  return (
    <div className="min-h-screen bg-background grid-dots flex items-center justify-center p-6">
      <div className="w-full max-w-md space-y-8">
        {/* Logo and Title */}
        <div className="text-center space-y-2">
          <h1 className="text-5xl font-bold bg-gradient-to-r from-primary via-secondary to-accent bg-clip-text text-transparent">
            Qontinui
          </h1>
          <p className="text-xl font-semibold text-foreground">Runner</p>
          <p className="text-sm text-muted-foreground mt-4">
            Sign in to connect your desktop runner
          </p>
        </div>

        {/* Sign-in Card */}
        <div className="card p-8 space-y-6 glow-cyan">
          <p className="text-sm text-muted-foreground text-center">
            Sign in with your Qontinui account. A secure browser window opens to
            complete authentication, then returns you here.
          </p>

          {/* Error Message */}
          {error && (
            <div
              id="login-error"
              data-testid="login-error"
              role="alert"
              aria-live="assertive"
              className="flex items-start gap-2 p-3 bg-destructive/10 border border-destructive/50 rounded-lg animate-slideDown"
            >
              <AlertCircle className="w-5 h-5 text-destructive shrink-0 mt-0.5" />
              <p className="text-sm text-destructive">{error}</p>
            </div>
          )}

          {/* Federated social sign-in (Cognito Hosted UI per IdP). Labels,
              ordering, and provider names match the web auth UI verbatim. */}
          <div className="space-y-3">
            <button
              type="button"
              onClick={() => handleSignIn("Google")}
              disabled={loading || pwLoading}
              data-testid="cognito-sign-in-google"
              className="w-full btn-secondary py-3 text-base font-semibold flex items-center justify-center gap-2"
            >
              {pending === "Google" ? (
                <>
                  <Loader2 className="w-5 h-5 animate-spin" />
                  <span>Redirecting to Google...</span>
                </>
              ) : (
                <>
                  <GoogleIcon className="w-5 h-5" />
                  <span>Continue with Google</span>
                </>
              )}
            </button>
            <button
              type="button"
              onClick={() => handleSignIn("MicrosoftEntra")}
              disabled={loading || pwLoading}
              data-testid="cognito-sign-in-microsoft"
              className="w-full btn-secondary py-3 text-base font-semibold flex items-center justify-center gap-2"
            >
              {pending === "MicrosoftEntra" ? (
                <>
                  <Loader2 className="w-5 h-5 animate-spin" />
                  <span>Redirecting to Microsoft...</span>
                </>
              ) : (
                <>
                  <MicrosoftIcon className="w-5 h-5" />
                  <span>Continue with Microsoft</span>
                </>
              )}
            </button>
            <button
              type="button"
              onClick={() => handleSignIn("GitHub")}
              disabled={loading || pwLoading}
              data-testid="cognito-sign-in-github"
              className="w-full btn-secondary py-3 text-base font-semibold flex items-center justify-center gap-2"
            >
              {pending === "GitHub" ? (
                <>
                  <Loader2 className="w-5 h-5 animate-spin" />
                  <span>Redirecting to GitHub...</span>
                </>
              ) : (
                <>
                  <GitHubIcon className="w-5 h-5" />
                  <span>Continue with GitHub</span>
                </>
              )}
            </button>
          </div>

          {/* Sign-in Button */}
          <button
            type="button"
            onClick={() => handleSignIn()}
            disabled={loading || pwLoading}
            data-testid="cognito-sign-in"
            className="w-full btn-primary py-3 text-base font-semibold flex items-center justify-center gap-2"
          >
            {pending === "qontinui" ? (
              <>
                <Loader2 className="w-5 h-5 animate-spin" />
                <span>Opening browser…</span>
              </>
            ) : (
              <>
                <LogIn className="w-5 h-5" />
                <span>Sign in with Qontinui</span>
              </>
            )}
          </button>

          {/* Divider: "or sign in with email" */}
          <div className="flex items-center gap-3" aria-hidden="true">
            <span className="flex-1 h-px bg-border/50" />
            <span className="text-xs uppercase tracking-wide text-muted-foreground">
              or sign in with email
            </span>
            <span className="flex-1 h-px bg-border/50" />
          </div>

          {/* Inline credential sign-in (direct Cognito, no browser) */}
          <form
            className="space-y-3"
            onSubmit={(e) => {
              e.preventDefault();
              if (!pwLoading && !loading) void handlePasswordSignIn();
            }}
          >
            <div className="space-y-1">
              <label
                htmlFor="email"
                className="block text-sm font-medium text-foreground"
              >
                Email
              </label>
              <input
                id="email"
                name="email"
                type="email"
                autoComplete="username"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                disabled={pwLoading}
                className="w-full px-3 py-2 rounded-lg bg-background border border-border/60 text-foreground focus:outline-none focus:border-primary"
                placeholder="you@example.com"
              />
            </div>
            <div className="space-y-1">
              <label
                htmlFor="password"
                className="block text-sm font-medium text-foreground"
              >
                Password
              </label>
              <input
                id="password"
                name="password"
                type="password"
                autoComplete="current-password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                disabled={pwLoading}
                className="w-full px-3 py-2 rounded-lg bg-background border border-border/60 text-foreground focus:outline-none focus:border-primary"
                placeholder="••••••••"
              />
            </div>
            <button
              id="button-sign-in-password"
              type="submit"
              disabled={pwLoading || loading}
              data-testid="cognito-sign-in-password"
              className="w-full btn-secondary py-3 text-base font-semibold flex items-center justify-center gap-2"
            >
              {pwLoading ? (
                <>
                  <Loader2 className="w-5 h-5 animate-spin" />
                  <span>Signing in…</span>
                </>
              ) : (
                <>
                  <LogIn className="w-5 h-5" />
                  <span>Sign In</span>
                </>
              )}
            </button>
          </form>

          {/* Help Text */}
          <div className="text-center pt-4 border-t border-border/50">
            <p className="text-sm text-muted-foreground">
              Don't have an account?{" "}
              <a
                href="https://qontinui.io"
                target="_blank"
                rel="noopener noreferrer"
                className="text-primary hover:text-primary/80 font-medium transition-colors"
              >
                Sign up on qontinui.io
              </a>
            </p>
          </div>
        </div>

        {/* Footer */}
        <div className="text-center text-xs text-muted-foreground">
          <p>Qontinui Runner v0.1.0</p>
          <p className="mt-1">Secure authentication via Cognito + OS keychain</p>
        </div>
      </div>
    </div>
  );
}

/** Google "G" brand mark (multi-color), used on the social sign-in button. */
function GoogleIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      aria-hidden="true"
      focusable="false"
    >
      <path
        fill="#4285F4"
        d="M22.56 12.25c0-.78-.07-1.53-.2-2.25H12v4.26h5.92a5.06 5.06 0 0 1-2.2 3.32v2.77h3.57c2.08-1.92 3.27-4.74 3.27-8.1Z"
      />
      <path
        fill="#34A853"
        d="M12 23c2.97 0 5.46-.98 7.28-2.66l-3.57-2.77c-.98.66-2.23 1.06-3.71 1.06-2.86 0-5.29-1.93-6.16-4.53H2.18v2.84A11 11 0 0 0 12 23Z"
      />
      <path
        fill="#FBBC05"
        d="M5.84 14.1a6.6 6.6 0 0 1 0-4.2V7.06H2.18a11 11 0 0 0 0 9.88l3.66-2.84Z"
      />
      <path
        fill="#EA4335"
        d="M12 5.38c1.62 0 3.06.56 4.21 1.64l3.15-3.15C17.45 2.09 14.97 1 12 1A11 11 0 0 0 2.18 7.06l3.66 2.84C6.71 7.3 9.14 5.38 12 5.38Z"
      />
    </svg>
  );
}

/** Microsoft four-square brand mark, used on the social sign-in button. */
function MicrosoftIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      aria-hidden="true"
      focusable="false"
    >
      <path fill="#F25022" d="M2 2h9.5v9.5H2z" />
      <path fill="#7FBA00" d="M12.5 2H22v9.5h-9.5z" />
      <path fill="#00A4EF" d="M2 12.5h9.5V22H2z" />
      <path fill="#FFB900" d="M12.5 12.5H22V22h-9.5z" />
    </svg>
  );
}

/** GitHub octocat brand mark, used on the social sign-in button. */
function GitHubIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      aria-hidden="true"
      focusable="false"
      fill="currentColor"
    >
      <path d="M12 .5a12 12 0 0 0-3.79 23.39c.6.11.82-.26.82-.58v-2.03c-3.34.73-4.04-1.61-4.04-1.61-.55-1.39-1.34-1.76-1.34-1.76-1.09-.74.08-.73.08-.73 1.2.09 1.84 1.24 1.84 1.24 1.07 1.84 2.81 1.31 3.5 1 .11-.78.42-1.31.76-1.61-2.67-.3-5.47-1.34-5.47-5.96 0-1.32.47-2.39 1.24-3.23-.12-.31-.54-1.53.12-3.18 0 0 1.01-.32 3.3 1.23a11.5 11.5 0 0 1 6 0c2.29-1.55 3.3-1.23 3.3-1.23.66 1.65.24 2.87.12 3.18.77.84 1.24 1.91 1.24 3.23 0 4.63-2.81 5.65-5.49 5.95.43.37.81 1.1.81 2.22v3.29c0 .32.22.7.83.58A12 12 0 0 0 12 .5Z" />
    </svg>
  );
}
