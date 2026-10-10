import { describe, expect, it } from "vitest";
import {
  buildGatewayPayload,
  formatHeaderLines,
  needsClearConfirmation,
  parseHeaderLines,
  parseToolList,
  prefillFromMarker,
} from "./ModelRoutingSection";

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

describe("gateway form payload", () => {
  // Review N9: a TTL set in settings.json survives a save from the form.
  it("preserves api_key_helper_ttl_secs from the loaded settings", () => {
    const payload = buildGatewayPayload(
      { baseUrl: "https://gw.example.com", headers: {}, helper: "h", networkAuth: false },
      { base_url: "https://gw.example.com", api_key_helper_ttl_secs: 900 },
    );
    expect(payload.api_key_helper_ttl_secs).toBe(900);
    expect(payload.api_key_helper).toBe("h");
  });

  // Review N8: clearing the URL while the state is unknown needs confirmation.
  it("asks for confirmation before clearing an unknown-state gateway", () => {
    expect(needsClearConfirmation("unknown", "")).toBe(true);
    expect(needsClearConfirmation("declared", "")).toBe(true);
    expect(needsClearConfirmation("unknown", "https://gw.example.com")).toBe(false);
    expect(needsClearConfirmation("not_declared", "")).toBe(false);
  });

  // Review N8: in the unknown state the form is prefilled from the marker.
  it("prefills the base URL from the marker when settings carry none", () => {
    expect(prefillFromMarker("unknown", "", { base_url: "https://m.example.com" })).toBe(
      "https://m.example.com",
    );
    expect(prefillFromMarker("declared", "https://a", { base_url: "https://m" })).toBe("https://a");
    expect(prefillFromMarker("unknown", "", null)).toBe("");
  });
});
