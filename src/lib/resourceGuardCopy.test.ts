import { describe, expect, it } from "vitest";

import { GRANT_SPAWN_LIMIT, type PendingResourceBlock } from "./resourceGuard";
import {
  RESOURCE_GUARD_HEADLINES,
  grantAnnouncement,
  grantBannerText,
  resourceGuardDialogCopy,
  type GuardOs,
} from "./resourceGuardCopy";
import wire from "./resourceGuardWire.fixture.json";

/**
 * The dialog's words (plan
 * `2026-10-01-runner-thread-ceilings-ignore-the-machine-and-the-guard-dialog-says-low-memory`
 * Phases 0 and 3). The incident this pins: a thread-lane refusal on a Linux box
 * with 205 GB free was titled "Low memory" and told the operator Windows might
 * kill a session.
 */

const ALL_OS: GuardOs[] = ["windows", "linux", "macos", "other"];

function block(overrides: Partial<PendingResourceBlock> = {}): PendingResourceBlock {
  return {
    metric: "thread_count",
    message: "Not starting a new terminal session: …",
    pending: 1,
    source: null,
    ...overrides,
  };
}

describe("dialog title", () => {
  it("uses the Rust headlines byte for byte (shared fixture)", () => {
    expect(RESOURCE_GUARD_HEADLINES.free_commit_bytes).toBe(wire.headlines.free_commit_bytes);
    expect(RESOURCE_GUARD_HEADLINES.thread_count).toBe(wire.headlines.thread_count);
  });

  it("titles each lane by its own headline", () => {
    expect(resourceGuardDialogCopy(block({ metric: "thread_count" }), "linux").title).toBe(
      "High thread count",
    );
    expect(resourceGuardDialogCopy(block({ metric: "free_commit_bytes" }), "windows").title).toBe(
      "Low memory",
    );
  });

  it("gives a refusal that named no lane a neutral title, never 'Low memory'", () => {
    const copy = resourceGuardDialogCopy(block({ metric: "unknown" }), "windows");
    expect(copy.title).toBe("Resource limit reached");
    expect(copy.title).not.toContain("memory");
  });

  it("names the coalesced count and switches to the all/none buttons", () => {
    const one = resourceGuardDialogCopy(block(), "linux");
    expect([one.confirmText, one.cancelText]).toEqual(["Start anyway", "Don't start"]);
    const many = resourceGuardDialogCopy(block({ pending: 4 }), "linux");
    expect(many.title).toBe("High thread count — 4 pending starts");
    expect([many.confirmText, many.cancelText]).toEqual(["Start all anyway", "Start none"]);
  });
});

describe("dialog description", () => {
  it("never mentions Windows on Linux, macOS or an unknown platform — on any lane", () => {
    for (const os of ["linux", "macos", "other"] as GuardOs[]) {
      for (const metric of ["free_commit_bytes", "thread_count", "unknown"] as const) {
        expect(resourceGuardDialogCopy(block({ metric }), os).description).not.toContain("Windows");
      }
    }
  });

  it("names Windows only for the memory lane on Windows", () => {
    expect(
      resourceGuardDialogCopy(block({ metric: "free_commit_bytes" }), "windows").description,
    ).toContain("Windows to kill");
    expect(
      resourceGuardDialogCopy(block({ metric: "free_commit_bytes" }), "linux").description,
    ).toContain("Linux out-of-memory killer");
  });

  it("gives the thread lane its own remedy, not 'close a build', on every OS", () => {
    for (const os of ALL_OS) {
      const d = resourceGuardDialogCopy(block({ metric: "thread_count" }), os).description;
      expect(d).toContain("closing idle terminals");
      expect(d).not.toContain("build");
      expect(d).not.toContain("Windows");
    }
  });

  it("says how far 'Start anyway' reaches on a named lane", () => {
    const d = resourceGuardDialogCopy(block({ metric: "thread_count" }), "linux").description;
    expect(d).toContain(`up to ${GRANT_SPAWN_LIMIT} more refused thread count starts`);
    expect(d).toContain("60 s after the last one (5 min at most)");
    expect(d).toContain("revoke");
  });

  it("promises no grant for an unnamed lane", () => {
    const d = resourceGuardDialogCopy(block({ metric: "unknown" }), "linux").description;
    expect(d).toContain("for this start only");
    expect(d).not.toContain("more refused");
  });

  it("names the caller and its queue, as an upper bound", () => {
    const d = resourceGuardDialogCopy(
      block({ source: { label: "session restore", queued: 281 } }),
      "linux",
    ).description;
    expect(d.startsWith("Requested by session restore — up to 281 starts queued.")).toBe(true);
    expect(
      resourceGuardDialogCopy(block({ source: { label: "new terminal" } }), "linux").description,
    ).toMatch(/^Requested by new terminal\. /);
  });

  it("keeps the Rust message verbatim", () => {
    expect(resourceGuardDialogCopy(block({ message: "exact text" }), "linux").message).toBe(
      "exact text",
    );
  });
});

describe("grantBannerText", () => {
  const grant = {
    metric: "thread_count" as const,
    remaining: 7,
    expiresAt: 100_000,
    sourceLabel: "session restore",
  };

  it("names the lane, the starts and seconds left, and the source", () => {
    expect(grantBannerText(grant, 58_000)).toBe(
      "Resource guard overridden for thread count — 7 starts, 42 s left (session restore)",
    );
  });

  it("rounds seconds up and never reads 0 s while live", () => {
    expect(grantBannerText({ ...grant, remaining: 1 }, 99_900)).toBe(
      "Resource guard overridden for thread count — 1 start, 1 s left (session restore)",
    );
  });

  it("announces without the countdown, so a live region speaks on change, not every second", () => {
    expect(grantAnnouncement(grant)).toBe(
      "Resource guard overridden for thread count — 7 starts left",
    );
  });

  it("omits a missing source", () => {
    expect(
      grantBannerText({ ...grant, metric: "free_commit_bytes", sourceLabel: null }, 90_000),
    ).toBe("Resource guard overridden for memory — 7 starts, 10 s left");
  });
});
