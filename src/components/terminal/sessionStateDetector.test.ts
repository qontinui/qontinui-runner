/**
 * Unit tests for `detectSessionState` — terminal-output → SessionState
 * classification behind the StatusStrip needs-input pill.
 *
 * Regression targets (StatusStrip session-count plan):
 *  - Bug 2: bare generic shell tokens (`(Y/n)`, `[y/N]`, "Press enter to
 *    continue", "Continue? [") must NOT latch a tab to `needs-input` — they
 *    appear in ordinary non-blocking tool output (git/apt/npm). Only
 *    Claude-specific prompt framings count.
 *  - Bug 3: the `needs-input` latch must break back to `working` when the
 *    session resumes producing Claude tool output.
 */

import { describe, it, expect } from "vitest";
import { detectSessionState } from "./sessionStateDetector";

describe("detectSessionState — generic shell tokens no longer flag needs-input", () => {
  // Keep these strings short (<= 20 chars) so they don't trip the
  // "substantial output ⇒ working" fallback; the point is purely that they
  // do NOT classify as needs-input.
  const generic = ["(Y/n)", "[y/N]", "Press enter", "Continue? ["];

  for (const token of generic) {
    it(`"${token}" does not flag needs-input`, () => {
      expect(detectSessionState(token, "idle")).not.toBe("needs-input");
    });
  }
});

describe("detectSessionState — genuine Claude prompts DO flag needs-input", () => {
  const claudePrompts = [
    "Allow Read tool? (y/n)",
    "Proceed? (yes/no)",
    "Do you want to proceed?",
    "Please provide the file path",
    "waiting for your input",
  ];

  for (const prompt of claudePrompts) {
    it(`"${prompt}" flags needs-input`, () => {
      expect(detectSessionState(prompt, "idle")).toBe("needs-input");
    });
  }
});

describe("detectSessionState — bypass-aware approval suppression", () => {
  // PHANTOM EVIDENCE (2026-06-07): a `--dangerously-skip-permissions`
  // continuation latched needs-input off Bash approval-shaped scrollback during
  // a 12-min `rm -rf`, despite no permission ask ever existing. A bypass
  // session can never await tool approval — these must NOT latch.
  const approvalShaped = [
    "Allow Read tool? (y/n)",
    "Proceed? (yes/no)",
    "Do you want to proceed?",
  ];

  for (const text of approvalShaped) {
    it(`"${text}" + bypass → does NOT flag needs-input`, () => {
      expect(detectSessionState(text, "idle", { bypassPermissions: true })).not.toBe("needs-input");
    });

    it(`"${text}" + non-bypass → flags needs-input (no regression)`, () => {
      expect(detectSessionState(text, "idle", { bypassPermissions: false })).toBe("needs-input");
      // Omitting opts entirely must behave as non-bypass (default false).
      expect(detectSessionState(text, "idle")).toBe("needs-input");
    });
  }

  // Question-shaped prompts (AskUserQuestion / free-form) are REAL even under
  // bypass — bypass suppresses only tool approvals, not questions.
  const questionShaped = [
    "Please provide the file path",
    "waiting for your input",
    "What would you like me to do?",
  ];

  for (const text of questionShaped) {
    it(`"${text}" + bypass → still flags needs-input`, () => {
      expect(detectSessionState(text, "idle", { bypassPermissions: true })).toBe("needs-input");
    });
  }

  it("resumed-pattern still breaks the latch for a bypass session", () => {
    expect(
      detectSessionState("Reading src/main.rs", "needs-input", { bypassPermissions: true }),
    ).toBe("working");
  });
});

describe("detectSessionState — needs-input latch-breaker", () => {
  it("transitions needs-input → working when resumed Claude tool output arrives", () => {
    expect(detectSessionState("Reading src/main.rs", "needs-input")).toBe("working");
    expect(detectSessionState("Running command: cargo build", "needs-input")).toBe("working");
    expect(detectSessionState("esc to interrupt", "needs-input")).toBe("working");
  });

  it("does NOT break the latch on unrelated short output", () => {
    // No resumed-pattern match and not a fresh needs-input prompt → stay put
    // (returns null = no state change, so the tab remains needs-input).
    expect(detectSessionState("ok", "needs-input")).toBeNull();
  });

  it("error output does not override an active needs-input latch", () => {
    expect(detectSessionState("Error: something failed", "needs-input")).toBeNull();
  });
});
