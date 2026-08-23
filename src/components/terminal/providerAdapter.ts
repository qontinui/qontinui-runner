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

/**
 * Success/failure patterns (ANSI-stripped) for `resumeVerification`.
 *
 * TWO matcher kinds, both live, unioned by `resumeVerification`'s detectors:
 *
 * - `success` / `failure` — case-insensitive SUBSTRINGS. The portable half of
 *   the descriptor contract; a provider adapter can be declared without
 *   regex knowledge.
 * - `successPatterns` / `failurePatterns` — REGEXES, for markers a substring
 *   cannot express (the Claude TUI's rounded input-box frame, `/[╭╰]─{3,}/`,
 *   is the motivating one — it is a shape, not a phrase).
 *
 * THE DEFECT this closes: boot-restore ALWAYS passes a descriptor's
 * `HandshakePatterns`, and the detectors used to treat that as an EITHER/OR —
 * `if (patterns) return matchSubstrings(...)`. The regex sets in
 * `resumeVerification.ts` were therefore dead on the only path that runs in
 * production, so the frame marker never participated in a real verification: a
 * restored pane that had painted only the box frame read as "handshake not
 * observed" and burned the full retry budget. The regexes now travel WITH the
 * provider descriptor, so unioning them cannot leak Claude's frames into a
 * future non-Claude adapter's verification.
 */
export interface HandshakePatterns {
  /** Substrings whose presence confirms the resume landed. */
  success: string[];
  /** Substrings whose presence means the resume FAILED (drives the banner). */
  failure: string[];
  /** Regex markers unioned with {@link HandshakePatterns.success}. */
  successPatterns?: RegExp[];
  /** Regex markers unioned with {@link HandshakePatterns.failure}. */
  failurePatterns?: RegExp[];
}

/**
 * Markers that the Claude Code TUI actually rendered in this pane — i.e. the
 * resume command landed and the CLI took over the terminal. Deliberately
 * Claude-UI-shaped (rounded input box, status-line hints) rather than "any
 * output grew": a shell prompt echo or an error message must NOT count.
 *
 * The interactive resume-size picker ("Resume from summary?") is itself
 * Claude UI — a pane wedged on it still counts as HANDSHAKE OK (the resume
 * landed; answering the picker is a separate concern).
 */
export const CLAUDE_HANDSHAKE_REGEXES: RegExp[] = [
  /\? for shortcuts/, // persistent status-line hint under the input box
  /esc to interrupt/i, // shown while Claude is actively working
  /bypass permissions/i, // permission-mode indicator in the status line
  /Welcome (?:back )?to Claude/i, // launch banner
  /[╭╰]─{3,}/, // rounded input-box / dialog frame
];

/**
 * Definitive evidence that the REQUESTED session did NOT resume — evaluated
 * BEFORE the success markers on every probe. "The Claude TUI appeared" is not
 * the same claim as "the requested session resumed": a
 * `claude --resume <bogus-id>` can fall through to an error dialog, a fresh
 * session, or the interactive session picker, all of which still render
 * Claude-UI frames.
 */
export const CLAUDE_RESUME_FAILURE_REGEXES: RegExp[] = [
  /No conversation found/i, // `--resume <unknown-id>` error
  /No conversations? (?:found|to resume)/i, // empty-history variants
  /Select a (?:session|conversation) to resume/i, // interactive session picker frame
];

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
 * `claude --resume <id>`. `restoreTier` is `"full"` — Claude resumes the FULL
 * conversation by id.
 *
 * The handshake set carries BOTH halves and both are live: the plain
 * substrings, plus {@link CLAUDE_HANDSHAKE_REGEXES} /
 * {@link CLAUDE_RESUME_FAILURE_REGEXES}, which `resumeVerification` unions into
 * the same probe. Before that union the regexes were dead on the boot-restore
 * path (which always supplies this descriptor), so the box-frame marker never
 * ran in production — see {@link HandshakePatterns} for the full defect note.
 * Failure is still checked FIRST, since a failure dialog is itself Claude UI.
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
    successPatterns: CLAUDE_HANDSHAKE_REGEXES,
    failurePatterns: CLAUDE_RESUME_FAILURE_REGEXES,
  }),
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
