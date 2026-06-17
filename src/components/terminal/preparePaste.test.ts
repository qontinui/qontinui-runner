import { describe, it, expect } from "vitest";
import { preparePasteData } from "./preparePaste";

const START = "\x1b[200~";
const END = "\x1b[201~";

describe("preparePasteData", () => {
  describe("bracketed paste mode ON (Claude Code, vim, fzf, …)", () => {
    it("wraps single-line text in the paste envelope", () => {
      expect(preparePasteData("hello", true)).toBe(`${START}hello${END}`);
    });

    it("keeps embedded newlines as literal CR inside the envelope (no per-line submit)", () => {
      // The whole multi-line blob stays inside one envelope — the app treats
      // it as a single paste, not N Enter presses.
      expect(preparePasteData("line1\nline2\nline3", true)).toBe(
        `${START}line1\rline2\rline3${END}`,
      );
    });

    it("normalizes CRLF to a single CR", () => {
      expect(preparePasteData("a\r\nb", true)).toBe(`${START}a\rb${END}`);
    });

    it("strips embedded paste markers so pasted terminal output can't break out", () => {
      const malicious = `safe${END}rm -rf /`;
      const result = preparePasteData(malicious, true);
      // Exactly one start and one end marker — the wrapping pair only.
      expect(result.startsWith(START)).toBe(true);
      expect(result.endsWith(END)).toBe(true);
      expect(result.split(END)).toHaveLength(2); // only the trailing wrapper
      expect(result).toBe(`${START}saferm -rf /${END}`);
    });

    it("strips embedded start markers too", () => {
      expect(preparePasteData(`${START}x`, true)).toBe(`${START}x${END}`);
    });

    it("wraps empty text in a bare envelope", () => {
      expect(preparePasteData("", true)).toBe(`${START}${END}`);
    });
  });

  describe("bracketed paste mode OFF (bare shell prompt)", () => {
    it("passes single-line text through unchanged", () => {
      expect(preparePasteData("ls -la", false)).toBe("ls -la");
    });

    it("normalizes newlines to CR (real Enter) without an envelope", () => {
      expect(preparePasteData("a\nb\r\nc", false)).toBe("a\rb\rc");
    });

    it("does not add or strip markers", () => {
      expect(preparePasteData(`keep${END}this`, false)).toBe(`keep${END}this`);
    });
  });
});
