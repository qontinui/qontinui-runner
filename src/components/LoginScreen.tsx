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

export function LoginScreen() {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  const handleSignIn = async () => {
    log.debug("handleSignIn() called — starting Cognito PKCE sign-in");
    setLoading(true);
    setError("");

    try {
      await invoke<{
        userId: string;
        email: string | null;
        tenantId: string | null;
        deviceId: string;
      }>("cognito_sign_in", { backendUrl: DEFAULT_BACKEND_URL });
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
            disabled={loading}
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
