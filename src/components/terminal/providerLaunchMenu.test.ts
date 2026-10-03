/**
 * The provider launch menu's row verdicts (`providerLaunchMenu.ts`): every
 * served profile is listed, an absent CLI is disabled with its install
 * command, and a probe that could not decide reads UNKNOWN, never absent.
 */

import { describe, expect, it } from "vitest";

import served from "../../../src-tauri/tests/fixtures/cli_screens/served_profiles.json";
import {
  installOsFor,
  launchEntry,
  structuredLaunchOffer,
  type CliAvailability,
  type LaunchMenuProfile,
} from "./providerLaunchMenu";

const profiles = served as LaunchMenuProfile[];
const codex = (): LaunchMenuProfile => {
  const p = profiles.find((x) => x.id === "codex");
  if (!p) throw new Error("served_profiles.json has no codex profile");
  return p;
};

describe("installOsFor", () => {
  it("maps navigator.platform strings to the install-command key", () => {
    expect(installOsFor("Win32")).toBe("windows");
    expect(installOsFor("MacIntel")).toBe("macos");
    expect(installOsFor("Linux x86_64")).toBe("linux");
    expect(installOsFor("")).toBe("linux");
  });
});

describe("launchEntry", () => {
  it("lists every served profile, Claude and Codex included", () => {
    expect(profiles.map((p) => p.id)).toEqual(expect.arrayContaining(["claude", "codex"]));
  });

  it("enables an available CLI and shows its version", () => {
    const a: CliAvailability = { id: "codex", available: true, version: "codex-cli 0.159.1" };
    expect(launchEntry(codex(), a, "linux")).toMatchObject({
      state: "available",
      enabled: true,
      detail: "codex-cli 0.159.1",
      installCommand: null,
    });
  });

  it("disables an absent CLI with the reason and this OS's install command — never hides it", () => {
    const a: CliAvailability = {
      id: "codex",
      available: false,
      error: "`codex` is not on the runner's PATH",
    };
    const entry = launchEntry(codex(), a, "windows");
    expect(entry).toMatchObject({
      id: "codex",
      label: "Codex CLI",
      state: "absent",
      enabled: false,
      detail: "`codex` is not on the runner's PATH",
      installCommand: "npm i -g @openai/codex",
    });
  });

  it("reads a failed probe — or one that never ran — as UNKNOWN, not absent", () => {
    const failed: CliAvailability = {
      id: "codex",
      available: null,
      error: "`/usr/bin/codex --version` did not answer within 10s",
    };
    const entry = launchEntry(codex(), failed, "linux");
    expect(entry.state).toBe("unknown");
    expect(entry.enabled).toBe(true);
    expect(entry.detail).toContain("UNKNOWN");
    expect(entry.detail).toContain("did not answer");

    const never = launchEntry(codex(), null, "linux", "IPC failed");
    expect(never.state).toBe("unknown");
    expect(never.detail).toContain("IPC failed");
  });

  it("is not launchable while its probe is in flight", () => {
    expect(launchEntry(codex(), undefined, "linux")).toMatchObject({
      state: "probing",
      enabled: false,
    });
  });
});

describe("structuredLaunchOffer (Phase 9)", () => {
  const claude = (): LaunchMenuProfile => {
    const p = profiles.find((x) => x.id === "claude");
    if (!p) throw new Error("served_profiles.json has no claude profile");
    return p;
  };

  it("offers a structured session for Claude — the lane the runner speaks", () => {
    expect(structuredLaunchOffer(claude())).toEqual({ offered: true });
  });

  it("never offers one for Codex, whose app-server lane the runner does not implement", () => {
    const verdict = structuredLaunchOffer(codex());
    expect(verdict.offered).toBe(false);
    if (!verdict.offered) expect(verdict.reason).toContain("codex_app_server");
  });

  it("refuses an unverified permission capability or a profile with no prompt arguments", () => {
    expect(structuredLaunchOffer({ ...claude(), typedPermission: "unknown" }).offered).toBe(false);
    expect(structuredLaunchOffer({ ...claude(), permissionPromptArgs: [] }).offered).toBe(false);
    expect(structuredLaunchOffer({ id: "x", displayName: "X" }).offered).toBe(false);
  });
});
