import type { SessionState } from "./useZoneLayout";

/**
 * Patterns that indicate Claude Code is waiting for user input.
 * These are checked against raw terminal output (may include ANSI sequences).
 */
const NEEDS_INPUT_PATTERNS = [
  // Claude Code tool approval prompts. These are Claude-specific framings
  // ("Allow X? (y/n)", "Do you want to proceed?") rather than the bare
  // `(Y/n)` / `[y/N]` tokens that ordinary shell tooling (git, apt, package
  // managers, npm) prints in non-blocking informational output — matching
  // those latched plain *and* Claude sessions to `needs-input` when nothing
  // was actually awaiting input (the "N need input" phantom). See the
  // StatusStrip session-count plan, Bug 2.
  /\? \(y\/n\)/i,
  /\? \(yes\/no\)/i,
  /Allow .+\? \(/, // "Allow Read tool? (y/n)"
  /Do you want to proceed/i,

  // Claude Code interactive prompts
  /\? >/, // Claude input prompt
  /waiting for your (?:input|response)/i,
  /What would you like/i,
  /Please (?:provide|enter|specify)/i,
];

/**
 * Patterns that indicate the session is actively producing output again —
 * i.e. the prompt that triggered `needs-input` has been answered / superseded
 * and the session is no longer awaiting the user. Used to break the sticky
 * `needs-input` latch from `handleOutput` (StatusStrip plan, Bug 3): the
 * working/error/completed branches below intentionally refuse to override
 * `needs-input`, so without an explicit "resumed" signal a tab stays
 * `needs-input` forever once latched.
 */
const RESUMED_PATTERNS = [
  /(?:Reading|Writing|Editing|Creating) /,
  /Running (?:command|bash|test)/i,
  /Searching /,
  /Analyzing /,
  /^[│├└─]/m, // Box-drawing chars from Claude's tool output
  /esc to interrupt/i, // Claude Code shows this only while actively working
];

/**
 * Patterns that indicate Claude Code has completed a task/session.
 * These are strong signals that work is done.
 */
const COMPLETED_PATTERNS = [
  // Claude Code session cost summary (appears at end of session)
  /Total (?:cost|tokens):/i,
  /Session (?:cost|summary):/i,
  /\$\d+\.\d+ total cost/i,

  // Claude Code exit patterns
  /Goodbye!/,
  /Session ended/i,
];

/**
 * Patterns that indicate active work is happening.
 * Used to distinguish "working" from background noise.
 */
const WORKING_PATTERNS = [
  // Claude Code tool execution indicators
  /(?:Reading|Writing|Editing|Creating) /,
  /Running (?:command|bash|test)/i,
  /Searching /,
  /Analyzing /,

  // Streaming indicators (Claude producing output)
  /^[│├└─]/m, // Box-drawing chars from Claude's tool output
];

/**
 * Patterns that indicate an error state.
 */
const ERROR_PATTERNS = [
  /Error: /,
  /FATAL/,
  /panic:/,
  /Traceback \(most recent call last\)/,
  /Unhandled exception/i,
];

/**
 * Analyze terminal output text and determine what session state it indicates.
 * Returns the detected state, or null if no state change is warranted.
 */
export function detectSessionState(text: string, currentState: SessionState): SessionState | null {
  // Strip ANSI for pattern matching but check against raw text too
  // eslint-disable-next-line no-control-regex
  const stripped = text.replace(/\x1b\[[0-9;]*[a-zA-Z]/g, "");

  // Needs-input has highest priority — check both raw and stripped
  if (NEEDS_INPUT_PATTERNS.some((p) => p.test(text) || p.test(stripped))) {
    return "needs-input";
  }

  // Latch-breaker (Bug 3): when already `needs-input`, a clear "session is
  // working again" signal (Claude resumed tool output) means the prompt was
  // answered — transition back to `working` instead of staying stuck. Without
  // this the working/error/completed branches below all return `null` for a
  // `needs-input` tab, so the latch never decays on its own.
  if (currentState === "needs-input" && RESUMED_PATTERNS.some((p) => p.test(stripped))) {
    return "working";
  }

  // Error patterns
  if (ERROR_PATTERNS.some((p) => p.test(stripped))) {
    // Don't override needs-input with error (the error might be in context)
    if (currentState === "needs-input") return null;
    return "error";
  }

  // Completed patterns
  if (COMPLETED_PATTERNS.some((p) => p.test(stripped))) {
    return "completed";
  }

  // Working patterns or substantial output
  if (WORKING_PATTERNS.some((p) => p.test(stripped)) || stripped.length > 20) {
    // Don't override needs-input with working
    if (currentState === "needs-input") return null;
    return "working";
  }

  return null;
}
