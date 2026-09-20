/**
 * The remote-close answer the frontend used to throw away.
 *
 * PR #1562 taught `terminal_close` to report what a remote tab's close did
 * about the relay binding, and `useTerminalManager.closeTerminal` discarded
 * the whole response — so a detach that never reached the relay looked
 * exactly like one that did, because the tab vanishes either way. These pin
 * the two pure halves of the fix: which responses are remote at all, and
 * which of those are clean enough to stay silent about.
 *
 * (The runner's vitest environment is `node` — no jsdom — so this covers the
 * decision, not the markup.)
 */

import { describe, expect, it } from "vitest";
import {
  isCleanRemoteClose,
  parseRemoteDetach,
  type RemoteDetachReport,
} from "./useTerminalManager";

const report = (over: Partial<RemoteDetachReport> = {}): RemoteDetachReport => ({
  outcome: "queued",
  error: null,
  relayPumpAttached: true,
  targetDeviceId: "device-1",
  remoteTerminalId: "490212f5-aaaa-bbbb-cccc-dddddddddddd",
  ...over,
});

describe("parseRemoteDetach", () => {
  it("reads the runner's remoteDetach object", () => {
    const parsed = parseRemoteDetach({
      remoteDetach: {
        outcome: "failed",
        error: "outbound backlog full",
        relayPumpAttached: false,
        targetDeviceId: "device-1",
        remoteTerminalId: "490212f5",
      },
    });
    expect(parsed).toEqual({
      outcome: "failed",
      error: "outbound backlog full",
      relayPumpAttached: false,
      targetDeviceId: "device-1",
      remoteTerminalId: "490212f5",
    });
  });

  it("keeps a mid-close pump change as null, never false", () => {
    // `null` is the runner saying the relay connection CHANGED across the
    // close, so delivery is unknown. Coercing it to `false` would turn an
    // unknown into a confident claim.
    const parsed = parseRemoteDetach({
      remoteDetach: { outcome: "queued", relayPumpAttached: null },
    });
    expect(parsed?.relayPumpAttached).toBeNull();
  });

  it("is null for a local tab and for anything it cannot read", () => {
    // A local tab's response carries no `remoteDetach` at all; the rest are
    // shapes a future/rolled-back runner could send. None of them may
    // manufacture a notice about a tab that was never remote.
    expect(parseRemoteDetach(null)).toBeNull();
    expect(parseRemoteDetach(undefined)).toBeNull();
    expect(parseRemoteDetach({})).toBeNull();
    expect(parseRemoteDetach({ remoteDetach: null })).toBeNull();
    expect(parseRemoteDetach({ remoteDetach: "queued" })).toBeNull();
    expect(parseRemoteDetach({ remoteDetach: { error: "no outcome field" } })).toBeNull();
  });
});

describe("isCleanRemoteClose", () => {
  it("is clean only when the detach queued on a steadily-attached pump", () => {
    expect(isCleanRemoteClose(report())).toBe(true);
  });

  it("notices every outcome that can leave the target claimed", () => {
    // queued but nothing draining the queue — discarded on reconnect
    expect(isCleanRemoteClose(report({ relayPumpAttached: false }))).toBe(false);
    // queued but the connection changed mid-close — UNKNOWN, so notice it
    expect(isCleanRemoteClose(report({ relayPumpAttached: null }))).toBe(false);
    // never queued at all
    expect(
      isCleanRemoteClose(report({ outcome: "failed", error: "channel closed" })),
    ).toBe(false);
    expect(isCleanRemoteClose(report({ outcome: "not_attempted" }))).toBe(false);
    // no pane found for the tab
    expect(isCleanRemoteClose(report({ outcome: "unknown" }))).toBe(false);
  });

  it("never treats an unrecognised outcome as clean", () => {
    // A runner newer than this webview may add an outcome; defaulting an
    // unknown one to "clean" would silence exactly the case nobody has
    // reasoned about yet.
    expect(isCleanRemoteClose(report({ outcome: "released" }))).toBe(false);
  });
});
