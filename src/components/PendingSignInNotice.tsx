/**
 * PendingSignInNotice — what a Hosted-UI sign-in shows when the runner could
 * not open a browser.
 *
 * `cognito_sign_in` opens the system browser and then waits up to five minutes
 * for the loopback callback. On a box with no browser launcher (a headless
 * Linux box with no `xdg-open`, measured 2026-11-15) the launch fails, and
 * before this notice the only trace was a log line: the UI waited five silent
 * minutes. While `active`, this polls `cognito_pending_sign_in` and, once the
 * launch has failed, shows the authorize URL with a copy control, the SSH
 * forward a browser on another machine needs, and a Cancel that releases the
 * callback port (`cognito_cancel_sign_in`).
 *
 * Plan 2026-11-15-headless-runner-recovers-its-coord-credential-without-a-human,
 * P1.1 / P1.2.
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { createLogger } from "@/lib/logger";

/** Wire shape of `cognito_pending_sign_in` (Rust `cognito::PendingSignIn`). */
export interface PendingSignIn {
  attemptId: number;
  authorizeUrl: string;
  browserLaunchError: string | null;
  callbackPort: number;
  startedAtUnix: number;
}

const log = createLogger("PendingSignInNotice");

/** How often the notice re-reads the pending sign-in while active. */
const POLL_MS = 1000;

/**
 * The SSH command that carries the callback from a browser on another machine
 * back to this runner. Pure, so it is unit-testable.
 */
export function sshForwardHint(callbackPort: number): string {
  return `ssh -L ${callbackPort}:127.0.0.1:${callbackPort} <this-box>`;
}

/**
 * How long a sign-in may wait with no launch error before the URL is shown
 * anyway: a launcher that hangs, or exits 0 onto a display nobody watches,
 * reports no error at all.
 */
export const LAUNCH_GRACE_SECS = 8;

/** The error a user's own Cancel makes `cognito_sign_in` return (Rust `cognito::SIGN_IN_CANCELLED`). */
export const SIGN_IN_CANCELLED = "sign-in cancelled";

/** The error a newer sign-in makes the one it replaced return (Rust `cognito::SIGN_IN_SUPERSEDED`). */
export const SIGN_IN_SUPERSEDED = "sign-in superseded by a newer sign-in attempt";

/**
 * Whether a `cognito_sign_in` rejection is the user's own Cancel, or a newer
 * attempt replacing this one: neither is a failure to show.
 */
export function isSignInCancelled(err: unknown): boolean {
  const text = String(err);
  return text === SIGN_IN_CANCELLED || text === SIGN_IN_SUPERSEDED;
}

/** Whether the notice has anything to show for `pending` at `nowUnix`. */
export function shouldShowPendingSignIn(pending: PendingSignIn | null, nowUnix: number): boolean {
  if (pending === null) return false;
  return (
    pending.browserLaunchError !== null || nowUnix - pending.startedAtUnix >= LAUNCH_GRACE_SECS
  );
}

/**
 * The no-browser door each place that renders the notice can actually reach.
 * The sign-in screen and the setup wizard own the whole viewport, so Settings
 * is out of reach there.
 */
export const NO_BROWSER_HINT = {
  loginScreen: "Cancel, then use the email-and-password form on this screen. It needs no browser.",
  settings:
    "Cancel, then redeem a pair code under Settings → Backend Connection. It needs no browser.",
  backendConnection: "Cancel, then redeem a pair code in the section below. It needs no browser.",
  setupWizard:
    "Cancel, then pair from a shell on this box with: qontinui_profile device pair --pair-code <CODE>. It needs no browser.",
} as const;

export function PendingSignInNotice({
  active,
  noBrowserHint,
}: {
  active: boolean;
  /** The no-browser door reachable from where this notice renders ({@link NO_BROWSER_HINT}). */
  noBrowserHint: string;
}) {
  const [pending, setPending] = useState<PendingSignIn | null>(null);
  // When `pending` was read: render stays pure, and the 1 s poll moves it.
  const [readAtUnix, setReadAtUnix] = useState(0);
  const [copied, setCopied] = useState(false);
  const [cancelling, setCancelling] = useState(false);

  useEffect(() => {
    if (!active) return;
    let stopped = false;
    const read = async () => {
      try {
        const next = await invoke<PendingSignIn | null>("cognito_pending_sign_in");
        if (!stopped) {
          setPending(next);
          setReadAtUnix(Date.now() / 1000);
        }
      } catch {
        // A runner build without the command: nothing to show.
        if (!stopped) setPending(null);
      }
    };
    void read();
    const timer = window.setInterval(() => void read(), POLL_MS);
    return () => {
      stopped = true;
      window.clearInterval(timer);
      // The next sign-in starts from nothing, never from this one's URL.
      setPending(null);
      setCopied(false);
    };
  }, [active]);

  if (!active || pending === null || !shouldShowPendingSignIn(pending, readAtUnix)) {
    return null;
  }

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(pending.authorizeUrl);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  };

  const handleCancel = async () => {
    setCancelling(true);
    try {
      await invoke<boolean>("cognito_cancel_sign_in", { attemptId: pending.attemptId });
    } catch (err) {
      log.error("cognito_cancel_sign_in failed:", err);
    } finally {
      setCancelling(false);
    }
  };

  return (
    <div
      role="status"
      data-testid="pending-sign-in-notice"
      style={{
        display: "flex",
        flexDirection: "column",
        gap: 6,
        marginTop: 8,
        fontSize: "0.8125rem",
        textAlign: "left",
      }}
    >
      <div>
        {pending.browserLaunchError !== null
          ? `The runner could not open a browser (${pending.browserLaunchError}).`
          : "Browser didn't open?"}{" "}
        Open this URL in a browser to finish signing in:
      </div>
      <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
        <input
          readOnly
          aria-label="Sign-in URL"
          value={pending.authorizeUrl}
          onFocus={(e) => e.currentTarget.select()}
          style={{
            flex: 1,
            minWidth: 0,
            fontFamily: "monospace",
            fontSize: "0.75rem",
            padding: "2px 6px",
            background: "transparent",
            color: "inherit",
            border: "1px solid var(--border, #3f3f46)",
            borderRadius: 4,
          }}
        />
        <button type="button" className="btn-secondary" onClick={() => void handleCopy()}>
          {copied ? "Copied" : "Copy"}
        </button>
        <button
          type="button"
          className="btn-secondary"
          onClick={() => void handleCancel()}
          disabled={cancelling}
        >
          Cancel
        </button>
      </div>
      <div style={{ opacity: 0.75 }}>No browser to use? {noBrowserHint}</div>
      <div style={{ opacity: 0.75 }}>
        If the browser is on another machine, forward the callback port first:{" "}
        <code>{sshForwardHint(pending.callbackPort)}</code>
      </div>
    </div>
  );
}
