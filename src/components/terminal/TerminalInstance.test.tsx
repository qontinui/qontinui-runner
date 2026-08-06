/**
 * Pure-logic tests for the input accumulator extracted from
 * `TerminalInstance`'s xterm.js `onData` loop.
 *
 * The runner's vitest config uses `environment: "node"` (no jsdom) — and
 * `TerminalInstance` itself is impractical to import from a test
 * because it transitively pulls `@xterm/addon-canvas`, which touches
 * `self` at module init and crashes under Node. The load-bearing logic
 * was extracted to `./consumeInputChunk.ts` (a leaf module with zero
 * imports) so it can be unit-tested in isolation. `TerminalInstance`'s
 * `onData` handler is now a thin shell over this helper.
 *
 * The cases below cover:
 *   - Multiple newlines in a single chunk emit multiple lines.
 *   - A line split across two chunks accumulates correctly.
 *   - Non-printable chars (code < 32) are dropped from the accumulator.
 *   - `firstInputLineIfAny` is gated by the caller's "already reported"
 *     flag so `onFirstInput` fires only once per session.
 *   - Bare empty newlines (whitespace-only) emit no line — preserving
 *     the historical "empty line trimmed" behavior.
 *
 * The second suite covers the stdin OWNERSHIP GATE that sits at the tail of the
 * same `onData` handler (`if (!isOwnedRef.current) return;` immediately before
 * `invoke("terminal_write", …)`). Same constraint as above — the component
 * can't be mounted here — so the gate is covered two ways: its decision
 * (`isOwnedBy`, the exact function `isOwned` delegates to) and its WIRING,
 * scanned from source. The wiring half is the load-bearing one: a
 * `<TerminalInstance>` mount site that forgets `pageId` silently reverts that
 * render path to the old stdin-dead behavior with nothing else going red.
 */

import { readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { describe, it, expect, vi } from "vitest";
import { consumeInputChunk } from "./consumeInputChunk";

// `isOwnedBy` lives in a module that imports the Tauri API + the localStorage
// binding mirror; neither is usable under the "node" test environment, and
// neither is exercised by the pure predicate.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ label: "main" }) }));
vi.mock("@/lib/pageBindings", () => ({ setPageBindings: vi.fn() }));

const { isOwnedBy } = await import("./contexts/WindowAssignmentsContext");

describe("consumeInputChunk", () => {
  it("emits two lines for two newlines in one chunk", () => {
    const result = consumeInputChunk("hello\nworld\n", "", false);
    expect(result.lines).toEqual(["hello", "world"]);
    expect(result.firstInputLineIfAny).toBe("hello");
    expect(result.accum).toBe("");
  });

  it("accumulates a line split across two chunks", () => {
    const first = consumeInputChunk("hel", "", false);
    expect(first.lines).toEqual([]);
    expect(first.firstInputLineIfAny).toBeUndefined();
    expect(first.accum).toBe("hel");

    const second = consumeInputChunk("lo\n", first.accum, false);
    expect(second.lines).toEqual(["hello"]);
    expect(second.firstInputLineIfAny).toBe("hello");
    expect(second.accum).toBe("");
  });

  it("drops non-printable chars (code < 32) from the accumulator", () => {
    // ESC (0x1b) sits between two printable chars — it must be dropped
    // so the line becomes "ab", matching the historical loop behavior at
    // the `ch.charCodeAt(0) >= 32` gate.
    const result = consumeInputChunk("a\x1bb\n", "", false);
    expect(result.lines).toEqual(["ab"]);
    expect(result.firstInputLineIfAny).toBe("ab");
    expect(result.accum).toBe("");
  });

  it("does not surface firstInputLineIfAny when already reported", () => {
    const result = consumeInputChunk("second turn\n", "", true);
    // The line still flows through `lines` (for `onUserInputLine`), but
    // the first-input slot stays empty so the caller's `onFirstInput`
    // doesn't fire again.
    expect(result.lines).toEqual(["second turn"]);
    expect(result.firstInputLineIfAny).toBeUndefined();
    expect(result.accum).toBe("");
  });

  it("emits no line for a bare empty newline", () => {
    const result = consumeInputChunk("\n", "", false);
    expect(result.lines).toEqual([]);
    expect(result.firstInputLineIfAny).toBeUndefined();
    expect(result.accum).toBe("");
  });

  it("emits no line when the accumulator is whitespace-only", () => {
    // Historical "trim then check empty" — a line of just spaces is
    // dropped from both `lines` and `firstInputLineIfAny`.
    const result = consumeInputChunk("   \n", "", false);
    expect(result.lines).toEqual([]);
    expect(result.firstInputLineIfAny).toBeUndefined();
    expect(result.accum).toBe("");
  });

  it("treats \\r the same as \\n as a line terminator", () => {
    // xterm.js fires `\r` on Enter, not `\n`, so the historical loop
    // accepts either as a line break. Verify both are honored.
    const result = consumeInputChunk("alpha\rbeta\n", "", false);
    expect(result.lines).toEqual(["alpha", "beta"]);
    expect(result.firstInputLineIfAny).toBe("alpha");
  });
});

// ── stdin ownership gate ──────────────────────────────────────────────────
// A page detached into a pop-out window ("Pop Out Page") binds that WHOLE page
// to the window. Its tabs mostly carry no `session_owner` entry — after a
// restart none do, because `clear_session_owners()` empties the map — so an
// owner-map-only gate never opened and typing did nothing (pasting still
// worked: those paths are ungated, which is the diagnostic signature).

const SRC_ROOT = resolve(__dirname, "..", "..");

/** Every prop block of a `<TerminalInstance …>` mount site under `src/`. */
function terminalInstanceMountSites(): { file: string; props: string }[] {
  const files = readdirSync(SRC_ROOT, { recursive: true, encoding: "utf8" }).filter(
    (f) => f.endsWith(".tsx") && !f.endsWith(".test.tsx"),
  );
  const sites: { file: string; props: string }[] = [];
  for (const rel of files) {
    const source = readFileSync(resolve(SRC_ROOT, rel), "utf8");
    // `<TerminalInstance` followed by whitespace = a JSX mount (excludes
    // `<TerminalInstanceHandle` in type positions).
    for (const match of source.matchAll(/<TerminalInstance\s[\s\S]*?\/>/g)) {
      sites.push({ file: rel.replace(/\\/g, "/"), props: match[0] });
    }
  }
  return sites;
}

describe("stdin ownership gate", () => {
  const assignments = {}; // no `session_owner` entry for anything — the bug case
  const pageBindings = { "page-popped": "term-1" };

  it("opens the gate when the tab's page is bound to this window and no session_owner entry exists", () => {
    // What `isOwnedRef.current = isOwned(terminalId, pageId)` resolves to in
    // the pop-out window that owns `page-popped`. `true` is what lets `onData`
    // reach `invoke("terminal_write", …)` instead of returning early.
    expect(isOwnedBy("term-1", assignments, pageBindings, "sess-fresh", "page-popped")).toBe(true);
  });

  it("keeps the gate shut when the tab's page is bound to another window", () => {
    expect(isOwnedBy("main", assignments, pageBindings, "sess-fresh", "page-popped")).toBe(false);
  });

  it("reverts to the stdin-dead answer when pageId is not threaded through", () => {
    // Why the wiring test below exists: drop `pageId` and the pop-out that
    // renders the tab stops owning it — exactly the reported bug.
    expect(isOwnedBy("term-1", assignments, pageBindings, "sess-fresh")).toBe(false);
  });

  it("wires the page-aware predicate into TerminalInstance's gate", () => {
    const source = readFileSync(resolve(__dirname, "./TerminalInstance.tsx"), "utf8");
    expect(source).toContain("isOwnedRef.current = isOwned(terminalId, pageId);");
    // Both gated forwarding paths still consult the ref.
    expect(source.match(/if \(!isOwnedRef\.current\) return;/g)?.length).toBe(2);
  });

  it("passes pageId at every <TerminalInstance> mount site", () => {
    const sites = terminalInstanceMountSites();
    // ZoneGrid's single-view + zoned mounts, plus the hidden mount.
    expect(sites.length).toBeGreaterThanOrEqual(3);
    const missing = sites.filter((s) => !/\bpageId=/.test(s.props)).map((s) => s.file);
    expect(missing).toEqual([]);
  });

  it("forwards pageId from every HiddenTerminal mount site", () => {
    const source = readFileSync(resolve(__dirname, "./ZoneGrid.tsx"), "utf8");
    for (const match of source.matchAll(/<HiddenTerminal\s[\s\S]*?\/>/g)) {
      expect(match[0]).toMatch(/\bpageId=/);
    }
  });
});
