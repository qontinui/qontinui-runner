/**
 * Tests for the frontend `SessionProviderDescriptor` registry (session-restore
 * -redesign plan §4, Phase 2). The descriptor mirrors the Rust
 * `SessionProviderAdapter` capability surface the boot-restore UX consumes:
 * the resume command, the resume handshake patterns, and the restore tier.
 */

import { describe, it, expect } from "vitest";
import { providerDescriptorFor, claudeDescriptor, codexDescriptor } from "./providerAdapter";

describe("providerDescriptorFor", () => {
  it("resolves the Claude descriptor for 'claude'", () => {
    expect(providerDescriptorFor("claude")).toBe(claudeDescriptor);
    expect(providerDescriptorFor("claude").provider).toBe("claude");
  });

  it("resolves the Codex descriptor for 'codex'", () => {
    expect(providerDescriptorFor("codex")).toBe(codexDescriptor);
    expect(providerDescriptorFor("codex").provider).toBe("codex");
  });

  it("degrades unknown/undefined providers to the Claude descriptor (never drops)", () => {
    expect(providerDescriptorFor("gemini")).toBe(claudeDescriptor);
    expect(providerDescriptorFor("totally-new")).toBe(claudeDescriptor);
    expect(providerDescriptorFor(undefined)).toBe(claudeDescriptor);
  });
});

describe("codexDescriptor", () => {
  it("builds the interactive `codex resume <id>` command (no --session-id; read-back identity)", () => {
    expect(codexDescriptor.resumeCommand("11111111-2222-7333-8444-555555555555")).toEqual([
      "codex",
      "resume",
      "11111111-2222-7333-8444-555555555555",
    ]);
  });

  it("declares the Full restore tier (codex resumes the full conversation by id)", () => {
    expect(codexDescriptor.restoreTier()).toBe("full");
  });

  it("exposes non-empty success + failure handshake patterns (best-effort / pin-your-version)", () => {
    const hp = codexDescriptor.handshakePatterns();
    expect(hp.success.length).toBeGreaterThan(0);
    expect(hp.failure.length).toBeGreaterThan(0);
    expect(hp.failure).toContain("No previous sessions");
    expect(hp.failure).toContain("not found");
  });
});

describe("claudeDescriptor", () => {
  it("builds the deterministic --resume command", () => {
    expect(claudeDescriptor.resumeCommand("sess-abc")).toEqual([
      "claude",
      "--resume",
      "sess-abc",
    ]);
  });

  it("declares the Full restore tier", () => {
    expect(claudeDescriptor.restoreTier()).toBe("full");
  });

  it("exposes non-empty success + failure handshake patterns (ported from resumeVerification.ts)", () => {
    const hp = claudeDescriptor.handshakePatterns();
    expect(hp.success.length).toBeGreaterThan(0);
    expect(hp.failure.length).toBeGreaterThan(0);
    // Representative markers from the live resumeVerification matchers.
    expect(hp.success).toContain("? for shortcuts");
    expect(hp.failure).toContain("No conversation found");
    expect(hp.failure).toContain("Select a session to resume");
  });
});
