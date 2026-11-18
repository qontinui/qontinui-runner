/**
 * Tests for the pure logic behind PendingSignInNotice: when it renders, and
 * the SSH forward it recommends. (This repo's vitest runs in `node` with no
 * DOM harness, so the component itself is covered by these helpers.)
 */

import { describe, it, expect } from "vitest";
import { shouldShowPendingSignIn, sshForwardHint, type PendingSignIn } from "./PendingSignInNotice";

const base: PendingSignIn = {
  authorizeUrl: "https://auth.qontinui.io/oauth2/authorize?state=x",
  browserLaunchError: null,
  callbackPort: 53682,
  startedAtUnix: 1_794_990_000,
};

describe("shouldShowPendingSignIn", () => {
  it("shows nothing when no sign-in is pending", () => {
    expect(shouldShowPendingSignIn(null)).toBe(false);
  });

  it("shows nothing while the browser launch has not failed", () => {
    expect(shouldShowPendingSignIn(base)).toBe(false);
  });

  it("shows the URL once the browser could not be opened", () => {
    expect(
      shouldShowPendingSignIn({ ...base, browserLaunchError: 'Launcher "gio" "open" failed' }),
    ).toBe(true);
  });
});

describe("sshForwardHint", () => {
  it("forwards the callback port to this box's loopback", () => {
    expect(sshForwardHint(53682)).toBe("ssh -L 53682:127.0.0.1:53682 <this-box>");
  });
});
