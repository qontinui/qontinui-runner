/**
 * `shellIntegrationEvents`: the OSC 633 parse a pane's `TerminalInstance` runs,
 * the replay summary a remount re-derives its marks from, and the rule that
 * output this window loses — a chunk dropped while hidden, a ring overrun, a
 * spliced resync, an unmount — reports `unobserved`, so the page's
 * `useShellIntegration` forgets the pane's prompt-state marks.
 *
 * `TerminalInstance` cannot be imported under `environment: "node"` (it pulls
 * `@xterm/addon-canvas`), so its wiring onto the sink is pinned by scanning its
 * source, the `paneOutputStream.test.ts` precedent.
 */

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import {
  COMMAND_MARKS_CAPABILITY,
  parseOsc633,
  shellMarksSink,
  type ShellIntegrationEvent,
} from "./shellIntegrationEvents";

describe("parseOsc633", () => {
  it("reads the prompt, command and capability marks", () => {
    expect(parseOsc633("A")).toEqual({ type: "prompt_start" });
    expect(parseOsc633("B")).toEqual({ type: "command_ready" });
    expect(parseOsc633("C")).toEqual({ type: "command_execute" });
    expect(parseOsc633("D;2")).toEqual({ type: "command_done", exitCode: 2 });
    expect(parseOsc633("E;echo a\\x3bb")).toEqual({ type: "command_line", command: "echo a;b" });
    expect(parseOsc633("P;Cwd=/repo")).toEqual({ type: "cwd", path: "/repo" });
    expect(parseOsc633(COMMAND_MARKS_CAPABILITY)).toEqual({ type: "command_marks" });
    expect(COMMAND_MARKS_CAPABILITY).toBe("P;CommandMarks=1");
  });

  it("reads nothing it does not know", () => {
    expect(parseOsc633("P;CommandMarks=0")).toBeNull();
    expect(parseOsc633("Z")).toBeNull();
  });
});

/**
 * A pane: its sink over a fake backend whose write queue parses only when
 * `parse()` runs, the way xterm parses asynchronously.
 */
function pane() {
  const events: ShellIntegrationEvent[] = [];
  let handler: ((e: ShellIntegrationEvent) => void) | undefined = (e) => events.push(e);
  const queue: Array<{ marks: string[]; cb?: () => void }> = [];
  const sink = shellMarksSink(
    () => handler,
    (cb) => queue.push({ marks: [], cb }),
  );
  return {
    sink,
    events,
    /** Hand the backend a chunk carrying these OSC 633 payloads (not parsed yet). */
    write: (...marks: string[]) => queue.push({ marks }),
    /** Let the backend parse everything queued, in order. */
    parse: () => {
      while (queue.length > 0) {
        const w = queue.shift()!;
        for (const m of w.marks) sink.osc633(m);
        w.cb?.();
      }
    },
    detach: () => (handler = undefined),
  };
}

describe("shellMarksSink — live", () => {
  it("forwards each parsed OSC 633 sequence to the pane's current handler", () => {
    const p = pane();
    p.sink.osc633(COMMAND_MARKS_CAPABILITY);
    p.sink.osc633("A");
    p.sink.osc633("Z");
    expect(p.events).toEqual([{ type: "command_marks" }, { type: "prompt_start" }]);
  });

  it("reports the pane's unmount as `unobserved`", () => {
    const p = pane();
    p.sink.unmounted();
    expect(p.events).toEqual([{ type: "unobserved" }]);
  });

  it("drops events for a pane with no handler", () => {
    const p = pane();
    p.detach();
    p.sink.osc633("A");
    p.sink.outputLost();
    p.parse();
    expect(p.events).toEqual([]);
  });
});

describe("shellMarksSink — lost output", () => {
  it("reports a loss at once and again behind the writes the backend still has to parse", () => {
    const p = pane();
    // A chunk ending at a prompt was handed to xterm, then the next chunk was
    // dropped (hidden): the queued prompt is parsed AFTER the first clear.
    p.write("A");
    p.sink.outputLost();
    expect(p.events).toEqual([{ type: "unobserved" }]);
    p.parse();
    expect(p.events).toEqual([
      { type: "unobserved" },
      { type: "prompt_start" },
      { type: "unobserved" },
    ]);
  });
});

describe("shellMarksSink — a mount's ring replay", () => {
  it("summarises a replay that ends at a prompt carrying the capability, and reports nothing else", () => {
    const p = pane();
    p.sink.beginReplay();
    p.write("E;make", "C", "D;0", "P;Cwd=/repo", COMMAND_MARKS_CAPABILITY, "A", "B");
    p.parse();
    expect(p.events).toEqual([]);
    p.sink.endReplay();
    expect(p.events).toEqual([
      { type: "replayed_marks", mark: "prompt_start", announcesCommandStarts: true },
    ]);
    // Live from here: sequences are reported again.
    p.sink.osc633("C");
    expect(p.events.at(-1)).toEqual({ type: "command_execute" });
  });

  it("summarises a replay that ends mid-command as busy", () => {
    const p = pane();
    p.sink.beginReplay();
    p.write(COMMAND_MARKS_CAPABILITY, "A", "E;Get-Content -Wait log", "C");
    p.parse();
    p.sink.endReplay();
    expect(p.events).toEqual([
      { type: "replayed_marks", mark: "command_execute", announcesCommandStarts: false },
    ]);
  });

  it("judges the capability by the LAST prompt of the replay", () => {
    const p = pane();
    p.sink.beginReplay();
    // A covered prompt, a command, then a nested prompt that carries no capability.
    p.write(COMMAND_MARKS_CAPABILITY, "A", "C", "A");
    p.parse();
    p.sink.endReplay();
    expect(p.events).toEqual([
      { type: "replayed_marks", mark: "prompt_start", announcesCommandStarts: false },
    ]);
  });

  it("summarises a replay with no marks as no reading", () => {
    const p = pane();
    p.sink.beginReplay();
    p.write();
    p.parse();
    p.sink.endReplay();
    expect(p.events).toEqual([
      { type: "replayed_marks", mark: undefined, announcesCommandStarts: false },
    ]);
  });
});

describe("TerminalInstance wiring onto the sink (source scan)", () => {
  const source = readFileSync(resolve(__dirname, "TerminalInstance.tsx"), "utf8");

  it("builds one sink per mount over the live onShellIntegration and the backend's write queue", () => {
    expect(source).toContain("shellMarksSink(\n        () => onShellIntegrationRef.current,");
    expect(source).toContain("if (b) b.write(new Uint8Array(0), cb);");
  });

  it("listens for OSC 633 in replay mode BEFORE the mount's ring replay is written", () => {
    const listen = source.indexOf("backend.onOsc633((data) => shellMarks.osc633(data))");
    const begin = source.indexOf("shellMarks.beginReplay()");
    const replay = source.indexOf("backend.write(ring.bytes)");
    expect(listen).toBeGreaterThan(-1);
    expect(begin).toBeGreaterThan(listen);
    expect(replay).toBeGreaterThan(begin);
    // Exactly one registration: the live stream is parsed by the same handler.
    expect(source.split("backend.onOsc633(").length - 1).toBe(1);
  });

  it("ends the replay behind the drained chunks, from a write callback", () => {
    const drain = source.indexOf("stream.drainPending()");
    const end = source.indexOf("backend.write(new Uint8Array(0), () => shellMarks.endReplay())");
    expect(drain).toBeGreaterThan(-1);
    expect(end).toBeGreaterThan(drain);
  });

  it("reports a dropped chunk from the output stream's missed-output hook", () => {
    const hook = source.slice(source.indexOf("noteMissedOutput: (end) =>"));
    const body = hook.slice(0, hook.indexOf("requestResync:"));
    expect(body).toContain("paneService.noteMissedOutput(end)");
    expect(body).toContain("shellMarks.outputLost()");
  });

  it("reports a resync's lost output through the ring-resync host", () => {
    expect(source).toContain("outputLost: () => shellMarks.outputLost(),");
  });

  it("reports the unmount from the mount effect's cleanup", () => {
    const cleanup = source.slice(source.indexOf("window.removeEventListener(REMOTE_HISTORY_EVENT"));
    const body = cleanup.slice(0, cleanup.indexOf("paneService.dispose()"));
    expect(body).toContain("shellMarks.unmounted()");
  });
});
