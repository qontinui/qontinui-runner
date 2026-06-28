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
 * The Claude descriptor (Phase 2). Resume is the deterministic, non-interactive
 * `claude --resume <id>`; the handshake patterns mirror `resumeVerification.ts`
 * (the `CLAUDE_HANDSHAKE_PATTERNS` success set + the `RESUME_FAILURE_PATTERNS`
 * "did NOT resume" set — failure checked first, since a failure dialog is itself
 * Claude UI). `restoreTier` is `"full"` — Claude resumes the FULL conversation
 * by id. The patterns are plain substrings (the descriptor contract is
 * substring-based); the regex-based matchers in `resumeVerification.ts` remain
 * the live boot-restore implementation this descriptor describes.
 */
export const claudeDescriptor: SessionProviderDescriptor = {
  provider: "claude",
  resumeCommand: (sessionId: string) => ["claude", "--resume", sessionId],
  handshakePatterns: () => ({
    success: [
      "? for shortcuts", // status-line hint under the input box
      "esc to interrupt", // shown while Claude is working
      "bypass permissions", // permission-mode indicator
      "Welcome to Claude", // launch banner
      "Welcome back to Claude", // resumed-banner variant
    ],
    failure: [
      "No conversation found", // `--resume <unknown-id>` error
      "No conversations found", // empty-history variant
      "No conversations to resume",
      "Select a session to resume", // interactive session-picker frame
      "Select a conversation to resume",
    ],
  }),
  restoreTier: () => "full",
};

/**
 * The Codex descriptor (Phase 3 — provider #2, OpenAI Codex CLI). Resume is the
 * interactive typed-into-PTV form `codex resume <id>` (codex accepts the UUID
 * directly; the `claude --resume` analogue). The handshake patterns mirror the
 * Rust `CodexAdapter::resume_handshake_patterns` — best-effort Codex-TUI success
 * markers + definitive failure markers ("No previous sessions" / "not found"),
 * failure checked first since a failure dialog is itself Codex UI. `restoreTier`
 * is `"full"` — codex resumes the FULL conversation by id.
 *
 * BEST-EFFORT / PIN-YOUR-VERSION: these substrings are derived from codex-cli
 * 0.142.2 and may need refinement against a live AUTHENTICATED codex. The
 * identity model differs from Claude (read-back, not `--session-id` pinning), but
 * that is a backend concern; the frontend descriptor only mirrors the resume +
 * handshake + tier surface, which IS substring-symmetric with Claude.
 */
export const codexDescriptor: SessionProviderDescriptor = {
  provider: "codex",
  resumeCommand: (sessionId: string) => ["codex", "resume", sessionId],
  handshakePatterns: () => ({
    success: [
      "Codex", // TUI banner / header
      "send a message", // input-box hint
      "/help", // status-line shortcut hint
    ],
    failure: [
      "No previous sessions", // resume with no history
      "not found", // unknown id
      "No such session",
      "error: ", // generic codex CLI error prefix
    ],
  }),
  restoreTier: () => "full",
};

/**
 * Registry lookup (plan §4 registry seam). Phase 1 knew only the Claude
 * descriptor; Phase 2 filled it in; Phase 3 adds the Codex arm. An unknown
 * provider degrades to the Claude descriptor (the reference provider) rather than
 * failing — a record with an unexpected provider should still restore via the
 * default path.
 */
export function providerDescriptorFor(provider: string | undefined): SessionProviderDescriptor {
  switch (provider) {
    case "codex":
      return codexDescriptor;
    case "claude":
    default:
      return claudeDescriptor;
  }
}
