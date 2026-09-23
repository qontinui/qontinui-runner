/**
 * Tests for the frontend `SessionProviderDescriptor` registry (session-restore
 * -redesign plan §4, Phase 2). The descriptor mirrors the Rust
 * `SessionProviderAdapter` capability surface the boot-restore UX consumes:
 * the resume command, the resume handshake patterns, and the restore tier.
 */

import { describe, it, expect } from "vitest";
import {
  providerDescriptorFor,
  claudeDescriptor,
  CLAUDE_HANDSHAKE_REGEXES,
  CLAUDE_RESUME_FAILURE_REGEXES,
} from "./providerAdapter";
import { detectClaudeHandshake, detectResumeFailure } from "./resumeVerification";

describe("providerDescriptorFor", () => {
  it("resolves the Claude descriptor for 'claude'", () => {
    expect(providerDescriptorFor("claude")).toBe(claudeDescriptor);
    expect(providerDescriptorFor("claude").provider).toBe("claude");
  });

  it("degrades unknown/undefined providers to the Claude descriptor (never drops)", () => {
    expect(providerDescriptorFor("gemini")).toBe(claudeDescriptor);
    expect(providerDescriptorFor("totally-new")).toBe(claudeDescriptor);
    expect(providerDescriptorFor(undefined)).toBe(claudeDescriptor);
  });
});

describe("claudeDescriptor", () => {
  it("builds the deterministic --resume command", () => {
    expect(claudeDescriptor.resumeCommand("sess-abc")).toEqual(["claude", "--resume", "sess-abc"]);
  });

  it("declares the Full restore tier", () => {
    expect(claudeDescriptor.restoreTier()).toBe("full");
  });

  it("declares every marker ONCE — as a regex — with empty substring lists", () => {
    const hp = claudeDescriptor.handshakePatterns();
    // Single source (plan 2026-08-23-single-source-derived-facts item 9): the
    // substring lists are empty, the regex sets are the whole declaration.
    expect(hp.success).toEqual([]);
    expect(hp.failure).toEqual([]);
    expect(hp.successPatterns).toBe(CLAUDE_HANDSHAKE_REGEXES);
    expect(hp.failurePatterns).toBe(CLAUDE_RESUME_FAILURE_REGEXES);
    expect(CLAUDE_HANDSHAKE_REGEXES.length).toBeGreaterThan(0);
    expect(CLAUDE_RESUME_FAILURE_REGEXES.length).toBeGreaterThan(0);
  });

  it("keeps the box-frame regex — the one marker no substring can express", () => {
    const frame = "╭────────────────────────────╮\n│ >  │\n╰────────────────────────────╯";
    expect(CLAUDE_HANDSHAKE_REGEXES.some((re) => re.source === "[╭╰]─{3,}")).toBe(true);
    expect(detectClaudeHandshake(frame, claudeDescriptor.handshakePatterns())).toBe(true);
    // Negative control: two dashes is not a frame.
    expect(detectClaudeHandshake("╭── not a frame", claudeDescriptor.handshakePatterns())).toBe(
      false,
    );
  });

  // The substring lists this fold deleted, frozen here as a fixture: every one
  // must still verify through the descriptor, case-insensitively as the
  // substring matcher did — the regexes are a superset, not a replacement.
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
    const hp = claudeDescriptor.handshakePatterns();
    expect(detectClaudeHandshake(`  ${marker}  `, hp)).toBe(true);
    expect(detectClaudeHandshake(marker.toUpperCase(), hp)).toBe(true);
  });

  it.each(FOLDED_FAILURE)("still detects the folded failure marker %j (any case)", (marker) => {
    const hp = claudeDescriptor.handshakePatterns();
    expect(detectResumeFailure(`Error: ${marker}`, hp)).toBe(true);
    expect(detectResumeFailure(marker.toLowerCase(), hp)).toBe(true);
  });
});
