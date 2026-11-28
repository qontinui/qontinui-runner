import { describe, expect, it } from "vitest";

import { notUpgradeSafeTooltip, type HolderState } from "./holderState";

describe("notUpgradeSafeTooltip", () => {
  it("marks nothing for an upgrade-safe, remote or unread pane", () => {
    expect(
      notUpgradeSafeTooltip({ holder: true, upgradeSafe: true, notUpgradeSafe: null }),
    ).toBeNull();
    expect(notUpgradeSafeTooltip(null)).toBeNull();
    expect(notUpgradeSafeTooltip(undefined)).toBeNull();
  });

  it("names the fallback's kind for a pane whose holder could not start", () => {
    const state: HolderState = {
      holder: false,
      upgradeSafe: false,
      notUpgradeSafe: { reason: "holder_spawn_failed", kind: "child_enoent", detail: "boom" },
    };
    const text = notUpgradeSafeTooltip(state);
    expect(text).toContain("child_enoent");
    expect(text).toContain("fell back to an in-process PTY");
  });

  it("explains a disabled holder and an unprotected cgroup", () => {
    expect(
      notUpgradeSafeTooltip({
        holder: false,
        upgradeSafe: false,
        notUpgradeSafe: { reason: "holder_disabled" },
      }),
    ).toContain("terminal.pty_holder is off");
    expect(
      notUpgradeSafeTooltip({
        holder: true,
        upgradeSafe: false,
        notUpgradeSafe: { reason: "unprotected_cgroup", detail: "unprotected=cgroup" },
      }),
    ).toContain("systemd unit");
  });

  it("marks an adopted holder whose cgroup could not be read as unknown, not safe", () => {
    expect(
      notUpgradeSafeTooltip({
        holder: true,
        upgradeSafe: false,
        notUpgradeSafe: { reason: "cgroup_unknown", detail: "unread" },
      }),
    ).toContain("unknown");
  });
});
