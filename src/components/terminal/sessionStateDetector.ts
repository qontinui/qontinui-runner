import type { SessionState } from "./useZoneLayout";

/**
 * Patterns that indicate Claude Code is waiting for user input.
 * These are checked against raw terminal output (may include ANSI sequences).
 */
const NEEDS_INPUT_PATTERNS = [
  // Claude Code tool approval prompts
  /\? \(y\/n\)/,
  /\? \(yes\/no\)/,
  /Allow .+\? \(/, // "Allow Read tool? (y/n)"
  /Do you want to proceed/i,
  /Press enter to continue/i,

  // Claude Code interactive prompts
  /\? >/, // Claude input prompt
  /waiting for your (?:input|response)/i,
  /What would you like/i,
  /Please (?:provide|enter|specify)/i,

  // Git/CLI interactive prompts that might appear in Claude sessions
  /\(Y\/n\)/,
  /\[y\/N\]/,
  /Continue\? \[/,
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
