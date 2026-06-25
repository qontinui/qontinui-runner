/**
 * Frontend descriptor mirror of the Rust `SessionProviderAdapter` contract
 * (session-restore-redesign plan §4). The provider-agnostic CORE lives in the
 * runner backend; the frontend only needs the per-provider capability surface
 * that drives the boot-restore UX: the resume command shape, the resume
 * handshake patterns `resumeVerification` matches against, and the declared
 * restore tier for the honest-capability UI.
 *
 * Phase 1 ships the TYPES + a registry lookup ({@link providerDescriptorFor})
 * only. The concrete Claude descriptor is filled in Phase 2 (port from
 * `aiLaunchCommand.ts` / `resumeVerification.ts`); Phase 3 adds Gemini. Until
 * then the registry returns a minimal default so callers compile and behave
 * sensibly.
 */

/**
 * Declared restore capability of a provider (mirrors Rust `RestoreTier`).
 * - `"full"`: the provider deterministically resumes the FULL conversation by
 *   id (`--resume <id>`) — restore brings the chat back.
 * - `"terminal-only"`: only terminal+cwd+launch-command restore; no
 *   conversation resume. The UI is honest about the loss ("fresh conversation").
 */
export type RestoreTier = "full" | "terminal-only";

/** Success/failure substring patterns (ANSI-stripped) for `resumeVerification`. */
export interface HandshakePatterns {
  /** Substrings whose presence confirms the resume landed. */
  success: string[];
  /** Substrings whose presence means the resume FAILED (drives the banner). */
  failure: string[];
}

/**
 * The capability surface the frontend needs from a provider adapter (plan §4).
 * The launch/account-isolation/hook-delivery halves are backend concerns and
 * are NOT mirrored here — the frontend only consumes resume + handshake + tier.
 */
export interface SessionProviderDescriptor {
  /** Provider id (`"claude"`, `"gemini"`). Matches the record's `provider`. */
  provider: string;
  /**
   * Build the deterministic, non-interactive resume command for `sessionId`.
   * Phase 2 fills the Claude shape (`["claude", "--resume", sessionId]`).
   */
  resumeCommand(sessionId: string): string[];
  /** Resume success/failure handshake patterns (Phase 2 ports the real sets). */
  handshakePatterns(): HandshakePatterns;
  /** Declared restore capability for the honest-UX surface. */
  restoreTier(): RestoreTier;
}

/**
 * The Phase-1 PLACEHOLDER Claude descriptor. The resume command shape is stable
 * enough to ship now; Phase 2 fills the handshake patterns from
 * `resumeVerification.ts`. No method throws.
 */
export const claudeDescriptor: SessionProviderDescriptor = {
  provider: "claude",
  resumeCommand: (sessionId: string) => ["claude", "--resume", sessionId],
  handshakePatterns: () => ({ success: [], failure: [] }),
  restoreTier: () => "full",
};

/**
 * Registry lookup (plan §4 registry seam). Phase 1 knows only the Claude
 * descriptor; Phase 3 adds the Gemini arm. An unknown provider degrades to the
 * Claude descriptor (the only shipped provider today) rather than failing — a
 * record with an unexpected provider should still restore via the default path.
 */
export function providerDescriptorFor(provider: string | undefined): SessionProviderDescriptor {
  switch (provider) {
    // Phase 3 adds: case "gemini": return geminiDescriptor;
    case "claude":
    default:
      return claudeDescriptor;
  }
}
