/**
 * Tests for {@link script-worker.ts}.
 *
 * Covers the four core contracts:
 *   (a) Trivial expressions return the expected JSON value.
 *   (b) Hard-timeout expressions reject within ~2.5s.
 *   (c) Access to sandbox escapes (`require`, `process`, `eval`,
 *       `Function`, `globalThis`) throws.
 *   (d) Returning a non-serializable value (cycle, function) throws.
 */

import { describe, it, expect } from "vitest";
import { runScript, ScriptWorkerError } from "./script-worker";

describe("script-worker.runScript", () => {
  describe("trivial expressions", () => {
    it("returns a string slice", async () => {
      const result = await runScript("output.slice(0, 5)", "hello world");
      expect(result).toBe("hello");
    });

    it("filters an array", async () => {
      const input = { lines: ["ok", "ERROR: boom", "ok", "ERROR: again"] };
      const result = await runScript("output.lines.filter(l => l.includes('ERROR'))", input);
      expect(result).toEqual(["ERROR: boom", "ERROR: again"]);
    });

    it("handles numeric computations", async () => {
      const result = await runScript("output.reduce((a, b) => a + b, 0)", [1, 2, 3, 4]);
      expect(result).toBe(10);
    });

    it("returns plain objects JSON-cloned", async () => {
      const result = await runScript("({ ok: true, n: output.length })", "abc");
      expect(result).toEqual({ ok: true, n: 3 });
    });
  });

  describe("timeout enforcement", () => {
    it("rejects within 2.5s on an infinite loop", async () => {
      const start = Date.now();
      await expect(runScript("(function(){ while(true){} })()", null, 500)).rejects.toBeInstanceOf(
        ScriptWorkerError,
      );
      const elapsed = Date.now() - start;
      expect(elapsed).toBeLessThan(2500);
    }, 5000);
  });

  describe("sandbox escape attempts", () => {
    it("throws on require access", async () => {
      await expect(runScript("require('fs')", null)).rejects.toBeInstanceOf(ScriptWorkerError);
    });

    it("throws on process access", async () => {
      await expect(runScript("process.env.PATH", null)).rejects.toBeInstanceOf(ScriptWorkerError);
    });

    it("throws when invoking eval (codeGeneration.strings disabled)", async () => {
      await expect(runScript("eval('1 + 1')", null)).rejects.toBeInstanceOf(ScriptWorkerError);
    });

    it("throws when constructing Function (codeGeneration.strings disabled)", async () => {
      await expect(runScript("new Function('return 1')()", null)).rejects.toBeInstanceOf(
        ScriptWorkerError,
      );
    });

    it("throws on globalThis.process access", async () => {
      // globalThis inside the VM points at the sandbox; no `process` member.
      await expect(runScript("globalThis.process.exit(0)", null)).rejects.toBeInstanceOf(
        ScriptWorkerError,
      );
    });
  });

  describe("non-serializable results", () => {
    it("throws when returning a cyclic object", async () => {
      await expect(
        runScript("(function(){ const a = {}; a.self = a; return a; })()", null),
      ).rejects.toBeInstanceOf(ScriptWorkerError);
    });

    it("throws when returning a function", async () => {
      await expect(runScript("(() => 1)", null)).rejects.toBeInstanceOf(ScriptWorkerError);
    });

    it("throws when returning undefined", async () => {
      await expect(runScript("undefined", null)).rejects.toBeInstanceOf(ScriptWorkerError);
    });
  });

  describe("input validation", () => {
    it("throws on empty expression", async () => {
      await expect(runScript("", null)).rejects.toBeInstanceOf(ScriptWorkerError);
    });

    it("throws on whitespace expression", async () => {
      await expect(runScript("   ", null)).rejects.toBeInstanceOf(ScriptWorkerError);
    });
  });
});
