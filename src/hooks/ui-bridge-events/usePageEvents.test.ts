/**
 * usePageEvents tests — `page_evaluate` blocklist gating.
 *
 * P5: `page_refresh` is deliberately disabled (a reload remounts the
 * terminal tree and duplicates sessions), so the evaluate blocklist must
 * reject `location.reload` too — otherwise any agent can do via
 * `page_evaluate` exactly what `page_refresh` refuses. Location READS
 * (pathname/host/href/...) must keep passing: test probes rely on them.
 */

import { describe, it, expect } from "vitest";

import { checkEvaluateBlocklist } from "./usePageEvents";

describe("checkEvaluateBlocklist", () => {
  describe("location.reload is blocked", () => {
    it.each([
      "location.reload()",
      "window.location.reload()",
      "document.location.reload()",
      "window.location.reload(true)",
      "location.reload ()",
    ])("rejects %s", (expr) => {
      expect(checkEvaluateBlocklist(expr, false)).not.toBeNull();
      // Also rejected when network requests are allowed (structural set).
      expect(checkEvaluateBlocklist(expr, true)).not.toBeNull();
    });
  });

  describe("history.go reload synonyms are blocked, navigation is not", () => {
    it.each(["history.go(0)", "history.go()", "history.go( 0 )"])(
      "rejects %s",
      (expr) => {
        expect(checkEvaluateBlocklist(expr, false)).not.toBeNull();
      },
    );
    it.each(["history.go(-1)", "history.go(1)", "history.back()"])(
      "allows %s",
      (expr) => {
        expect(checkEvaluateBlocklist(expr, false)).toBeNull();
      },
    );
  });

  describe("location reads still pass", () => {
    it.each([
      "location.pathname",
      "location.host",
      "location.href",
      "window.location.origin",
      "window.location.search",
      "document.location.hash",
    ])("allows %s", (expr) => {
      expect(checkEvaluateBlocklist(expr, false)).toBeNull();
    });
  });

  describe("existing structural blocks are unchanged", () => {
    it.each([
      "location.assign('/x')",
      "location.replace('/x')",
      "window.location.assign('/x')",
      "window.location = 'https://evil.example'",
      "eval('x')",
      "document.cookie",
    ])("rejects %s", (expr) => {
      expect(checkEvaluateBlocklist(expr, false)).not.toBeNull();
    });
  });

  describe("network patterns honor allowNetworkRequests", () => {
    it("blocks fetch by default", () => {
      expect(checkEvaluateBlocklist("fetch('/api')", false)).not.toBeNull();
    });

    it("allows fetch when explicitly opted in", () => {
      expect(checkEvaluateBlocklist("fetch('/api')", true)).toBeNull();
    });
  });
});
