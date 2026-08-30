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

/**
 * Manual-test-loop iteration 24, item 3.
 *
 * `pasteText` with a non-string `text` used to fail as
 * `TypeError: Er.replace is not a function` — the minified name of an internal
 * binding, handed to an automation caller as the entire diagnosis. There is no
 * way to act on that: it names nothing the caller controls, and it changes
 * every time the bundle is rebuilt.
 */
describe("preparePasteData — type guard (iter 24, item 3)", () => {
  const NON_STRINGS: Array<[string, unknown]> = [
    ["a number", 42],
    ["an object", { a: 1 }],
    ["an array", ["a", "b"]],
    ["null", null],
    ["undefined", undefined],
    ["a boolean", true],
  ];

  it.each(NON_STRINGS)("throws PASTE_TEXT_INVALID for %s", (_label, value) => {
    expect(() => preparePasteData(value as string, true)).toThrow("PASTE_TEXT_INVALID");
  });

  it("never surfaces a minified identifier", () => {
    let message = "";
    try {
      preparePasteData(42 as unknown as string, true);
    } catch (err) {
      message = (err as Error).message;
    }
    // The measured leak. `.replace is not a function` names an internal, and
    // its subject (`Er`) is whatever the minifier chose this build.
    expect(message).not.toMatch(/replace is not a function/);
    expect(message).toContain("PASTE_TEXT_INVALID");
    expect(message).toContain("must be a string");
  });

  it("carries a machine-readable .code, which is what the SDK hoists onto the response", () => {
    try {
      preparePasteData(42 as unknown as string, false);
      throw new Error("should have thrown");
    } catch (err) {
      expect((err as Error & { code?: string }).code).toBe("PASTE_TEXT_INVALID");
    }
  });

  it("does NOT echo the rejected value, which may be credential-derived", () => {
    let message = "";
    try {
      preparePasteData({ token: "sk-secret-value" } as unknown as string, true);
    } catch (err) {
      message = (err as Error).message;
    }
    expect(message).not.toContain("sk-secret-value");
    expect(message).toContain("an object");
  });

  // The formatter's own business is the TYPE. Emptiness is the handler's call,
  // and both handlers already make it — so an empty paste still formats.
  it('accepts "" as a well-defined empty paste', () => {
    expect(preparePasteData("", true)).toBe("\x1b[200~\x1b[201~");
    expect(preparePasteData("", false)).toBe("");
  });

  it('accepts the falsy-but-valid "0"', () => {
    expect(preparePasteData("0", false)).toBe("0");
  });
});
