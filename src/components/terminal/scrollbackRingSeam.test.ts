/**
 * The scrollback-ring seam (plan `2026-08-31-remote-session-tabs-in-runner-terminal`,
 * vet 2026-09-02 "The seam"): `ITerminalBackend.readScrollbackRing` is how a
 * consumer that holds a backend reads the PTY-side ring, and
 * `backends/localScrollbackRing.ts` is the ONLY module that names the local
 * Tauri command. Before this seam the command was hard-wired at four sites,
 * which is what made the abstraction one every consumer bypassed.
 *
 * `TerminalInstance` cannot be mounted under the runner's `environment: "node"`
 * vitest config (it transitively imports `@xterm/addon-canvas`, which touches
 * `self` at module init — see `TerminalInstance.test.tsx`), so the consumer
 * half is pinned the way that file pins its own wiring: by scanning source.
 * The interface half is pinned by a scripted in-memory backend that the type
 * checker must accept as an `ITerminalBackend`, and whose `readScrollbackRing`
 * a consumer-shaped replay routine consumes with `scrollbackReplay`'s own
 * offset math — no `invoke` anywhere in the path.
 */

import { readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { describe, it, expect } from "vitest";
import type { ITerminalBackend, ScrollbackRingWindow } from "./backends/types";
import { resyncSliceStart, lostWindowBytes } from "./scrollbackReplay";

const TERMINAL_ROOT = resolve(__dirname);
const LOCAL_READER = "backends/localScrollbackRing.ts";

/** Every non-test source file under `src/components/terminal/`, path relative to it. */
function terminalSourceFiles(): string[] {
  return readdirSync(TERMINAL_ROOT, { recursive: true, encoding: "utf8" })
    .map((f) => f.replace(/\\/g, "/"))
    .filter((f) => /\.tsx?$/.test(f) && !/\.test\.tsx?$/.test(f) && !f.endsWith(".d.ts"));
}

function read(rel: string): string {
  return readFileSync(resolve(TERMINAL_ROOT, rel), "utf8");
}

describe("scrollback ring seam — single command site", () => {
  it("names the local command in exactly one module: the local ring reader", () => {
    // Double-quoted literal only: prose in doc comments spells it in backticks.
    const sites = terminalSourceFiles().filter((f) =>
      read(f).includes('"terminal_get_scrollback"'),
    );
    expect(sites).toEqual([LOCAL_READER]);
  });

  it("never passes the command to invoke() anywhere but the local ring reader", () => {
    const offenders = terminalSourceFiles().filter(
      (f) => f !== LOCAL_READER && /invoke[^;]*terminal_get_scrollback/s.test(read(f)),
    );
    expect(offenders).toEqual([]);
  });
});

describe("scrollback ring seam — consumers", () => {
  it("TerminalInstance reads the ring through the backend at both sites (replay + resync)", () => {
    const source = read("TerminalInstance.tsx");
    const calls = source.match(/\.readScrollbackRing\(terminalId\)/g) ?? [];
    expect(calls.length).toBe(2);
    expect(source).not.toMatch(/invoke[^;]*terminal_get_scrollback/s);
    // The pure offset math is still what consumes the window.
    expect(source).toMatch(/resyncSliceStart\(ringWindow, writtenThrough\)/);
    expect(source).toMatch(/lostWindowBytes\(ringWindow, writtenThrough\)/);
  });

  it("both shipped backends implement readScrollbackRing by delegating to the local reader", () => {
    for (const backend of ["backends/XtermBackend.ts", "backends/GhosttyBackend.ts"]) {
      const source = read(backend);
      expect(source, backend).toMatch(
        /readScrollbackRing\(terminalId: string\): Promise<ScrollbackRingWindow \| null> \{[\s\S]*?return readLocalScrollbackRing\(terminalId\);/,
      );
    }
  });

  it("the two backend-less consumers read the local ring module, not the command", () => {
    // Mount-independent by design: no ITerminalBackend exists to route through.
    for (const consumer of ["TerminalBridgeProxies.tsx", "resumeVerification.ts"]) {
      const source = read(consumer);
      expect(source, consumer).toMatch(/readLocalScrollbackRing\(/);
      expect(source, consumer).not.toContain('"terminal_get_scrollback"');
    }
  });
});

// ── Interface half: a scripted in-memory backend ────────────────────────────
// Structurally typed as `ITerminalBackend`, so `tsc --noEmit` refuses this file
// the day the method leaves the interface or changes shape. Everything but the
// ring is inert.

function scriptedBackend(rings: (ScrollbackRingWindow | null)[]) {
  const written: Uint8Array[] = [];
  const requests: string[] = [];
  const noop = { dispose() {} };
  const backend: ITerminalBackend = {
    open() {},
    dispose() {},
    reset() {},
    focus() {},
    write(data) {
      written.push(typeof data === "string" ? new TextEncoder().encode(data) : data);
    },
    onData: () => noop,
    onBinary: () => noop,
    bracketedPasteMode: false,
    getSelection: () => "",
    hasSelection: () => false,
    clearSelection() {},
    onSelectionChange: () => noop,
    getBufferLine: () => null,
    getBufferLength: () => 0,
    setScrollback() {},
    setFontSize() {},
    setTheme() {},
    async readScrollbackRing(terminalId) {
      requests.push(terminalId);
      return rings.shift() ?? null;
    },
    fit() {},
    cols: 80,
    rows: 24,
    scrollToBottom() {},
    scrollLines() {},
    scrollPages() {},
    scrollToTop() {},
    scrollToPreviousCommand() {},
    scrollToNextCommand() {},
    findNext: () => false,
    findPrevious: () => false,
    clearSearch() {},
    onSearchResults: () => noop,
    attachCustomKeyEventHandler() {},
    registerLinkProvider: () => noop,
    onOsc633: () => noop,
    getInputElement: () => null,
    getViewportElement: () => null,
  };
  return { backend, written, requests };
}

function bytes(s: string): Uint8Array {
  return new TextEncoder().encode(s);
}

/**
 * The consumer shape `TerminalInstance.resyncFromRing` has, reduced to the
 * seam: fetch through the backend, then let `scrollbackReplay`'s pure
 * functions decide what to write.
 */
async function resyncOnce(backend: ITerminalBackend, terminalId: string, writtenThrough: number) {
  const ring = await backend.readScrollbackRing(terminalId);
  if (!ring) return { wrote: 0, lost: 0, writtenThrough };
  const window = { startOffset: ring.startOffset, endOffset: ring.endOffset };
  const lost = lostWindowBytes(window, writtenThrough);
  const slice = ring.bytes.subarray(resyncSliceStart(window, writtenThrough));
  if (slice.length > 0) backend.write(slice);
  return { wrote: slice.length, lost, writtenThrough: Math.max(writtenThrough, ring.endOffset) };
}

describe("scrollback ring seam — a scripted backend feeds scrollbackReplay's offset math", () => {
  it("replays only the bytes past what was already written", async () => {
    const payload = "0123456789";
    const { backend, written, requests } = scriptedBackend([
      { bytes: bytes(payload), startOffset: 100, endOffset: 110 },
    ]);
    const out = await resyncOnce(backend, "pane-1", 104);
    expect(requests).toEqual(["pane-1"]);
    expect(out.lost).toBe(0);
    expect(out.wrote).toBe(6);
    expect(new TextDecoder().decode(written[0])).toBe("456789");
    expect(out.writtenThrough).toBe(110);
  });

  it("reports the unrecoverable window when the ring has overrun it", async () => {
    const { backend, written } = scriptedBackend([
      { bytes: bytes("abc"), startOffset: 200, endOffset: 203 },
    ]);
    const out = await resyncOnce(backend, "pane-2", 150);
    expect(out.lost).toBe(50);
    expect(new TextDecoder().decode(written[0])).toBe("abc");
  });

  it("treats a null window as 'no ring', writing nothing", async () => {
    const { backend, written } = scriptedBackend([null]);
    const out = await resyncOnce(backend, "pane-3", 10);
    expect(out).toEqual({ wrote: 0, lost: 0, writtenThrough: 10 });
    expect(written).toEqual([]);
  });
});
