import { describe, expect, it } from "vitest";
import { formatHeaderLines, parseHeaderLines, parseToolList } from "./ModelRoutingSection";

describe("model routing settings parsers", () => {
  it("parses one header per line and ignores blanks and malformed lines", () => {
    expect(parseHeaderLines("X-Tenant: t1\n\n  X-Project :p1 \nno-colon\n: empty")).toEqual({
      "X-Tenant": "t1",
      "X-Project": "p1",
    });
  });

  it("round-trips headers through the textarea format", () => {
    const headers = { "X-Tenant": "t1", "X-Route": "a:b" };
    expect(parseHeaderLines(formatHeaderLines(headers))).toEqual(headers);
    expect(formatHeaderLines(undefined)).toBe("");
  });

  it("splits tool lists on newlines and commas, dropping blanks", () => {
    expect(parseToolList("Read\n Bash(git status) ,Edit\n\n")).toEqual([
      "Read",
      "Bash(git status)",
      "Edit",
    ]);
  });
});
