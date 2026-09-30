/**
 * Tests for the served-profile provider registry (plan
 * `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
 * Phase 4). The descriptors are built from the runner's served CLI profiles;
 * these tests prime the cache from the checked-in snapshot the Rust suite pins
 * to the live manifest (`src-tauri/tests/fixtures/cli_screens/served_profiles.json`).
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

import served from "../../../src-tauri/tests/fixtures/cli_screens/served_profiles.json";
import {
  cliProfilesLoaded,
  descriptorFromProfile,
  loadCliProfiles,
  providerDescriptorFor,
  resetCliProfiles,
  setCliProfiles,
  type ServedCliProfile,
} from "./providerAdapter";
import { detectClaudeHandshake, detectResumeFailure } from "./resumeVerification";

const profiles = served as ServedCliProfile[];
const claudeProfile = (): ServedCliProfile => {
  const p = profiles.find((x) => x.id === "claude");
  if (!p) throw new Error("served_profiles.json has no claude profile");
  return p;
};

beforeEach(() => {
  resetCliProfiles();
  invokeMock.mockReset();
});
afterEach(() => resetCliProfiles());

describe("providerDescriptorFor", () => {
  it("resolves the served Claude profile", () => {
    setCliProfiles(profiles);
    expect(providerDescriptorFor("claude")?.provider).toBe("claude");
  });

  it("answers null — never Claude — for unknown and absent providers", () => {
    setCliProfiles(profiles);
    expect(providerDescriptorFor("gemini")).toBeNull();
    expect(providerDescriptorFor("totally-new")).toBeNull();
    expect(providerDescriptorFor(undefined)).toBeNull();
  });

  it("answers null for EVERY provider while the cache is unprimed", () => {
    expect(cliProfilesLoaded()).toBe(false);
    expect(providerDescriptorFor("claude")).toBeNull();
  });
});

describe("loadCliProfiles", () => {
  it("fetches once through terminal_cli_profiles and caches", async () => {
    invokeMock.mockResolvedValue(profiles);
    const [a, b] = await Promise.all([loadCliProfiles(), loadCliProfiles()]);
    expect(a && b).toBe(true);
    expect(await loadCliProfiles()).toBe(true);
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("terminal_cli_profiles");
    expect(providerDescriptorFor("claude")?.restoreTier()).toBe("full");
  });

  it("a failed fetch leaves the cache unprimed (Claude stays unknown) and the next call retries", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    invokeMock.mockRejectedValueOnce(new Error("ipc down"));
    expect(await loadCliProfiles()).toBe(false);
    expect(providerDescriptorFor("claude")).toBeNull();

    invokeMock.mockResolvedValueOnce(profiles);
    expect(await loadCliProfiles()).toBe(true);
    expect(providerDescriptorFor("claude")).not.toBeNull();
    warn.mockRestore();
  });

  it("a malformed response is not a profile list", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    invokeMock.mockResolvedValueOnce({ success: true });
    expect(await loadCliProfiles()).toBe(false);
    expect(cliProfilesLoaded()).toBe(false);
    warn.mockRestore();
  });
});

describe("the Claude descriptor built from its served profile", () => {
  const claude = () => descriptorFromProfile(claudeProfile());

  it("builds the deterministic --resume command from the profile template", () => {
    expect(claude().resumeCommand("sess-abc")).toEqual(["claude", "--resume", "sess-abc"]);
  });

  it("declares the Full restore tier", () => {
    expect(claude().restoreTier()).toBe("full");
  });

  it("declares every marker once — as a regex — with empty substring lists", () => {
    const hp = claude().handshakePatterns();
    expect(hp.success).toEqual([]);
    expect(hp.failure).toEqual([]);
    expect(hp.successPatterns?.length).toBeGreaterThan(0);
    expect(hp.failurePatterns?.length).toBeGreaterThan(0);
    // The dialect contract: every source compiles case-insensitively.
    for (const re of [...(hp.successPatterns ?? []), ...(hp.failurePatterns ?? [])]) {
      expect(re.flags).toBe("i");
    }
  });

  it("carries the v2 markers: the versioned logo line and the rule-prompt-rule frame", () => {
    const hp = claude().handshakePatterns();
    const sources = (hp.successPatterns ?? []).map((re) => re.source);
    expect(sources).toContain("Claude Code v\\d");
    expect(sources.some((src) => src.includes("─{3,}") && src.includes("❯"))).toBe(true);
  });

  it("carries Claude's launch window title as a title-only marker", () => {
    const hp = claude().handshakePatterns();
    expect(hp.titlePatterns?.length).toBeGreaterThan(0);
    for (const re of hp.titlePatterns ?? []) expect(re.flags).toBe("i");
    expect(hp.titlePatterns?.some((re) => re.test("\u2733 Claude Code"))).toBe(true);
  });

  it("keeps the box-frame regex — the one marker no substring can express", () => {
    const hp = claude().handshakePatterns();
    const frame = "╭────────────────────────────╮\n│ >  │\n╰────────────────────────────╯";
    expect(hp.successPatterns?.some((re) => re.source === "[╭╰]─{3,}")).toBe(true);
    expect(detectClaudeHandshake(frame, hp)).toBe(true);
    // Negative control: two dashes is not a frame.
    expect(detectClaudeHandshake("╭── not a frame", hp)).toBe(false);
  });

  // The substring lists folded into the regexes (plan
  // 2026-08-23-single-source-derived-facts item 9), frozen as a fixture: every
  // one must still verify, in any case.
  const FOLDED_SUCCESS = [
    "? for shortcuts",
    "esc to interrupt",
    "bypass permissions",
    "Welcome to Claude",
    "Welcome back to Claude",
  ];
  const FOLDED_FAILURE = [
    "No conversation found",
    "No conversations found",
    "No conversations to resume",
    "Select a session to resume",
    "Select a conversation to resume",
  ];

  it.each(FOLDED_SUCCESS)("still verifies the folded success marker %j (any case)", (marker) => {
    const hp = claude().handshakePatterns();
    expect(detectClaudeHandshake(`  ${marker}  `, hp)).toBe(true);
    expect(detectClaudeHandshake(marker.toUpperCase(), hp)).toBe(true);
  });

  it.each(FOLDED_FAILURE)("still detects the folded failure marker %j (any case)", (marker) => {
    const hp = claude().handshakePatterns();
    expect(detectResumeFailure(`Error: ${marker}`, hp)).toBe(true);
    expect(detectResumeFailure(marker.toLowerCase(), hp)).toBe(true);
  });
});

describe("descriptorFromProfile honesty", () => {
  it("a Full claim with no by-id resume reads terminal-only, and has no resume command", () => {
    const d = descriptorFromProfile({
      ...claudeProfile(),
      id: "no-resume",
      resume: { kind: "unknown" },
    });
    expect(d.restoreTier()).toBe("terminal-only");
    expect(d.resumeCommand("sess-1")).toBeNull();
  });

  it("a profile with no handshake data has empty patterns that match nothing", () => {
    const d = descriptorFromProfile({ id: "bare", displayName: "Bare", programs: ["bare"] });
    const hp = d.handshakePatterns();
    expect(detectClaudeHandshake("? for shortcuts", hp)).toBe(false);
    expect(detectResumeFailure("No conversation found", hp)).toBe(false);
    expect(d.restoreTier()).toBe("terminal-only");
  });
});
