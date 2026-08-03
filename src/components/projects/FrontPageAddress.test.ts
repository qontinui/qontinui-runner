import { describe, expect, it } from "vitest";

import { describeAddressProblem } from "./FrontPageAddress";

describe("describeAddressProblem", () => {
  it("accepts a local dev server", () => {
    expect(describeAddressProblem("http://localhost:3000")).toBeNull();
    expect(describeAddressProblem("http://127.0.0.1:8081")).toBeNull();
  });

  it("accepts a REMOTE deployed address", () => {
    // The whole point of making this a first-class field: a project whose
    // backend is already deployed has nothing local to start, and its address
    // is an ordinary https host. `validate_preview_url` allows http AND https —
    // the scheme rule is a security boundary, not a localhost-only rule.
    expect(describeAddressProblem("https://pizzeria.example.com")).toBeNull();
    expect(
      describeAddressProblem("https://abc123.execute-api.eu-central-1.amazonaws.com"),
    ).toBeNull();
  });

  it("treats an empty value as 'no problem' — it means clear, not invalid", () => {
    expect(describeAddressProblem("")).toBeNull();
    expect(describeAddressProblem("   ")).toBeNull();
  });

  it("rejects a bare host with no scheme, and says what is missing", () => {
    const problem = describeAddressProblem("localhost:3000");
    // `new URL("localhost:3000")` parses — "localhost:" becomes the SCHEME —
    // so this lands on the scheme arm rather than the parse arm. Either way the
    // user must be told to add http://, which is the assertion that matters.
    expect(problem).not.toBeNull();
    expect(problem).toMatch(/http/);
  });

  it("rejects schemes the preview window must never open", () => {
    // `front_page_url` lands in a webview the app CSP does not govern, so
    // `file:` would turn Open into an arbitrary-file viewer and `javascript:`
    // into script execution. Mirrors `validate_preview_url`.
    expect(describeAddressProblem("file:///C:/Windows/System32/drivers/etc/hosts")).toMatch(
      /http:\/\/ and https:\/\//,
    );
    expect(describeAddressProblem("javascript:alert(1)")).toMatch(/http:\/\/ and https:\/\//);
  });

  it("does not accept gibberish", () => {
    expect(describeAddressProblem("not an address")).not.toBeNull();
  });
});
