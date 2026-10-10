import { describe, expect, it } from "vitest";
import { formatHeaderLines, parseHeaderLines, parseToolList } from "./ModelRoutingSection";

describe("model routing settings parsers", () => {
  it("parses one header per line and ignores blank lines", () => {
    expect(parseHeaderLines("X-Tenant: t1\n\n  X-Project :p1 ").headers).toEqual({
      "X-Tenant": "t1",
      "X-Project": "p1",
    });
  });

  // Review L8: a malformed line is surfaced, never silently dropped.
  it("surfaces malformed header lines instead of dropping them", () => {
    const parsed = parseHeaderLines("X-Tenant: t1\nno-colon\n: empty-name");
    expect(parsed.headers).toEqual({ "X-Tenant": "t1" });
    expect(parsed.malformed).toEqual(["no-colon", ": empty-name"]);
  });

  it("round-trips headers through the textarea format", () => {
    const headers = { "X-Tenant": "t1", "X-Route": "a:b" };
    const parsed = parseHeaderLines(formatHeaderLines(headers));
    expect(parsed.headers).toEqual(headers);
    expect(parsed.malformed).toEqual([]);
    expect(formatHeaderLines(undefined)).toBe("");
  });

  it("splits tool lists on newlines and commas, dropping blanks", () => {
    expect(parseToolList("Read\n Bash(git status) ,Edit\n\n")).toEqual([
      "Read",
      "Bash(git status)",
      "Edit",
    ]);
  });

  // Review L8: a comma inside a tool specifier's parentheses is part of it.
  it("does not split inside parentheses", () => {
    expect(parseToolList("Bash(git log, status), Read")).toEqual(["Bash(git log, status)", "Read"]);
  });
});
