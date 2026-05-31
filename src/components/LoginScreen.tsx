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

export function LoginScreen() {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  // Inline email/password (direct Cognito InitiateAuth — no browser).
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [pwLoading, setPwLoading] = useState(false);

  const handleSignIn = async () => {
    log.debug("handleSignIn() called — starting Cognito PKCE sign-in");
    setLoading(true);
    setError("");

    try {
      await invoke<CognitoSignInResponse>("cognito_sign_in", {
        backendUrl: DEFAULT_BACKEND_URL,
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

          {/* Sign-in Button */}
          <button
            type="button"
            onClick={handleSignIn}
            disabled={loading || pwLoading}
            data-testid="cognito-sign-in"
            className="w-full btn-primary py-3 text-base font-semibold flex items-center justify-center gap-2"
          >
            {loading ? (
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
