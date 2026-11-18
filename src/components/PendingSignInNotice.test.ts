/**
 * Tests for the pure logic behind PendingSignInNotice: when it renders, the
 * SSH forward it recommends, and how a user's own Cancel is told apart from a
 * failure. The component's polling and Cancel wiring are not rendered here.
 */

import { describe, it, expect } from "vitest";
import {
  LAUNCH_GRACE_SECS,
  isSignInCancelled,
  shouldShowPendingSignIn,
  sshForwardHint,
  type PendingSignIn,
} from "./PendingSignInNotice";

const base: PendingSignIn = {
  attemptId: 7,
  authorizeUrl: "https://auth.qontinui.io/oauth2/authorize?state=x",
  browserLaunchError: null,
  callbackPort: 53682,
  startedAtUnix: 1_794_990_000,
};

describe("shouldShowPendingSignIn", () => {
  const justStarted = base.startedAtUnix + 1;

  it("shows nothing when no sign-in is pending", () => {
    expect(shouldShowPendingSignIn(null, justStarted)).toBe(false);
  });

  it("shows nothing while a just-started launch has not failed", () => {
    expect(shouldShowPendingSignIn(base, justStarted)).toBe(false);
  });

  it("shows the URL once the browser could not be opened", () => {
    expect(
      shouldShowPendingSignIn(
        { ...base, browserLaunchError: 'Launcher "gio" "open" failed' },
        justStarted,
      ),
    ).toBe(true);
  });

  it("shows the URL anyway once a silent launch outlives the grace period", () => {
    expect(shouldShowPendingSignIn(base, base.startedAtUnix + LAUNCH_GRACE_SECS)).toBe(true);
  });
});

describe("isSignInCancelled", () => {
  it("recognises the user's own Cancel and nothing else", () => {
    expect(isSignInCancelled("sign-in cancelled")).toBe(true);
    expect(isSignInCancelled("sign-in timed out after 5 minutes")).toBe(false);
    expect(isSignInCancelled("sign-in superseded by a newer sign-in attempt")).toBe(false);
  });
});

describe("sshForwardHint", () => {
  it("forwards the callback port to this box's loopback", () => {
    expect(sshForwardHint(53682)).toBe("ssh -L 53682:127.0.0.1:53682 <this-box>");
  });
});
