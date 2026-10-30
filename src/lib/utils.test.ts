/**
 * `describeThrown` — the one catch-site describer (plan
 * `2026-09-09-catch-site-discard-is-repo-wide-and-the-deferral-was-never-measured`).
 */

import { describe, expect, it } from "vitest";

import { DESCRIBE_THROWN_DUMP_MAX, describeThrown } from "./utils";

describe("describeThrown", () => {
  it("THE REGRESSION: a plain-string rejection (what invoke() throws) keeps its text", () => {
    expect(
      describeThrown("coord unreachable: connection refused", "Failed to load overlap pairs"),
    ).toBe("coord unreachable: connection refused");
  });

  it("an Error still yields its message", () => {
    expect(describeThrown(new Error("boom"), "fallback")).toBe("boom");
  });

  it("a status + body object serializes BOTH halves", () => {
    expect(describeThrown({ status: 500, error: "list_overlapping_intents failed" }, "fb")).toBe(
      "HTTP 500: list_overlapping_intents failed",
    );
  });

  it("a status-only or body-only object still surfaces what it has", () => {
    expect(describeThrown({ status: 404 }, "fb")).toBe("HTTP 404");
    expect(describeThrown({ message: "no such command" }, "fb")).toBe("no such command");
    expect(describeThrown({ code: "E_NOENT" }, "fb")).toBe("E_NOENT");
  });

  it("an unrecognised object is DUMPED alongside the fallback rather than dropped", () => {
    expect(describeThrown({ weird: 1 }, "Failed to load overlap pairs")).toBe(
      'Failed to load overlap pairs ({"weird":1})',
    );
  });

  it("only a genuinely empty value falls back to the bare constant", () => {
    expect(describeThrown(null, "fb")).toBe("fb");
    expect(describeThrown(undefined, "fb")).toBe("fb");
    expect(describeThrown("   ", "fb")).toBe("fb");
    expect(describeThrown({}, "fb")).toBe("fb");
  });

  it("survives a circular object instead of throwing inside the error path", () => {
    const circular: Record<string, unknown> = {};
    circular.self = circular;
    expect(describeThrown(circular, "fb")).toBe("fb");
  });

  it("a thrown number/boolean keeps the fallback's context instead of a bare digit", () => {
    expect(describeThrown(0, "Failed to load X")).toBe("Failed to load X (0)");
    expect(describeThrown(false, "Failed to load X")).toBe("Failed to load X (false)");
  });

  it("caps an oversized dump instead of flooding the panel", () => {
    const out = describeThrown({ blob: "x".repeat(5000) }, "fb");
    expect(out.startsWith('fb ({"blob":"')).toBe(true);
    expect(out.endsWith("…(truncated))")).toBe(true);
    expect(out.length).toBeLessThan(DESCRIBE_THROWN_DUMP_MAX + 40);
  });

  it("an empty fallback yields the bare detail, never a leading-space fragment", () => {
    expect(describeThrown(7, "")).toBe("7");
    expect(describeThrown({ weird: 1 }, "")).toBe('{"weird":1}');
    expect(describeThrown(undefined, "")).toBe("");
  });

  it("labels only a numeric status as HTTP", () => {
    expect(describeThrown({ status: " 404 " }, "fb")).toBe("HTTP 404");
    expect(describeThrown({ status: "failed", message: "x" }, "fb")).toBe("x");
    expect(describeThrown({ status: "failed", reason: "disk full" }, "fb")).toBe(
      'fb ({"status":"failed","reason":"disk full"})',
    );
  });

  it("an Error message is trimmed, and a blank one falls back", () => {
    expect(describeThrown(new Error("  boom\n"), "fb")).toBe("boom");
    expect(describeThrown(new Error("   "), "fb")).toBe("fb");
  });

  it("an Error with an empty message falls back", () => {
    expect(describeThrown(new Error(""), "fb")).toBe("fb");
  });
});
