/**
 * The cross-language drift guard for the served CLI profiles (plan
 * `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
 * Phase 4).
 *
 * A profile's handshake regexes are ONE source compiled by two engines: Rust
 * `regex` in `src-tauri/src/cli_profile/mod.rs` and JavaScript `RegExp` in
 * `resumeVerification.ts`. Both suites classify the SAME ANSI-stripped screens
 * (`src-tauri/tests/fixtures/cli_screens/<cli>/*.txt`) against the SAME
 * expected table (`expected.json`). The Rust suite reads the live manifest and
 * pins `served_profiles.json` to it; this suite reads that snapshot, so a
 * profile change reaches it only through a regenerated snapshot the Rust side
 * has already agreed with. A pattern that matches in one engine and not the
 * other turns one of the two red.
 */

import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import served from "../../../src-tauri/tests/fixtures/cli_screens/served_profiles.json";
import expected from "../../../src-tauri/tests/fixtures/cli_screens/expected.json";
import { providerDescriptorFor, setCliProfiles, type ServedCliProfile } from "./providerAdapter";
import { detectClaudeHandshake, detectResumeFailure } from "./resumeVerification";

const FIXTURES = resolve(
  dirname(fileURLToPath(import.meta.url)),
  "../../../src-tauri/tests/fixtures/cli_screens",
);

const profiles = served as ServedCliProfile[];
setCliProfiles(profiles);

/** Mirrors `terminal/usage_limit.rs`: lowercase, collapse whitespace, substring. */
function usageLimit(profile: ServedCliProfile, screen: string): boolean {
  const normalized = screen.toLowerCase().replace(/\s+/g, " ");
  return (profile.usageLimitPhrases ?? []).some((phrase) => normalized.includes(phrase));
}

describe("shared CLI screen fixtures classify through the served profiles", () => {
  it("covers at least one screen per served profile", () => {
    for (const p of profiles) {
      expect(expected.screens.some((s) => s.provider === p.id)).toBe(true);
    }
  });

  it.each(expected.screens)("$file", ({ provider, file, resume, usageLimit: limit }) => {
    const descriptor = providerDescriptorFor(provider);
    const profile = profiles.find((p) => p.id === provider);
    expect(descriptor).not.toBeNull();
    expect(profile).toBeDefined();
    if (descriptor === null || profile === undefined) return;

    const screen = readFileSync(resolve(FIXTURES, file), "utf8");
    const hp = descriptor.handshakePatterns();
    const verdict = detectResumeFailure(screen, hp)
      ? "failed"
      : detectClaudeHandshake(screen, hp)
        ? "verified"
        : "none";
    expect(verdict).toBe(resume);
    expect(usageLimit(profile, screen)).toBe(limit);
  });
});
